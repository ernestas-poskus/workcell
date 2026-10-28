//! One child process: spawn, capture, timeout, stall watchdog, and the two-stage kill.
//!
//! The child runs in its own process group with stdin on `/dev/null` and exactly the environment
//! the caller supplies. Its stdout and stderr are pumped into both a full log file and a bounded
//! in-memory capture, and a stall watchdog measures silence by bytes received rather than by the
//! child's own liveness.
//!
//! # Timeout is SIGTERM, then SIGKILL
//!
//! A coding agent is not a stateless RPC: it may be holding a Git lock, a partial edit, or a child
//! of its own. SIGKILL first would leave the workspace in a state the next run has to repair, so a
//! timed-out, stalled, or shutting-down run gets `SIGTERM` to its whole process group, a short
//! grace period ([`KILL_GRACE`]), and then `SIGKILL`. The grace period is a constant rather than a
//! knob: a process that ignores `SIGTERM` for five seconds gets `SIGKILL` on every Unix.
//!
//! # Drop is not cancellation
//!
//! A `run` future that is dropped — a panic in the caller, a runtime shutdown — does not by itself
//! kill the child. The caller owns the future and always awaits it; this module also sets
//! `kill_on_drop(true)`, so the process exits with the runtime even if the future is lost. The log
//! stays behind either way.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::error::{Error, Result};

/// The most log bytes one run may append, across stdout and stderr together.
///
/// The in-memory capture is already bounded, but the log was append-only and unbounded: a child
/// that prints forever could fill the disk. Past this cap the pumps stop writing and append one
/// marker, so the file stays diagnosable without becoming a denial of service.
pub const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

/// The longest the in-memory capture keeps, per stream; the log file keeps more, up to
/// [`MAX_LOG_BYTES`].
pub const CAPTURE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The SIGTERM grace period before SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// One process to run.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    /// The argv, already substituted.
    pub argv: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// The environment, already decided by the caller.
    pub env: BTreeMap<String, String>,
    /// Where stdout and stderr are appended in full.
    pub log_path: PathBuf,
}

/// How one child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The exit code, when the process exited normally.
    pub exit_code: Option<i32>,
    /// Whether the timeout fired.
    pub timed_out: bool,
    /// Whether the child stopped producing bytes for the configured window.
    pub stalled: bool,
    /// Whether the process was interrupted by shutdown.
    pub interrupted: bool,
    /// The captured stdout (bounded).
    pub stdout: String,
    /// The captured stderr (bounded).
    pub stderr: String,
    /// The log file the full streams were written to.
    pub log_path: PathBuf,
    /// Wall-clock duration.
    pub duration: Duration,
}

impl Outcome {
    /// Whether the process exited zero and was not interrupted.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && !self.stalled && !self.interrupted && self.exit_code == Some(0)
    }
}

/// Run one child to completion, its timeout, or shutdown, with an optional silence deadline.
///
/// The child gets its own process group; a timeout or shutdown signals the whole group with
/// `SIGTERM`, waits [`KILL_GRACE`], then signals `SIGKILL`. `on_spawn` receives the child's pid as
/// soon as it exists, so the caller can record it.
///
/// # Errors
///
/// Returns [`Error::Process`] when the argv is empty, the program cannot be started, the log file
/// cannot be created, or waiting for the child fails.
pub async fn run_with_stall(
    spec: ProcessSpec,
    timeout: Duration,
    stall_timeout: Duration,
    shutdown: &mut watch::Receiver<bool>,
    on_spawn: Option<&(dyn Fn(u32) + Send + Sync)>,
) -> Result<Outcome> {
    run_with_stall_on(
        spec,
        timeout,
        stall_timeout,
        shutdown,
        on_spawn,
        Arc::new(TokioStallClock),
    )
    .await
}

/// Run one child to completion or its timeout, without a stall watchdog or shutdown channel.
///
/// # Errors
///
/// Returns [`Error::Process`] when the argv is empty, the program cannot be started, the log file
/// cannot be created, or waiting for the child fails.
pub async fn run(spec: ProcessSpec, timeout: Duration) -> Result<Outcome> {
    // Keep the sender alive: a dropped sender is a shutdown request.
    let (_sender, mut shutdown) = watch::channel(false);
    run_with_stall(spec, timeout, Duration::ZERO, &mut shutdown, None).await
}

/// Time source for the stall watchdog's silence measurement.
trait StallClock: Send + Sync {
    fn now(&self) -> tokio::time::Instant;
    fn sleep_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>;
}

/// The real, tokio-backed clock.
struct TokioStallClock;

impl StallClock for TokioStallClock {
    fn now(&self) -> tokio::time::Instant {
        tokio::time::Instant::now()
    }

    fn sleep_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep_until(deadline))
    }
}

/// Run with an injected clock for stall detection.
async fn run_with_stall_on(
    spec: ProcessSpec,
    timeout: Duration,
    stall_timeout: Duration,
    shutdown: &mut watch::Receiver<bool>,
    on_spawn: Option<&(dyn Fn(u32) + Send + Sync)>,
    clock: Arc<dyn StallClock>,
) -> Result<Outcome> {
    let started = Instant::now();
    if spec.argv.is_empty() {
        return Err(Error::Process {
            detail: "the argv is empty; there is no process to start".to_owned(),
        });
    }
    if let Some(parent) = spec.log_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Error::io(&parent.display().to_string(), error))?;
    }
    // Append, never truncate: a run's log path is unique per attempt, but a review reuses one
    // audit log across invocations (the operator's record must not lose the previous verdict).
    let log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)
        .await
        .map_err(|error| Error::io(&spec.log_path.display().to_string(), error))?;
    let log = Arc::new(Mutex::new(log));
    let log_written = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so the two-stage kill reaches anything the child started *and left in
    // the group*. A coding agent routinely spawns a shell, a formatter, or a `git` child;
    // signalling only the direct child leaves those holding the pipes and the run appears to hang
    // until they exit. A grandchild that calls `setsid` creates a new session and escapes the group
    // kill (measured 2026-10-28): it stays inside the sandbox, but it can outlive the run.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command.spawn().map_err(|error| Error::Process {
        detail: format!(
            "could not start `{}` in {}: {error}",
            spec.argv[0],
            spec.cwd.display()
        ),
    })?;
    if let (Some(on_spawn), Some(pid)) = (on_spawn, child.id()) {
        on_spawn(pid);
    }
    // The group leader's pid is captured now: after a normal exit `child.id()` is gone, but a
    // background child the agent left behind still belongs to this group and must not outlive the
    // run (it could rewrite the checkout after the caller's guard ran). A `setsid` grandchild is
    // the exception this cannot reach; see the module note.
    let group_pid = child.id();

    let (activity, mut last_output) = watch::channel(clock.now());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_pump = stdout.map(|stream| {
        let log = Arc::clone(&log);
        let clock = Arc::clone(&clock);
        let log_written = Arc::clone(&log_written);
        tokio::spawn(pump(stream, log, activity.clone(), clock, log_written))
    });
    let stderr_pump = stderr.map(|stream| {
        let log = Arc::clone(&log);
        let clock = Arc::clone(&clock);
        let log_written = Arc::clone(&log_written);
        tokio::spawn(pump(stream, log, activity.clone(), clock, log_written))
    });

    let finish = tokio::select! {
        status = child.wait() => Finish::Exited(status),
        () = tokio::time::sleep(timeout) => Finish::TimedOut,
        () = shutdown_requested(shutdown) => Finish::Shutdown,
        () = stalled(&*clock, &mut last_output, stall_timeout) => Finish::Stalled,
    };

    let is_stalled = matches!(finish, Finish::Stalled);
    let (exit_code, timed_out, interrupted) = match finish {
        Finish::Exited(status) => {
            let status = status.map_err(|error| Error::Process {
                detail: format!("waiting for `{}` failed: {error}", spec.argv[0]),
            })?;
            // A normal exit is not proof the run is over: `agent &` or a shell that backgrounds a
            // helper exits 0 while the group keeps running inside the sandbox. Reap the group
            // before returning, so nothing the caller guards afterwards can be rewritten by a
            // leftover process.
            #[cfg(unix)]
            if let Some(pid) = group_pid
                && group_alive(pid).await
            {
                terminate(&mut child, &spec.argv[0], Some(pid)).await;
            }
            (status.code(), false, false)
        }
        Finish::TimedOut => {
            terminate(&mut child, &spec.argv[0], group_pid).await;
            (None, true, false)
        }
        Finish::Stalled => {
            terminate(&mut child, &spec.argv[0], group_pid).await;
            (None, false, false)
        }
        Finish::Shutdown => {
            terminate(&mut child, &spec.argv[0], group_pid).await;
            (None, false, true)
        }
    };
    let stdout = collect(stdout_pump).await;
    let stderr = collect(stderr_pump).await;
    if let Ok(mut file) = log.try_lock() {
        let _ = file.flush().await;
    }

    Ok(Outcome {
        exit_code,
        timed_out,
        stalled: is_stalled,
        interrupted,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        log_path: spec.log_path,
        duration: started.elapsed(),
    })
}

/// Which branch ended the wait.
enum Finish {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Stalled,
    Shutdown,
}

/// EOF does not count as activity: a child that closes both streams can still stall.
async fn stalled(
    clock: &dyn StallClock,
    activity: &mut watch::Receiver<tokio::time::Instant>,
    timeout: Duration,
) {
    if timeout.is_zero() {
        std::future::pending::<()>().await;
    }
    loop {
        let Some(deadline) = activity.borrow_and_update().checked_add(timeout) else {
            // An unrepresentable window cannot expire; the absolute timeout still applies.
            std::future::pending::<()>().await;
            return;
        };
        tokio::select! {
            biased;
            changed = activity.changed() => {
                if changed.is_err() {
                    clock.sleep_until(deadline).await;
                    return;
                }
            }
            () = clock.sleep_until(deadline) => return,
        }
    }
}

/// Pump one stream into the log and a bounded memory buffer.
async fn pump(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    log: Arc<Mutex<tokio::fs::File>>,
    activity: watch::Sender<tokio::time::Instant>,
    clock: Arc<dyn StallClock>,
    log_written: Arc<std::sync::atomic::AtomicU64>,
) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        activity.send_replace(clock.now());
        {
            let prior = log_written.fetch_add(read as u64, std::sync::atomic::Ordering::Relaxed);
            if prior < MAX_LOG_BYTES {
                let take = (read as u64).min(MAX_LOG_BYTES - prior) as usize;
                let mut file = log.lock().await;
                let _ = file.write_all(&buffer[..take]).await;
                if prior + take as u64 >= MAX_LOG_BYTES {
                    let _ = file
                        .write_all(b"\n[workcell] log truncated at its size cap\n")
                        .await;
                }
            }
        }
        if captured.len() < CAPTURE_MAX_BYTES {
            let room = CAPTURE_MAX_BYTES - captured.len();
            captured.extend_from_slice(&buffer[..read.min(room)]);
        }
    }
    captured
}

/// Wait for a pump to finish, but not forever.
///
/// A pump ends when its pipe's write end closes. After a timeout kill that holds unless every
/// process holding the pipe died with the group; a grandchild that escaped the group (a
/// daemonising tool) would keep it open, so the wait is bounded and the task abandoned rather
/// than allowed to hold the run open past its own deadline.
async fn collect(handle: Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    match tokio::time::timeout(Duration::from_secs(2), handle).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => Vec::new(),
        Err(_) => Vec::new(),
    }
}

/// Whether the process group led by `pid` still has a live member.
#[cfg(unix)]
async fn group_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", "--", &format!("-{pid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// SIGTERM, then SIGKILL after the grace period.
#[cfg(unix)]
async fn terminate(child: &mut Child, program: &str, group: Option<u32>) {
    let pid = group;
    if let Some(pid) = pid {
        let group = format!("-{pid}");
        let signalled = Command::new("/bin/kill")
            .args(["-TERM", "--", &group])
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success());
        if !signalled {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }
    }
    // Even if the leader exits on TERM, descendants may ignore it and hold the pipes.
    let grace = tokio::time::sleep(KILL_GRACE);
    tokio::pin!(grace);
    let exited = tokio::select! {
        result = child.wait() => result.is_ok(),
        () = &mut grace => false,
    };
    if exited {
        // A surviving group needs the remainder of the grace period.
        if let Some(pid) = pid {
            let group = format!("-{pid}");
            let alive = Command::new("/bin/kill")
                .args(["-0", "--", &group])
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_ok_and(|status| status.success());
            if !alive {
                return;
            }
        }
        grace.await;
    }
    if let Some(pid) = pid {
        let group = format!("-{pid}");
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &group])
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
    eprintln!("workcell: `{program}` group did not exit on SIGTERM; sent SIGKILL");
}

/// Without Unix process groups there is only the direct child to stop.
#[cfg(not(unix))]
async fn terminate(child: &mut Child, _program: &str, _group: Option<u32>) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Resolve when shutdown is requested, or when the sender is gone.
async fn shutdown_requested(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn spec(argv: &[&str], dir: &std::path::Path) -> ProcessSpec {
        ProcessSpec {
            argv: argv.iter().map(|argument| (*argument).to_owned()).collect(),
            cwd: dir.to_path_buf(),
            env: BTreeMap::new(),
            log_path: dir.join("run.log"),
        }
    }

    fn channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    struct ManualStallClock {
        sender: watch::Sender<tokio::time::Instant>,
    }

    impl ManualStallClock {
        fn new() -> Arc<Self> {
            let (sender, _) = watch::channel(tokio::time::Instant::now());
            Arc::new(Self { sender })
        }

        fn advance(&self, by: Duration) {
            let current = *self.sender.borrow();
            let next = current.checked_add(by).expect("valid deadline");
            self.sender.send_replace(next);
        }
    }

    impl StallClock for ManualStallClock {
        fn now(&self) -> tokio::time::Instant {
            *self.sender.borrow()
        }

        fn sleep_until(
            &self,
            deadline: tokio::time::Instant,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let mut rx = self.sender.subscribe();
            Box::pin(async move {
                loop {
                    if *rx.borrow_and_update() >= deadline {
                        return;
                    }
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            })
        }
    }

    #[tokio::test]
    async fn silence_window_fires_only_after_the_deadline() {
        let clock = ManualStallClock::new();
        let (_activity, mut last_output) = watch::channel(clock.now());
        let window = Duration::from_secs(5);
        let mut stalled_fut = Box::pin(stalled(&*clock, &mut last_output, window));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err(),
            "stalled should not fire before deadline"
        );
        clock.advance(Duration::from_secs(4));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err(),
            "stalled should not fire before full window elapses"
        );
        clock.advance(Duration::from_secs(2));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut stalled_fut)
                .await
                .is_ok(),
            "stalled should fire when clock reaches the deadline"
        );
    }

    #[tokio::test]
    async fn silence_window_restarts_on_each_activity() {
        let clock = ManualStallClock::new();
        let (activity, mut last_output) = watch::channel(clock.now());
        let window = Duration::from_secs(5);
        let mut stalled_fut = Box::pin(stalled(&*clock, &mut last_output, window));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err()
        );
        clock.advance(Duration::from_secs(3));
        activity.send_replace(clock.now());
        clock.advance(Duration::from_secs(3));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err(),
            "activity must reset the silence window"
        );
        clock.advance(Duration::from_secs(2));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut stalled_fut)
                .await
                .is_ok(),
            "watchdog should fire after the full window since last activity"
        );
    }

    #[tokio::test]
    async fn silence_window_fires_when_the_activity_sender_is_dropped() {
        let clock = ManualStallClock::new();
        let (activity, mut last_output) = watch::channel(clock.now());
        let window = Duration::from_secs(5);
        let mut stalled_fut = Box::pin(stalled(&*clock, &mut last_output, window));

        // Dropping the sender must not fire immediately
        drop(activity);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err(),
            "dropping activity sender must not fire instantly"
        );

        clock.advance(Duration::from_secs(4));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut stalled_fut)
                .await
                .is_err(),
            "dropping activity sender must still wait out the remaining window"
        );

        clock.advance(Duration::from_secs(2));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut stalled_fut)
                .await
                .is_ok(),
            "watchdog should fire once the window elapses after drop"
        );
    }

    #[tokio::test]
    async fn a_stalled_group_is_terminated_then_killed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_sender, mut shutdown) = channel();
        let clock = ManualStallClock::new();

        let started_marker = dir.path().join("started");
        let term_witness = dir.path().join("term-witness");
        let log_path = dir.path().join("run.log");

        let script = "trap 'touch term-witness' TERM; touch started; while :; do sleep 1; done";
        let process = ProcessSpec {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: dir.path().to_path_buf(),
            env: BTreeMap::new(),
            log_path: log_path.clone(),
        };

        let stall_timeout = Duration::from_secs(2);
        let total_timeout = Duration::from_secs(30);
        let clock_clone = Arc::clone(&clock);

        let handle = tokio::spawn(async move {
            run_with_stall_on(
                process,
                total_timeout,
                stall_timeout,
                &mut shutdown,
                None,
                clock_clone,
            )
            .await
        });

        // (a) Bounded wait for the child to write `started`.
        let wait_started = async {
            while !started_marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(5), wait_started)
            .await
            .is_err()
        {
            let log_tail = std::fs::read_to_string(&log_path).unwrap_or_default();
            panic!("child script failed to write `started` within 5s; log tail:\n{log_tail}");
        }

        // Advance the manual clock past the stall timeout.
        let advanced_at = Instant::now();
        clock.advance(stall_timeout + Duration::from_secs(1));

        // (b) Wait for the child's TERM trap to write `term-witness`.
        let wait_witness = async {
            while !term_witness.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(5), wait_witness)
            .await
            .is_err()
        {
            let log_tail = std::fs::read_to_string(&log_path).unwrap_or_default();
            panic!(
                "child did not write `term-witness` within 5s after clock advance; log tail:\n{log_tail}"
            );
        }
        let witness_seen = Instant::now();

        // (c) Await completion, which happens after KILL_GRACE.
        let outcome = tokio::time::timeout(Duration::from_secs(15), handle)
            .await
            .expect("execution must complete within 15s")
            .expect("task join failed")
            .expect("runs");

        let elapsed_since_witness = witness_seen.elapsed();
        let elapsed_since_advanced = advanced_at.elapsed();
        assert!(
            elapsed_since_advanced >= KILL_GRACE,
            "kill must land at least KILL_GRACE ({KILL_GRACE:?}) after stall was triggered, took {elapsed_since_advanced:?}"
        );
        assert!(
            elapsed_since_witness + Duration::from_millis(250) >= KILL_GRACE,
            "child exited {elapsed_since_witness:?} after term witness, expected at least KILL_GRACE ({KILL_GRACE:?})"
        );

        // (d) The returned outcome reports the stalled branch.
        assert!(outcome.stalled, "{outcome:?}");
        assert!(!outcome.succeeded(), "{outcome:?}");
        assert!(!outcome.timed_out, "{outcome:?}");
        assert!(outcome.exit_code.is_none(), "{outcome:?}");
    }

    #[tokio::test]
    async fn an_empty_argv_is_refused_before_a_log_is_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_sender, mut shutdown) = channel();
        let error = run_with_stall(
            spec(&[], dir.path()),
            Duration::from_secs(1),
            Duration::ZERO,
            &mut shutdown,
            None,
        )
        .await
        .expect_err("nothing to start");
        assert!(matches!(error, Error::Process { .. }), "{error}");
        assert!(!dir.path().join("run.log").exists());
    }

    #[tokio::test]
    async fn the_log_is_capped_and_marked_when_a_child_prints_forever() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_sender, mut shutdown) = channel();
        let clock = ManualStallClock::new();
        let log_path = dir.path().join("run.log");
        // One megabyte past the cap.
        let bytes = MAX_LOG_BYTES + 1024 * 1024;
        let process = ProcessSpec {
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                format!("head -c {bytes} /dev/zero"),
            ],
            cwd: dir.path().to_path_buf(),
            env: BTreeMap::new(),
            log_path: log_path.clone(),
        };

        let outcome = run_with_stall_on(
            process,
            Duration::from_secs(60),
            Duration::from_secs(10),
            &mut shutdown,
            None,
            clock,
        )
        .await
        .expect("run");
        assert!(outcome.succeeded());

        let length = std::fs::metadata(&log_path).expect("log").len();
        assert!(
            length > 1024 * 1024,
            "the start of the log is kept: {length}"
        );
        assert!(
            length <= MAX_LOG_BYTES + 128,
            "the log is capped at MAX_LOG_BYTES: {length}"
        );
        let contents = std::fs::read(&log_path).expect("read log");
        let tail = &contents[contents.len().saturating_sub(128)..];
        assert!(
            String::from_utf8_lossy(tail).contains("log truncated"),
            "the cap is marked so a reader knows why the log ends"
        );
    }
}

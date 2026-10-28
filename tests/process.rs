//! The child-process contract: exit codes, capture, log file, timeout, stall watchdog, and the
//! two-stage process-group kill.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::watch;
use workcell::error::Error;
use workcell::process::{self, CAPTURE_MAX_BYTES, KILL_GRACE, ProcessSpec};

fn env(path: bool) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if path {
        env.insert(
            "PATH".to_owned(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
        );
    }
    env
}

fn spec(argv: &[&str], dir: &Path, log: PathBuf) -> ProcessSpec {
    ProcessSpec {
        argv: argv.iter().map(|argument| (*argument).to_owned()).collect(),
        cwd: dir.to_path_buf(),
        env: env(true),
        log_path: log,
    }
}

#[tokio::test]
async fn a_zero_exit_captures_both_streams_and_the_log_keeps_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("nested/run.log");
    let outcome = process::run(
        spec(
            &["/bin/sh", "-c", "printf 'out'; printf 'err' >&2"],
            dir.path(),
            log.clone(),
        ),
        Duration::from_secs(10),
    )
    .await
    .expect("runs");
    assert!(outcome.succeeded(), "{outcome:?}");
    assert_eq!(outcome.exit_code, Some(0));
    assert!(!outcome.timed_out && !outcome.stalled && !outcome.interrupted);
    assert_eq!(outcome.stdout, "out");
    assert_eq!(outcome.stderr, "err");
    assert_eq!(outcome.log_path, log);
    let written = std::fs::read_to_string(&log).expect("log");
    assert!(
        written.contains("out") && written.contains("err"),
        "{written}"
    );
}

#[tokio::test]
async fn a_non_zero_exit_is_reported_with_its_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = process::run(
        spec(
            &["/bin/sh", "-c", "exit 7"],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(10),
    )
    .await
    .expect("runs");
    assert_eq!(outcome.exit_code, Some(7));
    assert!(!outcome.succeeded());
}

#[tokio::test]
async fn only_the_supplied_environment_reaches_the_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child_env = BTreeMap::new();
    child_env.insert("PROBE".to_owned(), "value".to_owned());
    child_env.insert(
        "PATH".to_owned(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
    );
    let outcome = process::run(
        ProcessSpec {
            argv: vec!["/usr/bin/env".to_owned()],
            cwd: dir.path().to_path_buf(),
            env: child_env,
            log_path: dir.path().join("run.log"),
        },
        Duration::from_secs(10),
    )
    .await
    .expect("runs");
    assert!(outcome.succeeded(), "{outcome:?}");
    let mut lines: Vec<&str> = outcome.stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines.iter().any(|line| line.starts_with("PROBE=value")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("PATH=")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_timeout_signals_the_whole_group_and_the_descendants_do_not_survive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let started = std::time::Instant::now();
    let outcome = process::run(
        spec(
            &[
                "/bin/sh",
                "-c",
                "trap '' TERM; (sleep 9; touch survived) & printf ready; wait",
            ],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_millis(300),
    )
    .await
    .expect("runs");
    assert!(outcome.timed_out, "{outcome:?}");
    assert!(!outcome.interrupted && !outcome.stalled);
    assert!(outcome.exit_code.is_none());
    assert!(
        started.elapsed() >= KILL_GRACE,
        "a group that ignores SIGTERM must get the full grace period, took {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "the timeout must not wait for the descendant's own sleep"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !dir.path().join("survived").exists(),
        "a descendant survived SIGKILL"
    );
}

#[tokio::test]
async fn a_normal_exit_also_reaps_a_background_descendant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = process::run(
        spec(
            &[
                "/bin/sh",
                "-c",
                // The leader exits 0 immediately; its background child would touch `survived`
                // one second later, inside the sandbox, after the caller's guard ran.
                "(sleep 1; touch survived) & exit 0",
            ],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(10),
    )
    .await
    .expect("runs");
    assert!(
        outcome.succeeded(),
        "the leader's zero exit must be preserved: {outcome:?}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !dir.path().join("survived").exists(),
        "a background descendant outlived a normal exit"
    );
}

#[tokio::test]
async fn a_silent_child_is_reaped_by_the_stall_watchdog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_sender, mut shutdown) = watch::channel(false);
    let started = std::time::Instant::now();
    let outcome = process::run_with_stall(
        spec(
            &["/bin/sh", "-c", "printf ready; sleep 30"],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(20),
        Duration::from_millis(300),
        &mut shutdown,
        None,
    )
    .await
    .expect("runs");
    assert!(outcome.stalled, "{outcome:?}");
    assert!(!outcome.timed_out && !outcome.interrupted);
    assert!(outcome.exit_code.is_none());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the stall watchdog must not wait for the child's own sleep"
    );
}

#[tokio::test]
async fn the_capture_is_bounded_while_the_log_keeps_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("run.log");
    // 80 * 64 KiB = 5 MiB, which is larger than the in-memory capture bound.
    let outcome = process::run(
        spec(
            &[
                "/bin/sh",
                "-c",
                "dd if=/dev/zero bs=65536 count=80 2>/dev/null",
            ],
            dir.path(),
            log.clone(),
        ),
        Duration::from_secs(30),
    )
    .await
    .expect("runs");
    assert!(outcome.succeeded(), "{outcome:?}");
    const WRITTEN: u64 = 80 * 65_536;
    assert_eq!(outcome.stdout.len(), CAPTURE_MAX_BYTES);
    assert_eq!(
        std::fs::metadata(&log).expect("log").len(),
        WRITTEN,
        "the log file must keep the complete stream"
    );
}

#[tokio::test]
async fn a_shutdown_signal_interrupts_a_running_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (sender, mut shutdown) = watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = sender.send(true);
    });
    let outcome = process::run_with_stall(
        spec(
            &["/bin/sh", "-c", "sleep 30"],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(60),
        Duration::ZERO,
        &mut shutdown,
        None,
    )
    .await
    .expect("runs");
    assert!(outcome.interrupted, "{outcome:?}");
    assert!(!outcome.timed_out);
    assert!(outcome.exit_code.is_none());
}

#[tokio::test]
async fn the_spawn_callback_sees_the_child_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_sender, mut shutdown) = watch::channel(false);
    let pid = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&pid);
    let on_spawn = move |value: u32| seen.store(value, Ordering::SeqCst);
    let outcome = process::run_with_stall(
        spec(
            &["/bin/sh", "-c", "exit 0"],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(10),
        Duration::ZERO,
        &mut shutdown,
        Some(&on_spawn),
    )
    .await
    .expect("runs");
    assert!(outcome.succeeded(), "{outcome:?}");
    assert!(pid.load(Ordering::SeqCst) > 0, "the callback saw no pid");
}

#[tokio::test]
async fn an_unstartable_program_is_a_process_error_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = process::run(
        spec(
            &["/definitely/not/a/program"],
            dir.path(),
            dir.path().join("run.log"),
        ),
        Duration::from_secs(1),
    )
    .await
    .expect_err("cannot start");
    assert!(matches!(error, Error::Process { .. }), "{error}");
}

#[tokio::test]
async fn an_empty_argv_is_refused_before_a_log_is_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("run.log");
    let error = process::run(spec(&[], dir.path(), log.clone()), Duration::from_secs(1))
        .await
        .expect_err("nothing to start");
    assert!(matches!(error, Error::Process { .. }), "{error}");
    assert!(
        !log.exists(),
        "an empty argv must be refused before the log is created"
    );
}

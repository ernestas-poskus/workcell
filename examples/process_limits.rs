//! Run a child process with a deadline and bounded capture, and show the group kill.
//!
//! ```sh
//! cargo run --example process_limits
//! ```
//!
//! `process::run` starts the child in its own process group, appends the full streams to a log
//! file, captures at most a bounded prefix in memory, and kills the group on a timeout
//! (`SIGTERM`, a short grace period, then `SIGKILL`). A coding agent routinely starts children of
//! its own, so the group kill is what stops a `command &` from outliving the run.
//!
//! This example demonstrates the process layer on its own.

use std::time::Duration;

use workcell::process::{self, ProcessSpec};
use workcell::types::{AbsolutePath, Argv, Env, LogPath, Timeout};

fn spec(
    argv: [&str; 3],
    cwd: &AbsolutePath,
    log: &LogPath,
) -> Result<ProcessSpec, Box<dyn std::error::Error>> {
    Ok(ProcessSpec {
        argv: Argv::new(argv)?,
        cwd: cwd.clone(),
        env: Env::empty(),
        log_path: log.clone(),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_dir =
        std::env::temp_dir().join(format!("workcell-process-example-{}", std::process::id()));
    tokio::fs::create_dir_all(&log_dir).await?;
    let cwd = AbsolutePath::new(log_dir.clone())?;

    // 1. A child that finishes normally: bounded stdout, and the full stream in the log file.
    let ok = process::run(
        spec(
            ["/bin/sh", "-c", "printf 'hello'; printf 'world' >&2"],
            &cwd,
            &LogPath::new(log_dir.join("ok.log"))?,
        )?,
        Timeout::new(Duration::from_secs(10)),
    )
    .await?;
    println!(
        "normal exit: code={:?} stdout={:?} succeeded={}",
        ok.exit_code,
        ok.stdout.trim(),
        ok.succeeded()
    );

    // 2. A child that never finishes: the timeout fires, the process group is killed, and the
    //    outcome says so instead of pretending the child exited.
    let timed_out = process::run(
        spec(
            ["/bin/sh", "-c", "sleep 60"],
            &cwd,
            &LogPath::new(log_dir.join("timeout.log"))?,
        )?,
        Timeout::new(Duration::from_millis(250)),
    )
    .await?;
    println!(
        "timeout: timed_out={} stalled={} succeeded={} after {:?}",
        timed_out.timed_out,
        timed_out.stalled,
        timed_out.succeeded(),
        timed_out.duration
    );

    // 3. A child that spawns a background grandchild: the group kill reaps the whole tree, so the
    //    run cannot leave a process behind.
    let group = process::run(
        spec(
            ["/bin/sh", "-c", "sleep 60 & echo started"],
            &cwd,
            &LogPath::new(log_dir.join("group.log"))?,
        )?,
        Timeout::new(Duration::from_secs(10)),
    )
    .await?;
    println!(
        "background child: code={:?} stdout={:?} (the group was reaped)",
        group.exit_code,
        group.stdout.trim()
    );

    println!("logs: {}", log_dir.display());
    Ok(())
}

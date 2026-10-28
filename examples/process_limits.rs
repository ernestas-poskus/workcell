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
//! No sandbox is involved here: this example demonstrates the process layer on its own.

use std::collections::BTreeMap;
use std::time::Duration;

use workcell::process::{self, ProcessSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_dir =
        std::env::temp_dir().join(format!("workcell-process-example-{}", std::process::id()));
    tokio::fs::create_dir_all(&log_dir).await?;

    // 1. A child that finishes normally: bounded stdout, and the full stream in the log file.
    let ok = process::run(
        ProcessSpec {
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf 'hello'; printf 'world' >&2".to_owned(),
            ],
            cwd: log_dir.clone(),
            env: BTreeMap::new(),
            log_path: log_dir.join("ok.log"),
        },
        Duration::from_secs(10),
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
        ProcessSpec {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()],
            cwd: log_dir.clone(),
            env: BTreeMap::new(),
            log_path: log_dir.join("timeout.log"),
        },
        Duration::from_millis(250),
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
        ProcessSpec {
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "sleep 60 & echo started".to_owned(),
            ],
            cwd: log_dir.clone(),
            env: BTreeMap::new(),
            log_path: log_dir.join("group.log"),
        },
        Duration::from_secs(10),
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

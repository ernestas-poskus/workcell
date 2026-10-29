//! Run one command inside the always-on OS sandbox.
//!
//! Usage:
//!
//! ```sh
//! cargo run --example coding_agent -- /path/to/checkout -- /bin/echo hello
//! cargo run --example coding_agent -- /path/to/checkout --review /bin/echo review
//! cargo run --example coding_agent -- /path/to/worktree --worktree /bin/echo git-grants
//! cargo run --example coding_agent -- /path/to/checkout --guard /bin/echo guarded
//! cargo run --example coding_agent -- /path/to/checkout --codex-home codex exec ...
//! ```
//!
//! `--review` launches under the read-only review profile (the checkout is readable, not
//! writable); without it the agent profile makes the checkout writable. `--worktree` discovers
//! the linked Git worktree at the checkout and grants the same Git metadata a run gets
//! (objects/refs/logs plus the worktree Git directory). `--guard` snapshots the shared-checkout
//! Git guard before the child and verifies it after, failing the run when the repository
//! configuration, hooks, or identity changed. `--codex-home` creates a fresh CODEX_HOME under
//! the checkout's `.workcell` directory, links the operator's `~/.codex/auth.json` into it, and
//! grants it write access, so a run's codex state is isolated from the shared home.
//!
//! The sandbox binary (`nono`, 0.80.0 or newer) must be on `PATH`, and the policy is enforced
//! with macOS Seatbelt. `preflight` refuses a host that cannot enforce it; this example has no
//! path that runs the command unconfined.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use workcell::git::Git;
use workcell::process::{self, ProcessSpec};
use workcell::sandbox::{Sandbox, SandboxSpec};
use workcell::worktree::Worktree;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    let checkout = PathBuf::from(args.next().unwrap_or_else(|| ".".to_owned()));
    let mut review = false;
    let mut worktree_mode = false;
    let mut guard_mode = false;
    let mut codex_home_mode = false;
    let mut loopback_ports: Vec<u16> = Vec::new();
    let mut listen_ports: Vec<u16> = Vec::new();
    let mut harness_reads: Vec<PathBuf> = Vec::new();
    let mut harness_read_files: Vec<PathBuf> = Vec::new();
    let mut harness_bypass: Vec<PathBuf> = Vec::new();
    while let Some(flag) = args.peek().map(String::as_str) {
        match flag {
            "--review" => review = true,
            "--worktree" => worktree_mode = true,
            "--guard" => guard_mode = true,
            "--codex-home" => codex_home_mode = true,
            // The profile grants no loopback. A harness whose measured behaviour needs an
            // arbitrary-port bind (claude, agy) asks for it here; `0` is the wildcard.
            // The caller names the harness-owned paths its agent needs. Nothing here is a profile
            // grant, so this is also how a test proves one harness cannot read another's config.
            "--read-path" | "--harness-read" => {
                args.next();
                let path = args.peek().ok_or("--read-path needs a path")?;
                harness_reads.push(PathBuf::from(path));
            }
            "--read-file" | "--harness-read-file" => {
                args.next();
                let path = args.peek().ok_or("--read-file needs a path")?;
                harness_read_files.push(PathBuf::from(path));
            }
            "--bypass-protection" => {
                args.next();
                let path = args.peek().ok_or("--bypass-protection needs a path")?;
                harness_bypass.push(PathBuf::from(path));
            }
            // Listen-only: an arbitrary-port bind without the connect half. Measured: agy works
            // with this and claude does not (it needs the connect), so both are available.
            "--listen" => {
                args.next();
                let port: u16 = args
                    .peek()
                    .ok_or("--listen needs a port")?
                    .parse()
                    .map_err(|_| "--listen needs a port number")?;
                listen_ports.push(port);
            }
            "--loopback" => {
                // Consume only the flag here: the loop's trailing `args.next()` consumes the
                // value, so an extra `next()` would eat the first command token.
                args.next();
                let port: u16 = args
                    .peek()
                    .ok_or("--loopback needs a port")?
                    .parse()
                    .map_err(|_| "--loopback needs a port number")?;
                loopback_ports.push(port);
            }
            _ => break,
        }
        args.next();
    }
    let mut command: Vec<String> = args.collect();
    if command.is_empty() {
        command = vec!["/bin/echo".to_owned(), "hello from the sandbox".to_owned()];
    }

    // Resolve the binary and publish the embedded profiles into a caller-owned directory outside
    // the sandbox-writable checkout: a child that could rewrite the published policy could widen
    // the next launch. A caller embedding this harness must make the same choice.
    let state_root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(".workcell-example-sandbox-{}", std::process::id()));
    let sandbox = Sandbox::resolve(&state_root)?;
    sandbox.preflight().await?;

    let git = Git::new(&checkout);
    let worktree = if worktree_mode {
        Some(Worktree::discover(&git).await?)
    } else {
        None
    };
    let guard = if guard_mode {
        Some(Sandbox::guard_snapshot(&git).await?)
    } else {
        None
    };
    // A codex run needs writable state; `--codex-home` gives it a fresh home and links the
    // operator's credential in, exactly as a caller does.
    let codex_home = if codex_home_mode {
        let home = checkout.join(".workcell/codex-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home)?;
        if let Some(auth) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".codex/auth.json"))
            .filter(|auth| auth.is_file())
        {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&auth, home.join("auth.json"))?;
            #[cfg(not(unix))]
            std::fs::copy(&auth, home.join("auth.json"))?;
        }
        Some(home)
    } else {
        None
    };
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: worktree.as_ref(),
        read_paths: harness_reads.iter().map(PathBuf::as_path).collect(),
        read_files: harness_read_files.iter().map(PathBuf::as_path).collect(),
        loopback_ports,
        listen_ports,
        allow_domains: Vec::new(),
        bypass_protection: harness_bypass.iter().map(PathBuf::as_path).collect(),
        writable_paths: codex_home.as_deref().into_iter().collect(),
    };
    let argv = if review {
        sandbox.wrap_review(command, &spec)?
    } else {
        sandbox.wrap_agent(command, &spec)?
    };
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    Sandbox::scrub_env(&mut env);
    if let Some(home) = &codex_home {
        env.insert("CODEX_HOME".to_owned(), home.display().to_string());
    }

    let outcome = process::run(
        ProcessSpec {
            argv,
            cwd: checkout.clone(),
            env,
            log_path: checkout.join(".workcell/run.log"),
        },
        Duration::from_secs(600),
    )
    .await?;
    println!("exit: {:?}", outcome.exit_code);
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    if let Some(expected) = guard {
        match Sandbox::guard_verify(&git, &expected).await {
            Ok(()) => println!("guard: ok"),
            Err(error) => {
                eprintln!("guard: {error}");
                std::process::exit(3);
            }
        }
    }
    if !outcome.succeeded() {
        std::process::exit(1);
    }
    Ok(())
}

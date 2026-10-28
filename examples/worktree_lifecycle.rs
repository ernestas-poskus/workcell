//! Cut one isolated worktree per unit of work, use it, then collect it.
//!
//! The example builds a throwaway repository with a bare origin, so it needs only `git` on
//! `PATH` and never touches a real checkout:
//!
//! ```sh
//! cargo run --example worktree_lifecycle
//! ```
//!
//! `RepoSpec::prepare` fetches the base, cuts the worktree, and creates its branch. `Git` reads
//! the worktree in-process (no Git child process, no repository hooks). `gc` reports what a
//! collection would remove, and `RepoSpec::remove` cleans the worktree and branch up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use workcell::gc::{self, GcPolicy};
use workcell::git::Git;
use workcell::worktree::RepoSpec;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A throwaway checkout whose `main` is on a bare origin, with `.worktrees/` ignored.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let remote = dir.path().join("remote.git");
    std::fs::create_dir_all(&root).expect("checkout");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "example"]);
    git(&root, &["config", "user.email", "example@invalid"]);
    std::fs::write(root.join(".gitignore"), ".worktrees/\n").expect("gitignore");
    git(&root, &["add", ".gitignore"]);
    git(&root, &["commit", "-qm", "initial"]);
    let status = Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .expect("git init --bare");
    assert!(status.success());
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().expect("utf-8")],
    );
    git(&root, &["push", "-q", "-u", "origin", "main"]);
    (dir, root)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (_temp, root) = fixture();
    let spec = RepoSpec {
        root: root.clone(),
        remote: "origin".to_owned(),
        base_branch: "main".to_owned(),
        worktree_root: PathBuf::from(".worktrees"),
        branch_prefix: "agent".to_owned(),
        require_clean_root: true,
        env: std::env::vars().collect::<BTreeMap<_, _>>(),
    };

    // The names are deterministic, so a caller can log them before the work starts.
    let (branch, path) = spec.names("demo run");
    println!("will cut {branch} at {}", path.display());

    let tree = spec.prepare("demo run").await?;
    println!(
        "cut {} at {} from {}",
        tree.branch,
        tree.path.display(),
        &tree.base_sha[..12]
    );

    // Read the worktree through the in-process Git runner: no Git child process, no hooks.
    std::fs::write(tree.path.join("NOTES.md"), "agent notes\n")?;
    let git = Git::new(&tree.path).with_env(spec.env.clone());
    let changed = git.status().await?;
    println!(
        "changed paths: {}",
        changed
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let diff = git.diff(None, 64 * 1024).await?;
    println!(
        "{} changed file(s), {} byte(s) of patch",
        diff.changed_files.len(),
        diff.patch.len()
    );

    // `gc` is conservative: it reports what it would remove and why it skipped the rest.
    let report = gc::gc(
        &spec,
        &GcPolicy {
            dry_run: true,
            ..GcPolicy::default()
        },
        &[],
    )
    .await?;
    print!("{}", report.render(true));

    spec.remove(&tree).await?;
    println!("removed {}", tree.path.display());
    Ok(())
}

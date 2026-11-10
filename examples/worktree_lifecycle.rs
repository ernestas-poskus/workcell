//! Cut one isolated worktree per unit of work, use it, then collect it.
//!
//! The example builds a throwaway repository with a bare origin through the shared in-process
//! `git2` fixture, so it needs no `git` binary on `PATH` and never touches a real checkout:
//!
//! ```sh
//! cargo run --example worktree_lifecycle
//! ```
//!
//! `RepoSpec::prepare` fetches the base, cuts the worktree, and creates its branch. `Git` reads
//! the worktree in-process (no Git child process, no repository hooks). `gc` reports what a
//! collection would remove, and `RepoSpec::remove` cleans the worktree and branch up.

#[path = "../tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use workcell::gc::{self, GcPolicy};
use workcell::git::Git;
use workcell::types::{BranchName, BranchPrefix, ByteLimit, RemoteName, RepoRoot, WorktreeRoot};
use workcell::worktree::RepoSpec;

/// A throwaway checkout whose `main` is on a bare origin, with `.worktrees/` ignored.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let remote = dir.path().join("remote.git");
    support::init(&root);
    support::configure_user(&root, "example", "example@invalid");
    support::commit_file(&root, ".gitignore", ".worktrees/\n", "initial");
    support::init_bare(&remote);
    support::add_remote(&root, "origin", &remote);
    support::push_branch(&root, "origin", "main");
    (dir, root)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (_temp, root) = fixture();
    let spec = RepoSpec {
        root: RepoRoot::new(&root)?,
        remote: RemoteName::new("origin")?,
        base_branch: BranchName::new("main")?,
        worktree_root: WorktreeRoot::new(".worktrees")?,
        branch_prefix: BranchPrefix::new("agent")?,
        require_clean_root: true,
        env: std::env::vars().collect::<BTreeMap<_, _>>().into(),
    };

    // The names are deterministic, so a caller can log them before the work starts.
    let (branch, path) = spec.names("demo run")?;
    println!("will cut {branch} at {}", path.as_path().display());

    let tree = spec.prepare("demo run").await?;
    println!(
        "cut {} at {} from {}",
        tree.branch,
        tree.path.as_path().display(),
        &tree.base_sha.as_str()[..12]
    );

    // Read the worktree through the in-process Git runner: no Git child process, no hooks.
    std::fs::write(tree.path.as_path().join("NOTES.md"), "agent notes\n")?;
    let root = RepoRoot::new(tree.path.as_path())?;
    let git = Git::new(root).with_env(spec.env.clone());
    let changed = git.status().await?;
    println!(
        "changed paths: {}",
        changed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let diff = git.diff(None, ByteLimit::new(64 * 1024)).await?;
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
    println!("removed {}", tree.path.as_path().display());
    Ok(())
}

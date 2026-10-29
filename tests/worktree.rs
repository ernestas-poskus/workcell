//! Worktree preparation, base selection, and removal, against real temporary Git repositories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use workcell::error::Error;
use workcell::worktree::RepoSpec;

fn env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn git_at(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_at(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_at(dir, args).status.success()
}

fn commit(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write");
    git(dir, &["add", name]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            &format!("add {name}"),
        ],
    );
}

/// A checkout whose `main` is on a bare origin, with the worktree root already ignored.
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let remote = dir.path().join("remote.git");
    std::fs::create_dir_all(&root).expect("checkout");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@example.invalid"]);
    std::fs::write(root.join(".gitignore"), ".worktrees/\n").expect("gitignore");
    commit(&root, ".gitignore", ".worktrees/\n");
    let status = Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .expect("git init bare");
    assert!(status.success());
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().expect("utf-8")],
    );
    git(&root, &["push", "-q", "-u", "origin", "main"]);
    (dir, root, remote)
}

fn spec(root: &Path) -> RepoSpec {
    RepoSpec {
        root: root.to_path_buf(),
        remote: "origin".to_owned(),
        base_branch: "main".to_owned(),
        worktree_root: PathBuf::from(".worktrees"),
        branch_prefix: "agent".to_owned(),
        require_clean_root: true,
        env: env(),
    }
}

fn clone_of(remote: &Path, into: &Path) {
    let status = Command::new("git")
        .args(["clone", "-q"])
        .arg(remote)
        .arg(into)
        .status()
        .expect("clone");
    assert!(status.success());
    git(into, &["config", "user.name", "t"]);
    git(into, &["config", "user.email", "t@example.invalid"]);
}

#[tokio::test]
async fn a_worktree_is_cut_from_the_base_removed_cleanly_and_its_git_dirs_are_recorded() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    let (branch, path) = spec.names("demo-93 run-1");
    let tree = spec.prepare("demo-93 run-1").await.expect("prepare");

    assert_eq!(tree.branch, branch);
    assert_eq!(tree.path, path);
    assert_eq!(tree.branch, "agent/demo-93-run-1/head");
    assert!(tree.path.is_dir());
    assert_eq!(
        tree.base_sha,
        spec.remote_base().await.expect("remote base")
    );
    assert_eq!(
        tree.base_sha,
        spec.current_base().await.expect("current base")
    );
    assert!(tree.git_dir.is_dir(), "{}", tree.git_dir.display());
    assert!(
        tree.git_dir
            .ends_with(Path::new(".git/worktrees/demo-93-run-1")),
        "{}",
        tree.git_dir.display()
    );
    let dot_git = std::fs::read_to_string(tree.path.join(".git")).expect(".git file");
    let recorded = dot_git
        .trim()
        .strip_prefix("gitdir: ")
        .expect("a linked worktree's `.git` file names its git dir");
    assert_eq!(
        std::fs::canonicalize(recorded).expect("recorded git dir"),
        std::fs::canonicalize(&tree.git_dir).expect("git dir")
    );
    assert_eq!(
        std::fs::canonicalize(&tree.git_common_dir).expect("common dir"),
        std::fs::canonicalize(root.join(".git")).expect("root git dir")
    );
    assert!(
        tree.git_common_dir
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo-93-run-1")
            .is_dir(),
        "both ref directories must exist after prepare"
    );
    assert!(
        tree.git_common_dir
            .join("logs")
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo-93-run-1")
            .is_dir(),
        "both reflog directories must exist after prepare"
    );

    commit(&tree.path, "feature.txt", "feature\n");
    spec.remove(&tree).await.expect("remove");
    assert!(!tree.path.exists());
    assert!(!git_ok(
        &root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", tree.branch)
        ]
    ));
}

#[tokio::test]
async fn a_dirty_integration_checkout_is_refused() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    std::fs::write(root.join("dirty.txt"), "dirty\n").expect("write");
    let error = spec.prepare("x").await.expect_err("dirty root");
    match error {
        Error::Path { detail } => assert!(detail.contains("dirty"), "{detail}"),
        other => panic!("unexpected error: {other:?}"),
    }
    let error = spec.ensure_clean().await.expect_err("dirty root");
    assert!(matches!(error, Error::Path { .. }), "{error}");
    std::fs::remove_file(root.join("dirty.txt")).expect("cleanup");
    spec.ensure_clean().await.expect("clean");
}

#[tokio::test]
async fn a_branch_or_path_collision_is_refused() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    let tree = spec.prepare("same").await.expect("first");
    let error = spec.prepare("same").await.expect_err("collision");
    match error {
        Error::Path { detail } => assert!(detail.contains("already exists"), "{detail}"),
        other => panic!("unexpected error: {other:?}"),
    }
    spec.remove(&tree).await.expect("remove");
}

#[tokio::test]
async fn a_local_base_beyond_the_remote_is_the_starting_point() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    commit(&root, "queued.txt", "queued\n");
    let local = git(&root, &["rev-parse", "HEAD"]);
    let remote = spec.remote_base().await.expect("remote base");
    assert_ne!(
        local, remote,
        "the fixture must have a local commit beyond origin"
    );

    let tree = spec.prepare("local-ahead").await.expect("prepare");
    assert_eq!(
        tree.base_sha, local,
        "a worktree must start from the local integration point"
    );
    spec.remove(&tree).await.expect("remove");
}

#[tokio::test]
async fn a_remote_base_beyond_the_local_branch_is_fast_forwarded() {
    let (dir, root, remote) = repo();
    let spec = spec(&root);
    let other = dir.path().join("other");
    clone_of(&remote, &other);
    commit(&other, "remote.txt", "remote\n");
    git(&other, &["push", "-q", "origin", "main"]);
    let advanced = git(&remote, &["rev-parse", "main"]);

    let tree = spec.prepare("remote-ahead").await.expect("prepare");
    assert_eq!(tree.base_sha, advanced);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), advanced);
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
    spec.remove(&tree).await.expect("remove");
}

#[tokio::test]
async fn a_diverged_base_is_refused() {
    let (dir, root, remote) = repo();
    let spec = spec(&root);
    let other = dir.path().join("other");
    clone_of(&remote, &other);
    commit(&other, "remote.txt", "remote\n");
    git(&other, &["push", "-q", "origin", "main"]);
    commit(&root, "local.txt", "local\n");

    let error = spec.prepare("diverged").await.expect_err("diverged");
    match error {
        Error::Git { detail, .. } => assert!(detail.contains("diverged"), "{detail}"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn prune_drops_metadata_for_a_worktree_directory_that_disappeared() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    let tree = spec.prepare("vanished").await.expect("prepare");
    std::fs::remove_dir_all(&tree.path).expect("remove directory");
    assert!(git(&root, &["worktree", "list", "--porcelain"]).contains("vanished"));
    spec.prune().await.expect("prune");
    assert!(!git(&root, &["worktree", "list", "--porcelain"]).contains("vanished"));
    assert!(git_ok(
        &root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", tree.branch)
        ]
    ));
    spec.remove(&tree).await.expect("remove");
}

#[test]
fn names_produces_the_nested_branch_for_configured_and_empty_prefixes() {
    let mut spec = spec(Path::new("/repo"));
    let (configured_branch, configured_path) = spec.names("demo-93 run-1");
    assert_eq!(configured_branch, "agent/demo-93-run-1/head");
    assert_eq!(
        configured_path,
        PathBuf::from("/repo/.worktrees/demo-93-run-1")
    );

    spec.branch_prefix = String::new();
    let (empty_branch, empty_path) = spec.names("demo-93 run-1");
    assert_eq!(empty_branch, "demo-93-run-1/head");
    assert_eq!(empty_path, PathBuf::from("/repo/.worktrees/demo-93-run-1"));
}

#[tokio::test]
async fn an_ancestor_ref_collision_is_refused_with_the_existing_collision_error() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    // An existing ancestor ref at <prefix>/<label>
    git(&root, &["branch", "agent/demo-93", "main"]);
    let error = spec
        .prepare("demo-93")
        .await
        .expect_err("ancestor collision");
    match error {
        Error::Path { detail } => {
            assert!(
                detail.contains("worktree collision: branch `agent/demo-93` already exists"),
                "{detail}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn a_base_branch_inside_the_run_ref_directory_is_refused() {
    let (_dir, root, _remote) = repo();
    let mut spec = spec(&root);
    // Base branch at or under run ref directory `agent/demo-93`
    spec.base_branch = "agent/demo-93".to_owned();
    let error = spec
        .prepare("demo-93")
        .await
        .expect_err("base branch inside run ref dir");
    match error {
        Error::Config { detail } => {
            assert!(
                detail.contains("at or under the run ref directory"),
                "{detail}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn discover_creates_the_directories_for_a_pre_upgrade_branch() {
    let (_dir, root, _remote) = repo();
    git(&root, &["branch", "agent/legacy", "main"]);
    let legacy_path = root.join(".worktrees").join("legacy");
    git(
        &root,
        &[
            "worktree",
            "add",
            legacy_path.to_str().unwrap(),
            "agent/legacy",
        ],
    );
    let git = workcell::git::Git::new(&legacy_path);
    let common_dir = root.join(".git");
    let ref_dir = common_dir.join("refs").join("heads").join("agent");
    let reflog_dir = common_dir
        .join("logs")
        .join("refs")
        .join("heads")
        .join("agent");
    let _ = std::fs::remove_dir_all(&reflog_dir);
    let worktree = workcell::worktree::Worktree::discover(&git)
        .await
        .expect("discover");
    assert_eq!(worktree.branch, "agent/legacy");
    assert!(ref_dir.is_dir(), "ref dir: {}", ref_dir.display());
    assert!(reflog_dir.is_dir(), "reflog dir: {}", reflog_dir.display());
}

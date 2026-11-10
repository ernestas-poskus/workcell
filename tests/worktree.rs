//! Worktree preparation, base selection, and removal, against real temporary Git repositories.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use workcell::error::Error;
use workcell::types::{
    BranchName, BranchPrefix, Env, RemoteName, RepoRoot, WorktreePath, WorktreeRoot,
};
use workcell::worktree::RepoSpec;

fn env() -> Env {
    std::env::vars().collect::<BTreeMap<_, _>>().into()
}

/// A checkout whose `main` is on a bare origin, with the worktree root already ignored.
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let remote = dir.path().join("remote.git");
    support::init(&root);
    support::configure_user(&root, "t", "t@example.invalid");
    support::commit(&root, ".gitignore", ".worktrees/\n");
    support::init_bare(&remote);
    support::add_remote(&root, "origin", &remote);
    support::push_branch(&root, "origin", "main");
    (dir, root, remote)
}

fn spec(root: &Path) -> RepoSpec {
    RepoSpec {
        root: RepoRoot::new(root).expect("repository root"),
        remote: RemoteName::new("origin").expect("remote"),
        base_branch: BranchName::new("main").expect("base branch"),
        worktree_root: WorktreeRoot::new(".worktrees").expect("worktree root"),
        branch_prefix: BranchPrefix::new("agent").expect("branch prefix"),
        require_clean_root: true,
        env: env(),
    }
}

fn clone_of(remote: &Path, into: &Path) {
    support::clone(remote, into);
    support::configure_user(into, "t", "t@example.invalid");
}

#[tokio::test]
async fn a_worktree_is_cut_from_the_base_removed_cleanly_and_its_git_dirs_are_recorded() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    let (branch, path) = spec.names("demo-93 run-1").expect("names");
    let tree = spec.prepare("demo-93 run-1").await.expect("prepare");

    assert_eq!(tree.branch, branch);
    assert_eq!(tree.path, path);
    assert_eq!(tree.branch.as_str(), "agent/demo-93-run-1/head");
    assert!(tree.path.as_path().is_dir());
    assert_eq!(
        tree.base_sha,
        spec.remote_base().await.expect("remote base")
    );
    assert_eq!(
        tree.base_sha,
        spec.current_base().await.expect("current base")
    );
    assert!(
        tree.git_dir.as_path().is_dir(),
        "{}",
        tree.git_dir.as_path().display()
    );
    assert!(
        tree.git_dir
            .as_path()
            .ends_with(Path::new(".git/worktrees/demo-93-run-1")),
        "{}",
        tree.git_dir.as_path().display()
    );
    let dot_git = std::fs::read_to_string(tree.path.as_path().join(".git")).expect(".git file");
    let recorded = dot_git
        .trim()
        .strip_prefix("gitdir: ")
        .expect("a linked worktree's `.git` file names its git dir");
    assert_eq!(
        std::fs::canonicalize(recorded).expect("recorded git dir"),
        std::fs::canonicalize(tree.git_dir.as_path()).expect("git dir")
    );
    assert_eq!(
        std::fs::canonicalize(tree.git_common_dir.as_path()).expect("common dir"),
        std::fs::canonicalize(root.join(".git")).expect("root git dir")
    );
    assert!(
        tree.git_common_dir
            .as_path()
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo-93-run-1")
            .is_dir(),
        "both ref directories must exist after prepare"
    );
    assert!(
        tree.git_common_dir
            .as_path()
            .join("logs")
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo-93-run-1")
            .is_dir(),
        "both reflog directories must exist after prepare"
    );

    support::commit(tree.path.as_path(), "feature.txt", "feature\n");
    spec.remove(&tree).await.expect("remove");
    assert!(!tree.path.as_path().exists());
    assert!(!support::show_ref_exists(
        &root,
        &format!("refs/heads/{}", tree.branch)
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
    support::commit(&root, "queued.txt", "queued\n");
    let local = support::rev_parse(&root, "HEAD");
    let remote = spec.remote_base().await.expect("remote base");
    assert_ne!(
        local.as_str(),
        remote.as_str(),
        "the fixture must have a local commit beyond origin"
    );

    let tree = spec.prepare("local-ahead").await.expect("prepare");
    assert_eq!(
        tree.base_sha.as_str(),
        local,
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
    support::commit(&other, "remote.txt", "remote\n");
    support::push_branch(&other, "origin", "main");
    let advanced = support::rev_parse(&remote, "main");

    let tree = spec.prepare("remote-ahead").await.expect("prepare");
    assert_eq!(tree.base_sha.as_str(), advanced);
    assert_eq!(support::rev_parse(&root, "HEAD"), advanced);
    assert_eq!(support::status_porcelain(&root), "");
    spec.remove(&tree).await.expect("remove");
}

#[tokio::test]
async fn a_diverged_base_is_refused() {
    let (dir, root, remote) = repo();
    let spec = spec(&root);
    let other = dir.path().join("other");
    clone_of(&remote, &other);
    support::commit(&other, "remote.txt", "remote\n");
    support::push_branch(&other, "origin", "main");
    support::commit(&root, "local.txt", "local\n");

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
    std::fs::remove_dir_all(tree.path.as_path()).expect("remove directory");
    assert!(support::worktree_list_porcelain(&root).contains("vanished"));
    spec.prune().await.expect("prune");
    assert!(!support::worktree_list_porcelain(&root).contains("vanished"));
    assert!(support::show_ref_exists(
        &root,
        &format!("refs/heads/{}", tree.branch)
    ));
    spec.remove(&tree).await.expect("remove");
}

#[test]
fn names_produces_the_nested_branch_and_absolute_path() {
    let spec = spec(Path::new("/repo"));
    let (branch, path) = spec.names("demo-93 run-1").expect("names");
    assert_eq!(branch.as_str(), "agent/demo-93-run-1/head");
    assert_eq!(
        path.as_path(),
        PathBuf::from("/repo/.worktrees/demo-93-run-1")
    );
}

#[tokio::test]
async fn an_ancestor_ref_collision_is_refused_with_the_existing_collision_error() {
    let (_dir, root, _remote) = repo();
    let spec = spec(&root);
    // An existing ancestor ref at <prefix>/<label>
    support::create_branch(&root, "agent/demo-93", "main");
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
    spec.base_branch = BranchName::new("agent/demo-93").expect("base branch");
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
    support::create_branch(&root, "agent/legacy", "main");
    let legacy_path = root.join(".worktrees").join("legacy");
    support::worktree_add(&root, "agent/legacy", &legacy_path);
    let git = workcell::git::Git::new(RepoRoot::new(&legacy_path).expect("repository root"));
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
    assert_eq!(worktree.branch.as_str(), "agent/legacy");
    assert!(ref_dir.is_dir(), "ref dir: {}", ref_dir.display());
    assert!(reflog_dir.is_dir(), "reflog dir: {}", reflog_dir.display());
}

#[tokio::test]
async fn branches_with_prefix_filters_and_sorts() {
    let (_dir, root, _remote) = repo();
    support::create_branch(&root, "prefix/beta", "main");
    support::create_branch(&root, "prefix/alpha", "main");
    support::create_branch(&root, "other/gamma", "main");

    let git = workcell::git::Git::new(RepoRoot::new(&root).expect("repository root"));
    let matched = git
        .branches_with_prefix(&BranchPrefix::new("prefix/").expect("prefix"))
        .await
        .expect("prefix");
    let matched: Vec<&str> = matched.iter().map(BranchName::as_str).collect();
    assert_eq!(matched, vec!["prefix/alpha", "prefix/beta"]);
}

#[tokio::test]
async fn worktree_add_existing_checks_out_branch_and_validates_path_and_branch() {
    let (_dir, root, _remote) = repo();
    support::create_branch(&root, "prefix/beta", "main");
    support::commit(&root, "main-advance.txt", "advance\n");
    let tip = support::rev_parse(&root, "prefix/beta");

    let git = workcell::git::Git::new(RepoRoot::new(&root).expect("repository root"));
    let existing_path = root.join(".worktrees").join("existing");
    let existing_branch = BranchName::new("prefix/beta").expect("branch");
    let existing_worktree = WorktreePath::new(&existing_path).expect("worktree path");
    git.worktree_add_existing(&existing_branch, &existing_worktree)
        .await
        .expect("worktree_add_existing");

    let worktree_git =
        workcell::git::Git::new(RepoRoot::new(&existing_path).expect("repository root"));
    assert_eq!(
        worktree_git
            .head()
            .await
            .map(workcell::types::CommitId::into_string),
        Some(tip.clone())
    );
    assert_eq!(
        worktree_git
            .branch_name()
            .await
            .expect("branch")
            .map(workcell::types::BranchName::into_string),
        Some("prefix/beta".to_owned())
    );

    // Existing path fails
    let error = git
        .worktree_add_existing(&existing_branch, &existing_worktree)
        .await
        .expect_err("existing path");
    match error {
        Error::Path { detail } => {
            assert!(detail.contains("already exists"), "{detail}");
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Missing branch fails
    let missing_path = root.join(".worktrees").join("missing");
    let error = git
        .worktree_add_existing(
            &BranchName::new("missing-branch").expect("branch"),
            &WorktreePath::new(&missing_path).expect("worktree path"),
        )
        .await
        .expect_err("missing branch");
    match error {
        Error::Path { detail } => {
            assert!(detail.contains("not a local branch"), "{detail}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!missing_path.exists());
}

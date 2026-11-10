//! The Git runner against real temporary repositories.
//!
//! These tests are the executable form of the hardening contract: a planted hook, an external diff
//! driver, and a textconv driver must never run, while the primitives a workspace needs must keep
//! working. The runner is in-process (`git2`), and so are the fixtures. The three hardening tests
//! below deliberately keep one plain `git` child process each as a negative control: they prove
//! that the system Git would execute the planted hook or driver before the runner is asserted not
//! to. No other test or fixture spawns a Git child.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use workcell::error::Error;
use workcell::git::Git;
use workcell::types::{
    BranchName, ByteLimit, ReferenceName, RelativePath, RemoteName, RepoRoot, Revision,
    WorktreePath,
};

/// The environment a caller would supply: the real one, which is enough for Git to work.
fn env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn repo_root(path: &Path) -> RepoRoot {
    RepoRoot::new(path).expect("repository root")
}

fn revision(value: &str) -> Revision {
    Revision::new(value).expect("revision")
}

fn branch(value: &str) -> BranchName {
    BranchName::new(value).expect("branch")
}

fn reference(value: &str) -> ReferenceName {
    ReferenceName::new(value).expect("reference")
}

fn worktree_path(path: &Path) -> WorktreePath {
    WorktreePath::new(path).expect("worktree path")
}

fn relative(value: &str) -> RelativePath {
    RelativePath::new(value).expect("relative path")
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    support::init(&root);
    support::configure_user(&root, "t", "t@example.invalid");
    support::commit(&root, "README.md", "base\n");
    (dir, root)
}

fn repo_with_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (dir, root) = repo();
    let remote = dir.path().join("remote.git");
    support::init_bare(&remote);
    support::add_remote(&root, "origin", &remote);
    support::push_branch(&root, "origin", "main");
    (dir, root, remote)
}

fn runner(root: &Path) -> Git {
    Git::new(repo_root(root)).with_env(env().into())
}

#[tokio::test]
async fn the_basic_primitives_read_the_repository() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let head = git.head().await.expect("head");
    assert_eq!(head.as_str().len(), 40, "{head}");
    assert_eq!(
        git.rev_parse(&revision("HEAD")).await.expect("rev-parse"),
        head
    );
    assert_eq!(
        git.branch_name()
            .await
            .expect("branch")
            .map(BranchName::into_string),
        Some("main".to_owned())
    );
    assert!(
        git.show_ref(&reference("refs/heads/main"))
            .await
            .expect("show-ref")
    );
    assert!(
        !git.show_ref(&reference("refs/heads/nope"))
            .await
            .expect("show-ref")
    );

    let empty = _dir.path().join("empty");
    support::init(&empty);
    assert!(
        runner(&empty).head().await.is_none(),
        "an unborn HEAD is not a commit"
    );
    assert_eq!(runner(&empty).branch_name().await.expect("branch"), None);
}

#[tokio::test]
async fn status_reports_changed_paths() {
    let (_dir, root) = repo();
    let git = runner(&root);
    assert!(git.status().await.expect("status").is_empty());
    std::fs::write(root.join("new.txt"), "new\n").expect("write");
    let status = git.status().await.expect("status");
    assert_eq!(status, vec![relative("new.txt")]);
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    let status = git.status().await.expect("status");
    assert!(status.contains(&relative("README.md")), "{status:?}");
}

#[tokio::test]
async fn ancestry_and_merge_base_are_reported() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let base = git.rev_parse(&revision("HEAD")).await.expect("base");
    support::commit(&root, "next.txt", "next\n");
    let head = git.rev_parse(&revision("HEAD")).await.expect("head");
    assert!(
        git.is_ancestor(&revision(base.as_str()), &revision(head.as_str()))
            .await
            .expect("ancestor")
    );
    assert!(
        !git.is_ancestor(&revision(head.as_str()), &revision(base.as_str()))
            .await
            .expect("ancestor")
    );
    assert_eq!(
        git.merge_base(&revision(base.as_str()), &revision(head.as_str()))
            .await
            .expect("merge-base"),
        base
    );
}

#[tokio::test]
async fn fetch_updates_the_remote_tracking_ref() {
    let (_dir, root, remote) = repo_with_origin();
    let other = _dir.path().join("other");
    support::clone(&remote, &other);
    support::configure_user(&other, "t", "t@example.invalid");
    support::checkout_new(&other, "topic");
    support::commit(&other, "topic.txt", "topic\n");
    support::push_branch(&other, "origin", "topic");

    let git = runner(&root);
    assert!(
        !git.show_ref(&reference("refs/remotes/origin/topic"))
            .await
            .expect("before")
    );
    git.fetch(&RemoteName::new("origin").expect("remote"))
        .await
        .expect("fetch");
    assert!(
        git.show_ref(&reference("refs/remotes/origin/topic"))
            .await
            .expect("after")
    );
}

#[tokio::test]
async fn rebase_rebase_abort_and_reset_hard_leave_a_clean_tree() {
    let (_dir, root) = repo();
    let git = runner(&root);
    // Two branches that changed the same file conflict on rebase.
    support::checkout_new(&root, "side");
    support::commit(&root, "README.md", "side\n");
    support::checkout(&root, "main");
    support::commit(&root, "README.md", "main\n");
    support::checkout(&root, "side");
    let error = git.rebase(&revision("main")).await.expect_err("conflict");
    assert!(matches!(error, Error::Git { .. }), "{error}");
    git.rebase_abort().await.expect("abort");
    assert_eq!(
        git.status().await.expect("status"),
        Vec::<RelativePath>::new()
    );

    // reset_hard discards a working-tree edit.
    std::fs::write(root.join("README.md"), "dirty\n").expect("write");
    assert!(!git.status().await.expect("status").is_empty());
    git.reset_hard(&revision("HEAD")).await.expect("reset");
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).expect("read"),
        "side\n"
    );
}

#[tokio::test]
async fn update_ref_is_compare_and_swap() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let old = git.rev_parse(&revision("HEAD")).await.expect("old");
    support::commit(&root, "next.txt", "next\n");
    let new = git.rev_parse(&revision("HEAD")).await.expect("new");
    support::create_ref(&root, "refs/heads/cas", old.as_str());
    let cas = reference("refs/heads/cas");
    git.update_ref(&cas, &new, &old).await.expect("swap");
    assert_eq!(
        git.rev_parse(&revision("refs/heads/cas"))
            .await
            .expect("cas"),
        new
    );
    let error = git
        .update_ref(&cas, &old, &old)
        .await
        .expect_err("stale old value");
    assert!(matches!(error, Error::Git { .. }), "{error}");
    assert_eq!(
        git.rev_parse(&revision("refs/heads/cas"))
            .await
            .expect("cas"),
        new
    );
}

#[tokio::test]
async fn worktree_management_primitives_work() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add(
        &branch("tree"),
        &worktree_path(&path),
        &git.rev_parse(&revision("main")).await.expect("base"),
    )
    .await
    .expect("add");
    assert!(path.is_dir());
    let entries = git.list_worktrees().await.expect("list");
    let entry = entries
        .iter()
        .find(|entry| {
            entry
                .branch
                .as_ref()
                .map(workcell::types::BranchName::as_str)
                == Some("tree")
        })
        .expect("entry");
    assert_eq!(
        std::fs::canonicalize(&entry.path).expect("canon"),
        std::fs::canonicalize(&path).expect("canon")
    );
    git.worktree_remove(&worktree_path(&path))
        .await
        .expect("remove");
    assert!(!path.exists());
    git.branch_delete(&branch("tree")).await.expect("delete");
    assert!(
        !git.show_ref(&reference("refs/heads/tree"))
            .await
            .expect("show-ref")
    );
    git.worktree_prune().await.expect("prune");
}

#[tokio::test]
async fn a_linked_worktree_rebase_conflict_aborts_cleanly() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add(
        &branch("tree"),
        &worktree_path(&path),
        &git.rev_parse(&revision("main")).await.expect("base"),
    )
    .await
    .expect("add");
    support::commit_file(&path, "README.md", "side\n", "side");
    support::commit_file(&root, "README.md", "main\n", "main");
    let worktree_git = runner(&path);
    worktree_git
        .rebase(&revision("main"))
        .await
        .expect_err("conflict");
    worktree_git.rebase_abort().await.expect("abort");
    assert_eq!(
        worktree_git
            .branch_name()
            .await
            .expect("branch")
            .map(BranchName::into_string),
        Some("tree".to_owned())
    );
    assert!(
        worktree_git.status().await.expect("status").is_empty(),
        "the aborted rebase leaves no changes"
    );
    git.worktree_remove(&worktree_path(&path))
        .await
        .expect("remove");
    git.branch_delete(&branch("tree")).await.expect("delete");
}

#[tokio::test]
async fn a_linked_worktree_with_a_commit_rebases_cleanly() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add(
        &branch("tree"),
        &worktree_path(&path),
        &git.rev_parse(&revision("main")).await.expect("base"),
    )
    .await
    .expect("add");
    support::commit_file(&path, "work.txt", "work\n", "work");
    support::commit(&root, "main.txt", "main\n");
    runner(&path)
        .rebase(&revision("main"))
        .await
        .expect("rebased");
    assert!(path.join("main.txt").is_file());
    git.worktree_remove(&worktree_path(&path))
        .await
        .expect("remove");
    git.branch_delete(&branch("tree")).await.expect("delete");
}

#[tokio::test]
async fn git_directories_distinguish_a_linked_worktree() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add(
        &branch("tree"),
        &worktree_path(&path),
        &git.rev_parse(&revision("main")).await.expect("base"),
    )
    .await
    .expect("add");
    let directories = runner(&path).git_directories().await.expect("dirs");
    assert!(
        directories.git_dir.as_path().ends_with("worktrees/tree"),
        "{}",
        directories.git_dir.as_path().display()
    );
    assert!(
        directories.common_dir.as_path().ends_with(".git"),
        "{}",
        directories.common_dir.as_path().display()
    );
    git.worktree_remove(&worktree_path(&path))
        .await
        .expect("remove");
    git.branch_delete(&branch("tree")).await.expect("delete");
}

#[tokio::test]
async fn diff_is_bounded_and_lists_every_changed_path() {
    let (_dir, root) = repo();
    let git = runner(&root);
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    for index in 0..40 {
        std::fs::write(root.join(format!("f{index}.txt")), format!("{index}\n")).expect("write");
    }
    support::stage_all(&root);
    let diff = git.diff(None, ByteLimit::new(200)).await.expect("diff");
    assert_eq!(
        diff.base,
        git.rev_parse(&revision("HEAD")).await.expect("head")
    );
    assert_eq!(
        diff.range.as_str(),
        format!("{} -> working tree", diff.base)
    );
    assert!(
        diff.changed_files
            .iter()
            .any(|path| path.as_path() == std::path::Path::new("README.md"))
    );
    assert_eq!(diff.changed_files.len(), 41);
    assert!(!diff.stat.is_empty());
    assert!(diff.patch.len() <= 200);
    assert!(diff.truncated_from.is_some_and(|size| size.get() > 200));
    // A generous cap keeps the whole patch and reports no truncation.
    let whole = git
        .diff(Some(&revision("HEAD")), ByteLimit::new(usize::MAX))
        .await
        .expect("diff");
    assert!(whole.patch.contains("f39.txt"));
    assert!(whole.truncated_from.is_none());
}

#[tokio::test]
async fn local_config_digest_tracks_config_and_includes() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let empty = git.local_config_digest().await.expect("digest");
    support::set_config(&root, "core.editor", "true");
    let changed = git.local_config_digest().await.expect("digest");
    assert_ne!(empty, changed);

    let included = _dir.path().join("included.config");
    std::fs::write(&included, "[core]\n\teditor = included-a\n").expect("write include");
    support::set_config(&root, "include.path", included.to_str().expect("utf-8"));
    let with_include = git.local_config_digest().await.expect("digest");
    std::fs::write(&included, "[core]\n\teditor = included-b\n").expect("write include");
    let changed_include = git.local_config_digest().await.expect("digest");
    assert_ne!(with_include, changed_include);
}

#[tokio::test]
async fn a_planted_hook_never_runs() {
    let (_dir, root) = repo();
    let git = runner(&root);
    // A feature branch and a main commit, so the merge is a real (non-fast-forward) merge.
    support::checkout_new(&root, "feature");
    support::commit(&root, "feature.txt", "feature\n");
    support::checkout(&root, "main");
    let before = git.rev_parse(&revision("HEAD")).await.expect("head");
    support::commit(&root, "main.txt", "main\n");

    let marker = _dir.path().join("hook-ran");
    plant_hook(
        &root,
        "post-merge",
        &format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display()),
    );
    // Deliberate plain-Git negative control: this is the one step that must execute the system
    // `git` binary, so the test first proves the planted hook is live in this environment. Only
    // then can the crate's in-process merge be asserted not to run it. Do not convert this to
    // `git2`.
    let plain = Command::new("git")
        .args(["-C"])
        .arg(&root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "merge",
            "--no-edit",
            "--",
            "feature",
        ])
        .output()
        .expect("plain merge");
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        marker.exists(),
        "the planted hook did not run under plain git"
    );
    // Undo the plain merge and remove the witness.
    support::reset_hard(&root, before.as_str());
    std::fs::remove_file(&marker).expect("remove marker");

    // The same merge through the crate must not execute the hook.
    git.merge(&branch("feature")).await.expect("crate merge");
    eprintln!("log: {}", support::log_oneline(&root, 4));
    eprintln!("tree: {}", support::ls_tree(&root));
    eprintln!("status: {}", support::status_porcelain(&root));
    assert!(root.join("feature.txt").is_file());
    assert!(
        !marker.exists(),
        "a repository hook executed through the Git runner"
    );
}

#[tokio::test]
async fn an_external_diff_driver_never_runs() {
    let (_dir, root) = repo();
    let marker = _dir.path().join("ext-diff-ran");
    let driver = plant_hook(
        &root,
        "external-diff.sh",
        &format!("#!/bin/sh\nprintf ran > '{}'\nexit 1\n", marker.display()),
    );
    std::fs::write(root.join("README.md"), "changed\n").expect("write");

    // Deliberate plain-Git negative control: only the system `git` honours `GIT_EXTERNAL_DIFF`,
    // so this step proves the driver is live before the crate's in-process diff is asserted not to
    // invoke it. Do not convert this to `git2`.
    let plain = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("diff")
        .env("GIT_EXTERNAL_DIFF", &driver)
        .output()
        .expect("plain diff");
    assert!(
        marker.exists(),
        "GIT_EXTERNAL_DIFF did not run under plain git: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    std::fs::remove_file(&marker).expect("remove marker");

    let mut env = env();
    env.insert("GIT_EXTERNAL_DIFF".to_owned(), driver.display().to_string());
    let git = Git::new(repo_root(&root)).with_env(env.into());
    let diff = git
        .diff(None, ByteLimit::new(usize::MAX))
        .await
        .expect("diff");
    assert!(diff.patch.contains("changed"), "{}", diff.patch);
    assert!(
        !marker.exists(),
        "an external diff driver executed through the Git runner"
    );
}

#[tokio::test]
async fn a_textconv_driver_never_runs() {
    let (_dir, root) = repo();
    let marker = _dir.path().join("textconv-ran");
    let driver = plant_hook(
        &root,
        "textconv.sh",
        &format!(
            "#!/bin/sh\nprintf ran > '{}'\ncat \"$1\"\n",
            marker.display()
        ),
    );
    support::commit_file(
        &root,
        ".gitattributes",
        "README.md diff=hostile\n",
        "attributes",
    );
    support::set_config(
        &root,
        "diff.hostile.textconv",
        driver.to_str().expect("utf-8"),
    );
    std::fs::write(root.join("README.md"), "changed\n").expect("write");

    // Deliberate plain-Git negative control: only the system `git` runs a configured textconv
    // driver, so this step proves it is live before the crate's in-process diff is asserted not
    // to invoke it. Do not convert this to `git2`.
    let plain = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["diff", "HEAD", "--", "README.md"])
        .output()
        .expect("plain diff");
    assert!(
        marker.exists(),
        "the textconv driver did not run under plain git: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    std::fs::remove_file(&marker).expect("remove marker");

    let diff = runner(&root)
        .diff(None, ByteLimit::new(usize::MAX))
        .await
        .expect("diff");
    assert!(diff.patch.contains("changed"), "{}", diff.patch);
    assert!(
        !marker.exists(),
        "a textconv driver executed through the Git runner"
    );
}

#[tokio::test]
async fn the_commit_signature_uses_the_supplied_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("env");
    support::init(&root);
    support::configure_user(&root, "Repo", "repo@example.invalid");
    support::commit(&root, "README.md", "base\n");
    support::checkout_new(&root, "feature");
    support::commit(&root, "feature.txt", "feature\n");
    support::checkout(&root, "main");
    support::commit(&root, "main.txt", "main\n");
    let mut supplied = BTreeMap::new();
    supplied.insert("GIT_COMMITTER_NAME".to_owned(), "Env Committer".to_owned());
    supplied.insert(
        "GIT_COMMITTER_EMAIL".to_owned(),
        "committer@example.invalid".to_owned(),
    );
    let git = Git::new(repo_root(&root)).with_env(supplied.into());
    git.merge(&branch("feature")).await.expect("merge");
    let repo = git2::Repository::open(&root).expect("open");
    let commit = repo.head().expect("head").peel_to_commit().expect("commit");
    assert_eq!(
        commit.author().name().expect("author name"),
        "Env Committer"
    );
    assert_eq!(
        commit.author().email().expect("author email"),
        "committer@example.invalid"
    );
    assert_eq!(
        commit.committer().name().expect("committer name"),
        "Env Committer"
    );
    assert_eq!(
        commit.committer().email().expect("committer email"),
        "committer@example.invalid"
    );
}

#[tokio::test]
async fn a_missing_program_is_a_process_error_and_a_missing_repository_is_a_git_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = Git::new(repo_root(dir.path()))
        .with_env(env().into())
        .status()
        .await
        .expect_err("not a repository");
    assert!(matches!(error, Error::Git { .. }), "{error}");

    let missing = dir.path().join("does-not-exist");
    let error = Git::new(repo_root(&missing))
        .with_env(env().into())
        .status()
        .await
        .expect_err("no such directory");
    assert!(matches!(error, Error::Process { .. }), "{error}");
}

/// Write an executable file under `.git/hooks/` (or `.git/`) and return its path.
fn plant_hook(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(".git").join(if name.ends_with(".sh") {
        name.to_owned()
    } else {
        format!("hooks/{name}")
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hook");
    }
    path
}

#[tokio::test]
async fn commit_all_stages_the_working_tree_on_head() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let before = git.head().await.expect("head");
    assert_eq!(
        git.commit_all("fixture: finish").await.expect("commit"),
        None
    );
    std::fs::write(root.join("new.txt"), "new\n").expect("write");
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    let commit = git
        .commit_all("fixture: finish")
        .await
        .expect("commit")
        .expect("a commit");
    assert_ne!(commit, before);
    assert!(git.status().await.expect("status").is_empty());
    let head = git.head().await.expect("head");
    assert_eq!(head, commit);
    let message = support::head_subject(&root);
    assert_eq!(message, "fixture: finish");
}

#[test]
fn production_git_runner_has_no_git_child_process() {
    for (name, source) in [
        ("git.rs", include_str!("../src/git.rs")),
        ("worktree.rs", include_str!("../src/worktree.rs")),
        ("gc.rs", include_str!("../src/gc.rs")),
    ] {
        assert!(
            !source.contains("Command::new(\"git\")"),
            "{name} must not spawn a git child process"
        );
    }
}

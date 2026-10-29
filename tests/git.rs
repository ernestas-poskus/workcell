//! The Git runner against real temporary repositories.
//!
//! These tests are the executable form of the hardening contract: a planted hook, an external diff
//! driver, and a textconv driver must never run, while the primitives a workspace needs must keep
//! working. The runner is in-process (`git2`); the fixtures still use the system `git` binary to
//! create states a caller would create outside the crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use workcell::error::Error;
use workcell::git::Git;

/// The environment a caller would supply: the real one, which is enough for Git to work.
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

fn git_cmd(dir: &Path, args: &[&str]) -> String {
    let output = git_at(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write");
    git_cmd(dir, &["add", name]);
    git_cmd(
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

fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    std::fs::create_dir_all(&root).expect("checkout");
    git_cmd(&root, &["init", "-q", "-b", "main"]);
    git_cmd(&root, &["config", "user.name", "t"]);
    git_cmd(&root, &["config", "user.email", "t@example.invalid"]);
    commit(&root, "README.md", "base\n");
    (dir, root)
}

fn repo_with_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (dir, root) = repo();
    let remote = dir.path().join("remote.git");
    let status = Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .expect("git init bare");
    assert!(status.success());
    git_cmd(
        &root,
        &["remote", "add", "origin", remote.to_str().expect("utf-8")],
    );
    git_cmd(&root, &["push", "-q", "-u", "origin", "main"]);
    (dir, root, remote)
}

fn runner(root: &Path) -> Git {
    Git::new(root).with_env(env())
}

#[tokio::test]
async fn the_basic_primitives_read_the_repository() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let head = git.head().await.expect("head");
    assert_eq!(head.len(), 40, "{head}");
    assert_eq!(git.rev_parse("HEAD").await.expect("rev-parse"), head);
    assert_eq!(
        git.branch_name().await.expect("branch"),
        Some("main".to_owned())
    );
    assert!(git.show_ref("refs/heads/main").await.expect("show-ref"));
    assert!(!git.show_ref("refs/heads/nope").await.expect("show-ref"));

    let empty = _dir.path().join("empty");
    std::fs::create_dir_all(&empty).expect("empty");
    raw_init(&empty);
    assert!(
        runner(&empty).head().await.is_none(),
        "an unborn HEAD is not a commit"
    );
    assert_eq!(runner(&empty).branch_name().await.expect("branch"), None);
}

fn raw_init(root: &Path) {
    let output = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(root)
        .output()
        .expect("git init");
    assert!(output.status.success());
}

#[tokio::test]
async fn status_reports_changed_paths() {
    let (_dir, root) = repo();
    let git = runner(&root);
    assert!(git.status().await.expect("status").is_empty());
    std::fs::write(root.join("new.txt"), "new\n").expect("write");
    let status = git.status().await.expect("status");
    assert_eq!(status, vec![PathBuf::from("new.txt")]);
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    let status = git.status().await.expect("status");
    assert!(status.contains(&PathBuf::from("README.md")), "{status:?}");
}

#[tokio::test]
async fn ancestry_and_merge_base_are_reported() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let base = git.rev_parse("HEAD").await.expect("base");
    commit(&root, "next.txt", "next\n");
    let head = git.rev_parse("HEAD").await.expect("head");
    assert!(git.is_ancestor(&base, &head).await.expect("ancestor"));
    assert!(!git.is_ancestor(&head, &base).await.expect("ancestor"));
    assert_eq!(
        git.merge_base(&base, &head).await.expect("merge-base"),
        base
    );
}

#[tokio::test]
async fn fetch_updates_the_remote_tracking_ref() {
    let (_dir, root, remote) = repo_with_origin();
    let other = _dir.path().join("other");
    let status = Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(&other)
        .status()
        .expect("clone");
    assert!(status.success());
    git_cmd(&other, &["config", "user.name", "t"]);
    git_cmd(&other, &["config", "user.email", "t@example.invalid"]);
    git_cmd(&other, &["checkout", "-q", "-b", "topic"]);
    commit(&other, "topic.txt", "topic\n");
    git_cmd(&other, &["push", "-q", "origin", "topic"]);

    let git = runner(&root);
    assert!(
        !git.show_ref("refs/remotes/origin/topic")
            .await
            .expect("before")
    );
    git.fetch("origin").await.expect("fetch");
    assert!(
        git.show_ref("refs/remotes/origin/topic")
            .await
            .expect("after")
    );
}

#[tokio::test]
async fn rebase_rebase_abort_and_reset_hard_leave_a_clean_tree() {
    let (_dir, root) = repo();
    let git = runner(&root);
    // Two branches that changed the same file conflict on rebase.
    git_cmd(&root, &["checkout", "-q", "-b", "side"]);
    commit(&root, "README.md", "side\n");
    git_cmd(&root, &["checkout", "-q", "main"]);
    commit(&root, "README.md", "main\n");
    git_cmd(&root, &["checkout", "-q", "side"]);
    let error = git.rebase("main").await.expect_err("conflict");
    assert!(matches!(error, Error::Git { .. }), "{error}");
    git.rebase_abort().await.expect("abort");
    assert_eq!(git.status().await.expect("status"), Vec::<PathBuf>::new());

    // reset_hard discards a working-tree edit.
    std::fs::write(root.join("README.md"), "dirty\n").expect("write");
    assert!(!git.status().await.expect("status").is_empty());
    git.reset_hard("HEAD").await.expect("reset");
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).expect("read"),
        "side\n"
    );
}

#[tokio::test]
async fn update_ref_is_compare_and_swap() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let old = git.rev_parse("HEAD").await.expect("old");
    commit(&root, "next.txt", "next\n");
    let new = git.rev_parse("HEAD").await.expect("new");
    git_cmd(&root, &["update-ref", "refs/heads/cas", &old]);
    git.update_ref("refs/heads/cas", &new, &old)
        .await
        .expect("swap");
    assert_eq!(git.rev_parse("refs/heads/cas").await.expect("cas"), new);
    let error = git
        .update_ref("refs/heads/cas", &old, &old)
        .await
        .expect_err("stale old value");
    assert!(matches!(error, Error::Git { .. }), "{error}");
    assert_eq!(git.rev_parse("refs/heads/cas").await.expect("cas"), new);
}

#[tokio::test]
async fn worktree_management_primitives_work() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add("tree", &path, "main").await.expect("add");
    assert!(path.is_dir());
    let entries = git.list_worktrees().await.expect("list");
    let entry = entries
        .iter()
        .find(|entry| entry.branch.as_deref() == Some("tree"))
        .expect("entry");
    assert_eq!(
        std::fs::canonicalize(&entry.path).expect("canon"),
        std::fs::canonicalize(&path).expect("canon")
    );
    git.worktree_remove(&path).await.expect("remove");
    assert!(!path.exists());
    git.branch_delete("tree").await.expect("delete");
    assert!(!git.show_ref("refs/heads/tree").await.expect("show-ref"));
    git.worktree_prune().await.expect("prune");
}

#[tokio::test]
async fn a_linked_worktree_rebase_conflict_aborts_cleanly() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add("tree", &path, "main").await.expect("add");
    std::fs::write(path.join("README.md"), "side\n").expect("write");
    git_cmd(&path, &["add", "README.md"]);
    git_cmd(
        &path,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "side",
        ],
    );
    std::fs::write(root.join("README.md"), "main\n").expect("write");
    git_cmd(&root, &["add", "README.md"]);
    git_cmd(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "main",
        ],
    );
    let worktree_git = runner(&path);
    worktree_git.rebase("main").await.expect_err("conflict");
    worktree_git.rebase_abort().await.expect("abort");
    assert_eq!(
        worktree_git.branch_name().await.expect("branch"),
        Some("tree".to_owned())
    );
    assert!(
        worktree_git.status().await.expect("status").is_empty(),
        "the aborted rebase leaves no changes"
    );
    git.worktree_remove(&path).await.expect("remove");
    git.branch_delete("tree").await.expect("delete");
}

#[tokio::test]
async fn a_linked_worktree_with_a_commit_rebases_cleanly() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add("tree", &path, "main").await.expect("add");
    std::fs::write(path.join("work.txt"), "work\n").expect("write");
    git_cmd(&path, &["add", "work.txt"]);
    git_cmd(
        &path,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "work",
        ],
    );
    commit(&root, "main.txt", "main\n");
    runner(&path).rebase("main").await.expect("rebased");
    assert!(path.join("main.txt").is_file());
    git.worktree_remove(&path).await.expect("remove");
    git.branch_delete("tree").await.expect("delete");
}

#[tokio::test]
async fn git_directories_distinguish_a_linked_worktree() {
    let (dir, root) = repo();
    let git = runner(&root);
    let path = dir.path().join("tree");
    git.worktree_add("tree", &path, "main").await.expect("add");
    let directories = runner(&path).git_directories().await.expect("dirs");
    assert!(
        directories.git_dir.ends_with("worktrees/tree"),
        "{}",
        directories.git_dir.display()
    );
    assert!(
        directories.common_dir.ends_with(".git"),
        "{}",
        directories.common_dir.display()
    );
    git.worktree_remove(&path).await.expect("remove");
    git.branch_delete("tree").await.expect("delete");
}

#[tokio::test]
async fn diff_is_bounded_and_lists_every_changed_path() {
    let (_dir, root) = repo();
    let git = runner(&root);
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    for index in 0..40 {
        std::fs::write(root.join(format!("f{index}.txt")), format!("{index}\n")).expect("write");
    }
    git_cmd(&root, &["add", "-A"]);
    let diff = git.diff(None, 200).await.expect("diff");
    assert_eq!(diff.base, git.rev_parse("HEAD").await.expect("head"));
    assert_eq!(diff.range, format!("{} -> working tree", diff.base));
    assert!(diff.changed_files.contains(&"README.md".to_owned()));
    assert_eq!(diff.changed_files.len(), 41);
    assert!(!diff.stat.is_empty());
    assert!(diff.patch.len() <= 200);
    assert!(diff.truncated_from.is_some_and(|size| size > 200));
    // A generous cap keeps the whole patch and reports no truncation.
    let whole = git.diff(Some("HEAD"), usize::MAX).await.expect("diff");
    assert!(whole.patch.contains("f39.txt"));
    assert!(whole.truncated_from.is_none());
}

#[tokio::test]
async fn local_config_digest_tracks_config_and_includes() {
    let (_dir, root) = repo();
    let git = runner(&root);
    let empty = git.local_config_digest().await.expect("digest");
    git_cmd(&root, &["config", "core.editor", "true"]);
    let changed = git.local_config_digest().await.expect("digest");
    assert_ne!(empty, changed);

    let included = _dir.path().join("included.config");
    std::fs::write(&included, "[core]\n\teditor = included-a\n").expect("write include");
    git_cmd(
        &root,
        &["config", "include.path", included.to_str().expect("utf-8")],
    );
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
    git_cmd(&root, &["checkout", "-q", "-b", "feature"]);
    commit(&root, "feature.txt", "feature\n");
    git_cmd(&root, &["checkout", "-q", "main"]);
    let before = git.rev_parse("HEAD").await.expect("head");
    commit(&root, "main.txt", "main\n");

    let marker = _dir.path().join("hook-ran");
    plant_hook(
        &root,
        "post-merge",
        &format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display()),
    );
    // Prove the planted hook is live: a plain merge runs it.
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
    git_cmd(&root, &["reset", "--hard", &before]);
    std::fs::remove_file(&marker).expect("remove marker");

    // The same merge through the crate must not execute the hook.
    git.merge("feature").await.expect("crate merge");
    eprintln!(
        "log: {}",
        git_cmd(&root, &["log", "--oneline", "--graph", "-4"])
    );
    eprintln!(
        "tree: {}",
        git_cmd(&root, &["ls-tree", "-r", "--name-only", "HEAD"])
    );
    eprintln!("status: {}", git_cmd(&root, &["status", "--porcelain"]));
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

    // Prove the driver is live in this environment: plain `git diff` calls it.
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
    let git = Git::new(&root).with_env(env);
    let diff = git.diff(None, usize::MAX).await.expect("diff");
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
    std::fs::write(root.join(".gitattributes"), "README.md diff=hostile\n").expect("attributes");
    git_cmd(&root, &["add", ".gitattributes"]);
    git_cmd(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "attributes",
        ],
    );
    git_cmd(
        &root,
        &[
            "config",
            "diff.hostile.textconv",
            driver.to_str().expect("utf-8"),
        ],
    );
    std::fs::write(root.join("README.md"), "changed\n").expect("write");

    // Prove the driver is live: plain `git diff` runs the textconv.
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

    let diff = runner(&root).diff(None, usize::MAX).await.expect("diff");
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
    std::fs::create_dir_all(&root).expect("checkout");
    raw_init(&root);
    git_cmd(&root, &["config", "user.name", "Repo"]);
    git_cmd(&root, &["config", "user.email", "repo@example.invalid"]);
    commit(&root, "README.md", "base\n");
    git_cmd(&root, &["checkout", "-q", "-b", "feature"]);
    commit(&root, "feature.txt", "feature\n");
    git_cmd(&root, &["checkout", "-q", "main"]);
    commit(&root, "main.txt", "main\n");
    let mut supplied = BTreeMap::new();
    supplied.insert("GIT_COMMITTER_NAME".to_owned(), "Env Committer".to_owned());
    supplied.insert(
        "GIT_COMMITTER_EMAIL".to_owned(),
        "committer@example.invalid".to_owned(),
    );
    let git = Git::new(&root).with_env(supplied);
    git.merge("feature").await.expect("merge");
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
    let error = Git::new(dir.path())
        .with_env(env())
        .status()
        .await
        .expect_err("not a repository");
    assert!(matches!(error, Error::Git { .. }), "{error}");

    let missing = dir.path().join("does-not-exist");
    let error = Git::new(&missing)
        .with_env(env())
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
        git.commit_all("wirepuller: finish").await.expect("commit"),
        None
    );
    std::fs::write(root.join("new.txt"), "new\n").expect("write");
    std::fs::write(root.join("README.md"), "changed\n").expect("write");
    let commit = git
        .commit_all("wirepuller: finish")
        .await
        .expect("commit")
        .expect("a commit");
    assert_ne!(commit, before);
    assert!(git.status().await.expect("status").is_empty());
    let head = git.head().await.expect("head");
    assert_eq!(head, commit);
    let message = git_cmd(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(message, "wirepuller: finish");
}

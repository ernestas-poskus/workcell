//! Garbage collection against real temporary repositories: dry run, protection, age, dirt,
//! merge state, and `force`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use workcell::gc::{GcPolicy, gc, under_root};
use workcell::git::Git;
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

fn git(dir: &Path, args: &[&str]) {
    let output = git_at(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    std::fs::create_dir_all(&root).expect("checkout");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@example.invalid"]);
    commit(&root, "README.md", "base\n");
    let remote = dir.path().join("remote.git");
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
    (dir, root)
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

fn add_worktree(root: &Path, branch: &str, path: &Path, unmerged: bool) {
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            path.to_str().expect("utf-8"),
        ],
    );
    if unmerged {
        commit(path, "change.txt", "change\n");
    }
}

fn skipped(report: &workcell::gc::GcReport, path: &Path) -> bool {
    report.skipped.iter().any(|(candidate, _)| {
        std::fs::canonicalize(candidate).ok() == std::fs::canonicalize(path).ok()
    })
}

#[tokio::test]
async fn gc_removes_only_safe_worktrees_and_protects_active_ones() {
    let (dir, root) = repo();
    let spec = spec(&root);

    let merged = root.join(".worktrees/merged");
    let unmerged = root.join(".worktrees/unmerged");
    let dirty = root.join(".worktrees/dirty");
    let protected = root.join(".worktrees/protected");
    let outside_prefix = root.join(".worktrees/other");
    let outside_root = dir.path().join("outside");
    add_worktree(&root, "agent/merged", &merged, false);
    add_worktree(&root, "agent/unmerged", &unmerged, true);
    add_worktree(&root, "agent/dirty", &dirty, false);
    std::fs::write(dirty.join("README.md"), "dirty\n").expect("dirty");
    add_worktree(&root, "agent/protected", &protected, false);
    add_worktree(&root, "other/prefix", &outside_prefix, false);
    add_worktree(&root, "agent/outside", &outside_root, true);

    let dry = gc(
        &spec,
        &GcPolicy {
            dry_run: true,
            ..GcPolicy::default()
        },
        std::slice::from_ref(&protected),
    )
    .await
    .expect("dry run");
    assert!(dry.removed.is_empty());
    assert_eq!(dry.planned.len(), 1, "{dry:?}");
    assert_eq!(
        std::fs::canonicalize(&dry.planned[0].path).expect("canon"),
        std::fs::canonicalize(&merged).expect("canon")
    );
    assert!(skipped(&dry, &unmerged), "{dry:?}");
    assert!(skipped(&dry, &dirty), "{dry:?}");
    assert!(skipped(&dry, &protected), "{dry:?}");
    assert!(skipped(&dry, &outside_prefix), "{dry:?}");
    assert!(
        !skipped(&dry, &outside_root),
        "a worktree outside the configured root is not considered: {dry:?}"
    );
    assert!(merged.exists() && unmerged.exists() && dirty.exists() && protected.exists());

    let normal = gc(
        &spec,
        &GcPolicy::default(),
        std::slice::from_ref(&protected),
    )
    .await
    .expect("gc");
    assert_eq!(normal.removed.len(), 1, "{normal:?}");
    assert!(!merged.exists());
    assert!(unmerged.exists() && dirty.exists() && protected.exists() && outside_prefix.exists());

    let forced = gc(
        &spec,
        &GcPolicy {
            force: true,
            ..GcPolicy::default()
        },
        std::slice::from_ref(&protected),
    )
    .await
    .expect("forced gc");
    assert_eq!(forced.removed.len(), 2, "{forced:?}");
    assert!(!unmerged.exists());
    assert!(!dirty.exists());
    assert!(
        outside_prefix.exists(),
        "a branch outside the configured prefix is never a candidate"
    );
    assert!(protected.exists(), "a protected path is never removed");
    assert!(outside_root.exists());
}

#[tokio::test]
async fn a_young_worktree_is_left_alone_until_the_age_threshold_passes() {
    let (_dir, root) = repo();
    let spec = spec(&root);
    let path = root.join(".worktrees/young");
    add_worktree(&root, "agent/young", &path, false);

    let young = gc(
        &spec,
        &GcPolicy {
            older_than_secs: Some(3_600),
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("gc");
    assert!(young.removed.is_empty(), "{young:?}");
    assert!(skipped(&young, &path));
    assert!(path.exists());

    let old_enough = gc(
        &spec,
        &GcPolicy {
            older_than_secs: Some(0),
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("gc");
    assert_eq!(old_enough.removed.len(), 1, "{old_enough:?}");
    assert!(!path.exists());
}

#[tokio::test]
async fn the_report_and_the_worktree_list_agree_on_a_real_listing() {
    let (_dir, root) = repo();
    let path = root.join(".worktrees/agent-one");
    add_worktree(&root, "agent/one", &path, false);
    let git = Git::new(&root).with_env(env());
    let entries = git.list_worktrees().await.expect("list");
    assert!(
        entries
            .iter()
            .any(|entry| entry.branch.as_deref() == Some("agent/one")),
        "{entries:?}"
    );
    assert!(under_root(&root, Path::new(".worktrees"), &path));
    for entry in entries {
        if entry.branch.as_deref() == Some("agent/one") {
            assert_eq!(
                std::fs::canonicalize(&entry.path).expect("canon"),
                std::fs::canonicalize(&path).expect("canon")
            );
        }
    }
}

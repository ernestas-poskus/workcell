//! Garbage collection against real temporary repositories: dry run, protection, age, dirt,
//! merge state, and `force`.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use workcell::gc::{GcPolicy, gc, under_root};
use workcell::git::Git;
use workcell::types::{
    BranchName, BranchPrefix, RelativePath, RemoteName, RepoRoot, Seconds, WorktreePath,
    WorktreeRoot,
};
use workcell::worktree::RepoSpec;

fn env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    support::init(&root);
    support::configure_user(&root, "t", "t@example.invalid");
    support::commit(&root, "README.md", "base\n");
    let remote = dir.path().join("remote.git");
    support::init_bare(&remote);
    support::add_remote(&root, "origin", &remote);
    support::push_branch(&root, "origin", "main");
    (dir, root)
}

fn spec(root: &Path) -> RepoSpec {
    RepoSpec {
        root: RepoRoot::new(root).expect("repository root"),
        remote: RemoteName::new("origin").expect("remote"),
        base_branch: BranchName::new("main").expect("base branch"),
        worktree_root: WorktreeRoot::new(".worktrees").expect("worktree root"),
        branch_prefix: BranchPrefix::new("agent").expect("branch prefix"),
        require_clean_root: true,
        env: env().into(),
    }
}

fn add_worktree(root: &Path, branch: &str, path: &Path, unmerged: bool) {
    support::create_branch(root, branch, "main");
    support::worktree_add(root, branch, path);
    if unmerged {
        support::commit(path, "change.txt", "change\n");
    }
}

fn skipped(report: &workcell::gc::GcReport, path: &Path) -> bool {
    report.skipped.iter().any(|(candidate, _)| {
        std::fs::canonicalize(candidate.as_path()).ok() == std::fs::canonicalize(path).ok()
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
    let protected_tree = WorktreePath::new(&protected).expect("protected path");
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
        std::slice::from_ref(&protected_tree),
    )
    .await
    .expect("dry run");
    assert!(dry.removed.is_empty());
    assert_eq!(dry.planned.len(), 1, "{dry:?}");
    assert_eq!(
        std::fs::canonicalize(dry.planned[0].path.as_path()).expect("canon"),
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
        std::slice::from_ref(&protected_tree),
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
        std::slice::from_ref(&protected_tree),
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
            older_than_secs: Some(Seconds::new(3_600)),
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
            older_than_secs: Some(Seconds::new(0)),
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
    let git = Git::new(RepoRoot::new(&root).expect("repository root")).with_env(env().into());
    let entries = git.list_worktrees().await.expect("list");
    assert!(
        entries.iter().any(|entry| entry
            .branch
            .as_ref()
            .map(workcell::types::BranchName::as_str)
            == Some("agent/one")),
        "{entries:?}"
    );
    assert!(under_root(&root, Path::new(".worktrees"), &path));
    for entry in entries {
        if entry
            .branch
            .as_ref()
            .map(workcell::types::BranchName::as_str)
            == Some("agent/one")
        {
            assert_eq!(
                std::fs::canonicalize(&entry.path).expect("canon"),
                std::fs::canonicalize(&path).expect("canon")
            );
        }
    }
}

#[tokio::test]
async fn git_tracked_status_reports_tracked_edit_but_not_untracked_file() {
    let (_dir, root) = repo();
    let git = Git::new(RepoRoot::new(&root).expect("repository root")).with_env(env().into());

    assert!(git.status().await.expect("status").is_empty());
    assert!(
        git.tracked_status()
            .await
            .expect("tracked_status")
            .is_empty()
    );

    std::fs::write(root.join("untracked.txt"), "untracked\n").expect("write untracked");
    let untracked_path = RelativePath::new("untracked.txt").expect("relative path");
    assert_eq!(
        git.status().await.expect("status"),
        vec![untracked_path.clone()]
    );
    assert!(
        git.tracked_status()
            .await
            .expect("tracked_status")
            .is_empty()
    );

    std::fs::write(root.join("README.md"), "modified base\n").expect("write tracked");
    let readme_path = RelativePath::new("README.md").expect("relative path");
    assert_eq!(
        git.tracked_status().await.expect("tracked_status"),
        vec![readme_path.clone()]
    );
    let mut all_status = git.status().await.expect("status");
    all_status.sort();
    assert_eq!(all_status, vec![readme_path, untracked_path]);
}

#[tokio::test]
async fn reap_merged_removes_clean_merged_worktrees_with_untracked_residue() {
    let (_dir, root) = repo();
    let spec = spec(&root);

    let untracked_residue = root.join(".worktrees/untracked");
    let tracked_dirty = root.join(".worktrees/tracked-dirty");
    let unmerged = root.join(".worktrees/unmerged");

    add_worktree(&root, "agent/untracked", &untracked_residue, false);
    std::fs::write(untracked_residue.join("build-residue.txt"), "residue\n")
        .expect("write residue");

    add_worktree(&root, "agent/tracked-dirty", &tracked_dirty, false);
    std::fs::write(tracked_dirty.join("README.md"), "dirty\n").expect("modify tracked");

    add_worktree(&root, "agent/unmerged", &unmerged, true);

    // Default policy dry run skips untracked_residue as "dirty worktree"
    let default_dry = gc(
        &spec,
        &GcPolicy {
            dry_run: true,
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("default dry");
    assert!(default_dry.removed.is_empty());
    assert!(default_dry.planned.is_empty());
    assert!(default_dry.skipped.iter().any(|(p, r)| {
        std::fs::canonicalize(p).ok() == std::fs::canonicalize(&untracked_residue).ok()
            && r == "dirty worktree"
    }));
    assert!(default_dry.skipped.iter().any(|(p, r)| {
        std::fs::canonicalize(p).ok() == std::fs::canonicalize(&tracked_dirty).ok()
            && r == "dirty worktree"
    }));
    assert!(default_dry.skipped.iter().any(|(p, r)| {
        std::fs::canonicalize(p).ok() == std::fs::canonicalize(&unmerged).ok()
            && r == "branch is not merged into the base"
    }));
    assert!(untracked_residue.exists() && tracked_dirty.exists() && unmerged.exists());

    // reap_merged dry run plans untracked_residue with nothing deleted
    let merged_dry = gc(
        &spec,
        &GcPolicy {
            dry_run: true,
            reap_merged: true,
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("merged dry");
    assert!(merged_dry.removed.is_empty());
    assert_eq!(merged_dry.planned.len(), 1, "{merged_dry:?}");
    assert_eq!(
        std::fs::canonicalize(merged_dry.planned[0].path.as_path()).expect("canon"),
        std::fs::canonicalize(&untracked_residue).expect("canon")
    );
    assert!(merged_dry.skipped.iter().any(|(p, r)| {
        std::fs::canonicalize(p).ok() == std::fs::canonicalize(&tracked_dirty).ok()
            && r == "tracked changes in the worktree"
    }));
    assert!(merged_dry.skipped.iter().any(|(p, r)| {
        std::fs::canonicalize(p).ok() == std::fs::canonicalize(&unmerged).ok()
            && r == "branch is not merged into the base"
    }));
    assert!(untracked_residue.exists() && tracked_dirty.exists() && unmerged.exists());

    // Real reap_merged run removes untracked_residue and its branch, but leaves tracked_dirty and unmerged
    let merged_run = gc(
        &spec,
        &GcPolicy {
            reap_merged: true,
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("merged run");
    assert_eq!(merged_run.removed.len(), 1, "{merged_run:?}");
    assert!(!untracked_residue.exists());
    assert!(tracked_dirty.exists());
    assert!(unmerged.exists());

    // Verify branch was deleted
    let git = Git::new(RepoRoot::new(&root).expect("repository root")).with_env(env().into());
    assert!(
        !git.show_ref(
            &workcell::types::ReferenceName::new("refs/heads/agent/untracked").expect("reference")
        )
        .await
        .expect("show_ref")
    );

    // reap_merged combined with force still removes the tracked-dirty and unmerged ones
    let forced_run = gc(
        &spec,
        &GcPolicy {
            reap_merged: true,
            force: true,
            ..GcPolicy::default()
        },
        &[],
    )
    .await
    .expect("forced run");
    assert_eq!(forced_run.removed.len(), 2, "{forced_run:?}");
    assert!(!tracked_dirty.exists());
    assert!(!unmerged.exists());
}

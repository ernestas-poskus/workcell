//! Conservative garbage collection for stale worktrees and branches.
//!
//! Cleanup runs after a successful integration, but an interrupted or failed unit of work can
//! leave a checkout and its branch behind. Collection here is deliberately conservative: it only
//! considers paths under the configured worktree root whose branch carries the configured prefix,
//! never touches a caller-protected path, and refuses a dirty worktree or an unmerged branch unless
//! `force` is explicit.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;
use crate::git::Git;
use crate::worktree::RepoSpec;

/// What the caller asked the collector to do.
#[derive(Debug, Clone, Default)]
pub struct GcPolicy {
    /// Report only; remove nothing.
    pub dry_run: bool,
    /// Remove dirty worktrees and unmerged branches too.
    pub force: bool,
    /// Only consider worktrees older than this many seconds.
    pub older_than_secs: Option<u64>,
}

/// One worktree the collector considered removing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcEntry {
    /// The worktree's path.
    pub path: PathBuf,
    /// Its branch.
    pub branch: String,
}

/// What happened, and what was deliberately left alone.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    /// Entries that were removed.
    pub removed: Vec<GcEntry>,
    /// Entries a dry run would have removed.
    pub planned: Vec<GcEntry>,
    /// Paths left alone, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

impl GcReport {
    /// Render the report for a human-facing command.
    #[must_use]
    pub fn render(&self, dry_run: bool) -> String {
        let mut out = String::new();
        for entry in &self.removed {
            out.push_str(&format!(
                "removed {} ({})\n",
                entry.path.display(),
                entry.branch
            ));
        }
        for entry in &self.planned {
            out.push_str(&format!(
                "would remove {} ({})\n",
                entry.path.display(),
                entry.branch
            ));
        }
        for (path, reason) in &self.skipped {
            out.push_str(&format!("skipped {}: {reason}\n", path.display()));
        }
        if self.removed.is_empty() && self.planned.is_empty() && self.skipped.is_empty() {
            out.push_str("no worktrees found\n");
        } else if dry_run {
            out.push_str(&format!("dry run: {} removable\n", self.planned.len()));
        } else {
            out.push_str(&format!("removed {} worktree(s)\n", self.removed.len()));
        }
        out
    }
}

/// Whether `path` belongs under the configured worktree root.
#[must_use]
pub fn under_root(root: &Path, configured_root: &Path, path: &Path) -> bool {
    let base = root.join(configured_root);
    path.starts_with(&base) && path != base
}

/// Collect stale worktrees and branches, never touching a protected path.
///
/// `protect` names worktrees that are still live (for example a path a caller is actively
/// editing); a protected path is reported as skipped and never removed, even with `force`.
///
/// # Errors
///
/// Returns a Git failure when the repository cannot be read or a removal fails.
pub async fn gc(spec: &RepoSpec, policy: &GcPolicy, protect: &[PathBuf]) -> Result<GcReport> {
    let git = Git::new(&spec.root).with_env(spec.env.clone());
    let root = tokio::fs::canonicalize(&spec.root)
        .await
        .unwrap_or_else(|_| spec.root.clone());
    let entries: Vec<GcEntry> = git
        .list_worktrees()
        .await?
        .into_iter()
        .filter_map(|entry| {
            entry.branch.map(|branch| GcEntry {
                path: entry.path,
                branch,
            })
        })
        .collect();

    let mut protected: Vec<PathBuf> = Vec::with_capacity(protect.len());
    for path in protect {
        protected.push(canonical_or_self(path).await);
    }

    let mut report = GcReport::default();
    let prefix = format!("{}/", spec.branch_prefix.trim_end_matches('/'));
    let base_branch = spec.base_branch.trim();
    for entry in entries {
        if !under_root(&root, spec.worktree_root(), &entry.path) {
            continue;
        }
        if !entry.branch.starts_with(&prefix) {
            report.skipped.push((
                entry.path.clone(),
                "branch prefix does not match".to_owned(),
            ));
            continue;
        }
        if protected.contains(&canonical_or_self(&entry.path).await) {
            report
                .skipped
                .push((entry.path.clone(), "protected by the caller".to_owned()));
            continue;
        }
        if let Some(seconds) = policy.older_than_secs
            && !older_than(&entry.path, seconds).await
        {
            report
                .skipped
                .push((entry.path.clone(), format!("younger than {seconds}s")));
            continue;
        }
        let worktree_git = Git::new(&entry.path).with_env(spec.env.clone());
        let dirty = !worktree_git.status().await?.is_empty();
        let merged = git.is_ancestor(&entry.branch, base_branch).await?;
        if !policy.force && (dirty || !merged) {
            let reason = if dirty {
                "dirty worktree"
            } else {
                "branch is not merged into the base"
            };
            report.skipped.push((entry.path.clone(), reason.to_owned()));
            continue;
        }
        if policy.dry_run {
            report.planned.push(entry);
            continue;
        }
        git.worktree_remove(&entry.path).await?;
        git.branch_delete(&entry.branch).await?;
        report.removed.push(entry);
    }
    let _ = git.worktree_prune().await;
    Ok(report)
}

/// The canonical form of `path`, or `path` itself when it cannot be resolved.
async fn canonical_or_self(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `path` was modified at least `seconds` ago.
async fn older_than(path: &Path, seconds: u64) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_under_the_configured_root_are_candidates() {
        let root = Path::new("/repo");
        assert!(under_root(
            root,
            Path::new(".worktrees"),
            Path::new("/repo/.worktrees/one")
        ));
        assert!(!under_root(
            root,
            Path::new(".worktrees"),
            Path::new("/repo/other/one")
        ));
        assert!(!under_root(
            root,
            Path::new(".worktrees"),
            Path::new("/elsewhere/one")
        ));
        assert!(!under_root(
            root,
            Path::new(".worktrees"),
            Path::new("/repo/.worktrees")
        ));
    }

    #[test]
    fn an_empty_report_says_so() {
        let report = GcReport::default();
        assert_eq!(report.render(false), "no worktrees found\n");
    }

    #[test]
    fn a_report_counts_what_it_did() {
        let entry = GcEntry {
            path: PathBuf::from("/repo/.worktrees/one"),
            branch: "agent/one".to_owned(),
        };
        let planned = GcReport {
            planned: vec![entry.clone()],
            ..GcReport::default()
        };
        assert_eq!(
            planned.render(true),
            "would remove /repo/.worktrees/one (agent/one)\ndry run: 1 removable\n"
        );
        let removed = GcReport {
            removed: vec![entry],
            skipped: vec![(
                PathBuf::from("/repo/.worktrees/two"),
                "dirty worktree".to_owned(),
            )],
            ..GcReport::default()
        };
        assert!(removed.render(false).contains("removed 1 worktree(s)"));
        assert!(
            removed
                .render(false)
                .contains("skipped /repo/.worktrees/two: dirty worktree")
        );
    }
}

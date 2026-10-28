//! A typed Git runner and the primitives a coding workspace needs.
//!
//! Every operation is performed in-process through `git2`/libgit2 against one working tree. That
//! is a security property, not an implementation detail: libgit2 does not execute repository
//! hooks, and it does not spawn the external diff or textconv programs a repository can name in
//! `.gitattributes`/`diff.<driver>` configuration. A repository therefore cannot execute code in
//! the caller's process tree through Git.
//!
//! The runner is confined to the working tree it was created for. Filesystem and network work run
//! on the blocking pool, never on an async worker, and each operation has a wall-clock budget
//! ([`GIT_TIMEOUT`]). A failure reports the logical operation and a bounded libgit2 message, never
//! an environment or a credential.
//!
//! One operation, `fetch`, negotiates credentials: see the `auth` submodule for the
//! ssh-agent/default-key and global-credential-helper policy. A repository-local `credential.helper` is ignored on purpose,
//! and a helper runs with the caller-supplied environment instead of the parent process.env.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use git2::{
    BranchType, Config, DiffFindOptions, DiffFormat, DiffOptions, DiffStatsFormat, ErrorCode,
    FetchOptions, FetchPrune, Oid, RebaseOperationType, RebaseOptions, RemoteCallbacks, Repository,
    ResetType, Signature, StatusOptions, WorktreeAddOptions, WorktreePruneOptions,
};
use sha2::{Digest, Sha256};
use tokio::task;

mod auth;

use crate::error::{Error, Result};

/// Default wall-clock budget for one Git operation.
///
/// For local operations this is a hard timeout: the operation runs on the blocking pool and the
/// caller returns after this long. For `fetch` it is best-effort: libgit2 reports progress through
/// a callback that aborts at the deadline, but an in-process connection that blocks before the
/// first callback cannot be killed (there is no child process to signal).
pub const GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// The most repository-configuration bytes the shared-checkout guard will hash.
const MAX_CONFIG_BYTES: usize = 1_048_576;

/// A tracked diff, bounded for a reviewer while keeping its complete path list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    /// The resolved base commit the diff compares against.
    pub base: String,
    /// The range as reported, e.g. `<base>..HEAD`.
    pub range: String,
    /// The repository stat output (libgit2's format).
    pub stat: String,
    /// The bounded patch text.
    pub patch: String,
    /// Every changed path.
    pub changed_files: Vec<String>,
    /// The original patch size when [`Diff::patch`] was truncated.
    pub truncated_from: Option<usize>,
}

/// One worktree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The worktree's path.
    pub path: PathBuf,
    /// Its checked-out branch, when one is checked out.
    pub branch: Option<String>,
}

/// The Git directories of one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirectories {
    /// This worktree's Git directory (`.git`, or `.git/worktrees/<name>` for a linked worktree).
    pub git_dir: PathBuf,
    /// The repository's common Git directory.
    pub common_dir: PathBuf,
}

/// A Git runner confined to one working tree.
#[derive(Debug, Clone)]
pub struct Git {
    root: PathBuf,
    env: BTreeMap<String, String>,
}

impl Git {
    /// A runner for `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            env: BTreeMap::new(),
        }
    }

    /// The environment supplied to commit signatures and credential helpers.
    ///
    /// There is no Git child process any more: this map is not a full environment replacement. It
    /// provides `GIT_AUTHOR_*`/`GIT_COMMITTER_*` when present, and it is the environment a
    /// credential helper runs with. `HOME`/`PATH` should be included when a caller expects the
    /// helper or the default SSH keys to resolve.
    #[must_use]
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// The working tree this runner is confined to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The current commit, when the tree is a repository with a born HEAD.
    pub async fn head(&self) -> Option<String> {
        let root = self.root.clone();
        self.run("git rev-parse HEAD", move || {
            let repo = match Repository::open(&root) {
                Ok(repo) => repo,
                Err(_) => return Ok(None),
            };
            Ok(repo
                .head()
                .ok()
                .and_then(|head| head.peel_to_commit().ok())
                .map(|commit| commit.id().to_string()))
        })
        .await
        .ok()
        .flatten()
    }

    /// The branch name `HEAD` is on, or `None` when detached or unborn.
    pub async fn branch_name(&self) -> Result<Option<String>> {
        let root = self.root.clone();
        self.run("git rev-parse --abbrev-ref HEAD", move || {
            let repo = open(&root)?;
            match repo.head() {
                Ok(head) if head.is_branch() => Ok(head.shorthand().ok().map(str::to_owned)),
                Ok(_) => Ok(None),
                Err(error)
                    if matches!(error.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) =>
                {
                    Ok(None)
                }
                Err(error) => Err(git_error("git rev-parse --abbrev-ref HEAD", error)),
            }
        })
        .await
    }

    /// Resolve a revision to a commit id.
    pub async fn rev_parse(&self, rev: &str) -> Result<String> {
        let root = self.root.clone();
        let rev = rev.to_owned();
        self.run("git rev-parse", move || {
            let repo = open(&root)?;
            Ok(resolve_commit(&repo, &rev)?.id().to_string())
        })
        .await
    }

    /// Whether `reference` exists.
    pub async fn show_ref(&self, reference: &str) -> Result<bool> {
        let root = self.root.clone();
        let reference = reference.to_owned();
        self.run("git show-ref", move || {
            let repo = open(&root)?;
            match repo.find_reference(&reference) {
                Ok(_) => Ok(true),
                Err(error) if error.code() == ErrorCode::NotFound => Ok(false),
                Err(error) => Err(git_error("git show-ref", error)),
            }
        })
        .await
    }

    /// Whether `ancestor` is an ancestor of `descendant`.
    ///
    /// A revision that does not resolve to a commit answers `false`, matching the old exit-status
    /// contract of `git merge-base --is-ancestor`.
    pub async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let root = self.root.clone();
        let ancestor = ancestor.to_owned();
        let descendant = descendant.to_owned();
        self.run("git merge-base --is-ancestor", move || {
            let repo = open(&root)?;
            let (Some(ancestor), Some(descendant)) =
                (commit_id(&repo, &ancestor), commit_id(&repo, &descendant))
            else {
                return Ok(false);
            };
            // libgit2's descendant test is strict; Git's `--is-ancestor` answers true for the
            // same commit, which is the contract callers depend on (for example an unchanged base).
            if ancestor == descendant {
                return Ok(true);
            }
            repo.graph_descendant_of(descendant, ancestor)
                .map_err(|error| git_error("git merge-base --is-ancestor", error))
        })
        .await
    }

    /// Fast-forward to `reference`; a diverged history is an error.
    pub async fn merge_ff_only(&self, reference: &str) -> Result<()> {
        let root = self.root.clone();
        let reference = reference.to_owned();
        self.run("git merge --ff-only", move || {
            let repo = open(&root)?;
            fast_forward(&repo, &reference)
        })
        .await
    }

    /// Merge `reference`, creating a merge commit for a non-fast-forward history.
    ///
    /// The merge commit message is `Merge branch '<reference>'`; there is no editor to consult.
    pub async fn merge(&self, reference: &str) -> Result<()> {
        let root = self.root.clone();
        let reference = reference.to_owned();
        let env = self.env.clone();
        self.run("git merge", move || {
            let repo = open(&root)?;
            let head = head_commit(&repo)?;
            let target = resolve_commit(&repo, &reference)?;
            if repo
                .graph_descendant_of(head.id(), target.id())
                .map_err(|error| git_error("git merge", error))?
            {
                return Ok(());
            }
            if repo
                .graph_descendant_of(target.id(), head.id())
                .map_err(|error| git_error("git merge", error))?
            {
                return fast_forward(&repo, &reference);
            }
            let annotated = repo
                .find_annotated_commit(target.id())
                .map_err(|error| git_error("git merge", error))?;
            repo.merge(&[&annotated], None, None)
                .map_err(|error| git_error("git merge", error))?;
            let mut index = repo
                .index()
                .map_err(|error| git_error("git merge", error))?;
            if index.has_conflicts() {
                return Err(Error::git(
                    "git merge",
                    format!("merging `{reference}` left conflicts"),
                ));
            }
            let tree_id = index
                .write_tree()
                .map_err(|error| git_error("git merge", error))?;
            let tree = repo
                .find_tree(tree_id)
                .map_err(|error| git_error("git merge", error))?;
            let signature = committer_signature(&repo, &env)?;
            let message = format!("Merge branch '{reference}'");
            let merged = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    &message,
                    &tree,
                    &[&head, &target],
                )
                .map_err(|error| git_error("git merge", error))?;
            let merged = repo
                .find_commit(merged)
                .map_err(|error| git_error("git merge", error))?;
            repo.reset(merged.as_object(), ResetType::Hard, None)
                .map_err(|error| git_error("git merge", error))?;
            repo.cleanup_state()
                .map_err(|error| git_error("git merge", error))?;
            Ok(())
        })
        .await
    }

    /// The merge base of two revisions.
    pub async fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        let root = self.root.clone();
        let a = a.to_owned();
        let b = b.to_owned();
        self.run("git merge-base", move || {
            let repo = open(&root)?;
            let (Some(a), Some(b)) = (commit_id(&repo, &a), commit_id(&repo, &b)) else {
                return Err(Error::git(
                    "git merge-base",
                    "one of the revisions does not resolve to a commit",
                ));
            };
            Ok(repo
                .merge_base(a, b)
                .map_err(|error| git_error("git merge-base", error))?
                .to_string())
        })
        .await
    }

    /// Rebase the current branch onto `onto`, preserving each commit's author and message.
    pub async fn rebase(&self, onto: &str) -> Result<()> {
        let root = self.root.clone();
        let onto = onto.to_owned();
        let env = self.env.clone();
        self.run("git rebase", move || {
            let repo = open(&root)?;
            let onto = resolve_commit(&repo, &onto)?;
            let onto_annotated = repo
                .find_annotated_commit(onto.id())
                .map_err(|error| git_error("git rebase", error))?;
            let mut options = RebaseOptions::new();
            let mut rebase = repo
                .rebase(
                    None,
                    Some(&onto_annotated),
                    Some(&onto_annotated),
                    Some(&mut options),
                )
                .map_err(|error| git_error("git rebase", error))?;
            let signature = committer_signature(&repo, &env)?;
            loop {
                let exec = {
                    let operation = match rebase.next() {
                        Some(Ok(operation)) => operation,
                        Some(Err(error)) => return Err(git_error("git rebase", error)),
                        None => break,
                    };
                    matches!(operation.kind(), Some(RebaseOperationType::Exec))
                };
                if exec {
                    return Err(Error::git(
                        "git rebase",
                        "an exec operation in the rebase plan is not supported",
                    ));
                }
                rebase
                    .commit(None, &signature, None)
                    .map_err(|error| git_error("git rebase", error))?;
            }
            rebase
                .finish(Some(&signature))
                .map_err(|error| git_error("git rebase", error))
        })
        .await
    }

    /// Abort an in-progress rebase.
    pub async fn rebase_abort(&self) -> Result<()> {
        let root = self.root.clone();
        self.run("git rebase --abort", move || {
            let repo = open(&root)?;
            let mut rebase = repo
                .open_rebase(None)
                .map_err(|error| git_error("git rebase --abort", error))?;
            rebase
                .abort()
                .map_err(|error| git_error("git rebase --abort", error))
        })
        .await
    }

    /// Reset the working tree and index to `rev`.
    pub async fn reset_hard(&self, rev: &str) -> Result<()> {
        let root = self.root.clone();
        let rev = rev.to_owned();
        self.run("git reset --hard", move || {
            let repo = open(&root)?;
            let commit = resolve_commit(&repo, &rev)?;
            repo.reset(commit.as_object(), ResetType::Hard, None)
                .map_err(|error| git_error("git reset --hard", error))
        })
        .await
    }

    /// Compare-and-swap a reference from `old` to `new`.
    ///
    /// The reference is read under libgit2's ref lock; a stale `old` is an error, not a force.
    pub async fn update_ref(&self, reference: &str, new: &str, old: &str) -> Result<()> {
        let root = self.root.clone();
        let env = self.env.clone();
        let reference = reference.to_owned();
        let new = new.to_owned();
        let old = old.to_owned();
        self.run("git update-ref", move || {
            let repo = open(&root)?;
            let new = resolve_commit(&repo, &new)?.id();
            let expected = expected_old(&repo, &old)?;
            let signature = committer_signature(&repo, &env)?;
            let mut transaction = repo
                .transaction()
                .map_err(|error| git_error("git update-ref", error))?;
            transaction
                .lock_ref(&reference)
                .map_err(|error| git_error("git update-ref", error))?;
            let current = repo
                .find_reference(&reference)
                .ok()
                .and_then(|reference| reference.target());
            if current != expected {
                let current = current.map_or_else(|| "absent".to_owned(), |oid| oid.to_string());
                return Err(Error::git(
                    "git update-ref",
                    format!("`{reference}` is at {current}, not the expected `{old}`"),
                ));
            }
            transaction
                .set_target(&reference, new, Some(&signature), "update by workcell")
                .map_err(|error| git_error("git update-ref", error))?;
            transaction
                .commit()
                .map_err(|error| git_error("git update-ref", error))
        })
        .await
    }

    /// Fetch `remote`, pruning stale remote-tracking refs.
    pub async fn fetch(&self, remote: &str) -> Result<()> {
        let root = self.root.clone();
        let env = self.env.clone();
        let remote = remote.to_owned();
        self.run("git fetch", move || {
            let repo = open(&root)?;
            let mut remote = repo
                .find_remote(&remote)
                .map_err(|error| git_error("git fetch", error))?;
            let deadline = Instant::now() + GIT_TIMEOUT;
            let timed_out = Arc::new(AtomicBool::new(false));
            let timed_out_callback = Arc::clone(&timed_out);
            let mut callbacks = RemoteCallbacks::new();
            callbacks.transfer_progress(move |_progress| {
                if Instant::now() >= deadline {
                    timed_out_callback.store(true, Ordering::SeqCst);
                    false
                } else {
                    true
                }
            });
            let credential_env = env.clone();
            let credential_root = root.clone();
            callbacks.credentials(move |url, username, allowed| {
                auth::credentials(&credential_env, &credential_root, url, username, allowed)
            });
            let mut options = FetchOptions::new();
            options.remote_callbacks(callbacks).prune(FetchPrune::On);
            let refspecs: &[&str] = &[];
            remote
                .fetch(refspecs, Some(&mut options), None)
                .map_err(|error| {
                    if timed_out.load(Ordering::SeqCst) {
                        Error::git(
                            "git fetch",
                            format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
                        )
                    } else {
                        git_error("git fetch", error)
                    }
                })
        })
        .await
    }

    /// Every worktree Git knows about: the main working tree first, then linked worktrees.
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeEntry>> {
        let root = self.root.clone();
        self.run("git worktree list", move || {
            let repo = open(&root)?;
            let mut entries = Vec::new();
            if let Some(workdir) = repo.workdir() {
                let branch = repo
                    .head()
                    .ok()
                    .filter(|head| head.is_branch())
                    .and_then(|head| head.shorthand().ok().map(str::to_owned));
                entries.push(WorktreeEntry {
                    path: canonical(workdir),
                    branch,
                });
            }
            let names = worktree_names(&repo, "git worktree list")?;
            for name in names {
                let worktree = repo
                    .find_worktree(&name)
                    .map_err(|error| git_error("git worktree list", error))?;
                let branch =
                    Repository::open_from_worktree(&worktree)
                        .ok()
                        .and_then(|repository| {
                            let head = repository.head().ok()?;
                            if head.is_branch() {
                                head.shorthand().ok().map(str::to_owned)
                            } else {
                                None
                            }
                        });
                entries.push(WorktreeEntry {
                    path: canonical(worktree.path()),
                    branch,
                });
            }
            Ok(entries)
        })
        .await
    }

    /// Create a worktree at `path` on a new `branch` cut from `base`.
    pub async fn worktree_add(&self, branch: &str, path: &Path, base: &str) -> Result<()> {
        let root = self.root.clone();
        let branch = branch.to_owned();
        let path = path.to_path_buf();
        let base = base.to_owned();
        self.run("git worktree add", move || {
            let repo = open(&root)?;
            if repo.find_branch(&branch, BranchType::Local).is_ok() {
                return Err(Error::Path {
                    detail: format!("worktree collision: branch `{branch}` already exists"),
                });
            }
            let base_commit = resolve_commit(&repo, &base)?;
            let branch_ref = repo
                .branch(&branch, &base_commit, false)
                .map_err(|error| git_error("git worktree add", error))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::Path {
                    detail: format!(
                        "the worktree path `{}` has no usable final component",
                        path.display()
                    ),
                })?
                .to_owned();
            let reference = branch_ref.into_reference();
            let mut options = WorktreeAddOptions::new();
            options.reference(Some(&reference));
            match repo.worktree(&name, &path, Some(&options)) {
                Ok(_) => Ok(()),
                Err(error) => {
                    if let Ok(mut branch) = repo.find_branch(&branch, BranchType::Local) {
                        let _ = branch.delete();
                    }
                    Err(git_error("git worktree add", error))
                }
            }
        })
        .await
    }

    /// Remove the worktree at `path`, force-removing a dirty one.
    pub async fn worktree_remove(&self, path: &Path) -> Result<()> {
        let root = self.root.clone();
        let path = path.to_path_buf();
        self.run("git worktree remove", move || {
            let repo = open(&root)?;
            let target = canonical(&path);
            let names = worktree_names(&repo, "git worktree remove")?;
            for name in names {
                let worktree = repo
                    .find_worktree(&name)
                    .map_err(|error| git_error("git worktree remove", error))?;
                if canonical(worktree.path()) == target {
                    let mut options = WorktreePruneOptions::new();
                    options.valid(true).working_tree(true);
                    worktree
                        .prune(Some(&mut options))
                        .map_err(|error| git_error("git worktree remove", error))?;
                    return Ok(());
                }
            }
            if path.exists() {
                std::fs::remove_dir_all(&path)
                    .map_err(|error| Error::io(&path.display().to_string(), error))?;
            }
            Ok(())
        })
        .await
    }

    /// Prune stale worktree metadata.
    pub async fn worktree_prune(&self) -> Result<()> {
        let root = self.root.clone();
        self.run("git worktree prune", move || {
            let repo = open(&root)?;
            let names = worktree_names(&repo, "git worktree prune")?;
            for name in names {
                if let Ok(worktree) = repo.find_worktree(&name) {
                    let _ = worktree.prune(None);
                }
            }
            Ok(())
        })
        .await
    }

    /// Delete a branch even when it is not merged.
    pub async fn branch_delete(&self, branch: &str) -> Result<()> {
        let root = self.root.clone();
        let branch = branch.to_owned();
        self.run("git branch -D", move || {
            let repo = open(&root)?;
            let mut branch = repo
                .find_branch(&branch, BranchType::Local)
                .map_err(|error| git_error("git branch -D", error))?;
            branch
                .delete()
                .map_err(|error| git_error("git branch -D", error))
        })
        .await
    }

    /// The changed paths from `git status --porcelain` (plus untracked files and renames).
    pub async fn status(&self) -> Result<Vec<PathBuf>> {
        let root = self.root.clone();
        self.run("git status --porcelain", move || {
            let repo = open(&root)?;
            let mut options = StatusOptions::new();
            options
                .include_untracked(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true);
            let statuses = repo
                .statuses(Some(&mut options))
                .map_err(|error| git_error("git status --porcelain", error))?;
            Ok(statuses
                .iter()
                .map(|entry| path_from_bytes(entry.path_bytes()))
                .collect())
        })
        .await
    }

    /// A tracked diff against `base` (default `HEAD`), with the patch bounded to `max_patch_bytes`.
    pub async fn diff(&self, base: Option<&str>, max_patch_bytes: usize) -> Result<Diff> {
        let root = self.root.clone();
        let base = base.map(str::to_owned);
        self.run("git diff", move || {
            let repo = open(&root)?;
            let base_commit = match base.as_deref() {
                Some(base) => resolve_commit(&repo, base)?,
                None => head_commit(&repo)?,
            };
            let base_tree = base_commit
                .tree()
                .map_err(|error| git_error("git diff", error))?;
            let mut options = DiffOptions::new();
            let mut diff = repo
                .diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut options))
                .map_err(|error| git_error("git diff", error))?;
            let mut find = DiffFindOptions::new();
            find.renames(true);
            diff.find_similar(Some(&mut find))
                .map_err(|error| git_error("git diff", error))?;
            let stats = diff.stats().map_err(|error| git_error("git diff", error))?;
            let stat = if stats.files_changed() == 0 {
                String::new()
            } else {
                String::from_utf8_lossy(
                    &stats
                        .to_buf(DiffStatsFormat::FULL, 80)
                        .map_err(|error| git_error("git diff", error))?,
                )
                .into_owned()
            };
            let mut patch: Vec<u8> = Vec::new();
            diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
                // `DiffLine::content` omits the `+`/`-`/space origin column that a unified patch
                // carries; add it back for content lines.
                if matches!(line.origin(), '+' | '-' | ' ') {
                    patch.push(line.origin() as u8);
                }
                patch.extend_from_slice(line.content());
                true
            })
            .map_err(|error| git_error("git diff", error))?;
            let mut patch = String::from_utf8_lossy(&patch).into_owned();
            let truncated_from = (patch.len() > max_patch_bytes).then_some(patch.len());
            if truncated_from.is_some() {
                let mut end = max_patch_bytes.min(patch.len());
                while end > 0 && !patch.is_char_boundary(end) {
                    end -= 1;
                }
                patch.truncate(end);
            }
            let mut changed_files = Vec::new();
            for delta in diff.deltas() {
                let file = delta.new_file();
                let Some(path) = file.path_bytes() else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                changed_files.push(String::from_utf8_lossy(path).into_owned());
            }
            let base = base_commit.id().to_string();
            Ok(Diff {
                range: format!("{base} -> working tree"),
                base,
                stat: if stat.is_empty() {
                    "no changes between base and working tree".to_owned()
                } else {
                    stat
                },
                patch,
                changed_files,
                truncated_from,
            })
        })
        .await
    }

    /// The paths changed between two commits.
    pub async fn changed_files_between(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let root = self.root.clone();
        let base = base.to_owned();
        let head = head.to_owned();
        self.run("git diff --name-only", move || {
            let repo = open(&root)?;
            let base = resolve_commit(&repo, &base)?;
            let head = resolve_commit(&repo, &head)?;
            let base_tree = base
                .tree()
                .map_err(|error| git_error("git diff --name-only", error))?;
            let head_tree = head
                .tree()
                .map_err(|error| git_error("git diff --name-only", error))?;
            let mut options = DiffOptions::new();
            let mut diff = repo
                .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut options))
                .map_err(|error| git_error("git diff --name-only", error))?;
            let mut find = DiffFindOptions::new();
            find.renames(true);
            diff.find_similar(Some(&mut find))
                .map_err(|error| git_error("git diff --name-only", error))?;
            let mut paths = Vec::new();
            for delta in diff.deltas() {
                let file = delta.new_file();
                let Some(path) = file.path_bytes() else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                paths.push(String::from_utf8_lossy(path).into_owned());
            }
            paths.sort_unstable();
            paths.dedup();
            Ok(paths)
        })
        .await
    }

    /// The number of commits between `base` and `head`.
    pub async fn commit_count_between(&self, base: &str, head: &str) -> Result<u64> {
        let root = self.root.clone();
        let base = base.to_owned();
        let head = head.to_owned();
        self.run("git rev-list --count", move || {
            let repo = open(&root)?;
            let base = resolve_commit(&repo, &base)?.id();
            let head = resolve_commit(&repo, &head)?.id();
            let mut walk = repo
                .revwalk()
                .map_err(|error| git_error("git rev-list --count", error))?;
            walk.push(head)
                .map_err(|error| git_error("git rev-list --count", error))?;
            walk.hide(base)
                .map_err(|error| git_error("git rev-list --count", error))?;
            let mut count = 0u64;
            for oid in walk {
                oid.map_err(|error| git_error("git rev-list --count", error))?;
                count = count.saturating_add(1);
            }
            Ok(count)
        })
        .await
    }

    /// This checkout's Git directory and the repository's common directory.
    pub async fn git_directories(&self) -> Result<GitDirectories> {
        let root = self.root.clone();
        self.run(
            "git rev-parse --absolute-git-dir --git-common-dir",
            move || {
                let repo = open(&root)?;
                Ok(GitDirectories {
                    git_dir: canonical(repo.path()),
                    common_dir: canonical(repo.commondir()),
                })
            },
        )
        .await
    }

    /// The value of one repository-local configuration key, or `None` when it is unset.
    ///
    /// Used by the shared-checkout guard to find a configured hooks directory, so the surface it
    /// hashes is the one Git would execute rather than only the default.
    pub async fn config_get(&self, key: &'static str) -> Result<Option<String>> {
        let root = self.root.clone();
        self.run("git config --get", move || {
            let repo = open(&root)?;
            let config = repo
                .config()
                .map_err(|error| git_error("git config --get", error))?;
            match config.get_string(key) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.code() == ErrorCode::NotFound => Ok(None),
                Err(error) => Err(git_error("git config --get", error)),
            }
        })
        .await
    }

    /// A digest of the repository-local configuration, includes honored.
    ///
    /// The caller uses this as the shared-checkout guard's configuration surface: any change to the
    /// local config, an included file, or the linked worktree's `config.worktree` changes the
    /// digest.
    pub async fn local_config_digest(&self) -> Result<String> {
        let root = self.root.clone();
        self.run("git config --local --list --includes", move || {
            let repo = open(&root)?;
            let mut files = vec![repo.commondir().join("config")];
            let worktree_config = repo.path().join("config.worktree");
            if worktree_config.is_file() {
                files.push(worktree_config);
            }
            let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
            let mut total = 0usize;
            for file in files {
                let config = match Config::open(&file) {
                    Ok(config) => config,
                    Err(error) if error.code() == ErrorCode::NotFound => continue,
                    Err(error) => {
                        return Err(git_error("git config --local --list --includes", error));
                    }
                };
                let mut config_entries = config
                    .entries(None)
                    .map_err(|error| git_error("git config --local --list --includes", error))?;
                while let Some(entry) = config_entries.next() {
                    let entry =
                        entry.map_err(|error| git_error("git config --local --list --includes", error))?;
                    let name = entry.name_bytes().to_vec();
                    let value = entry.value_bytes().to_vec();
                    total = total.saturating_add(name.len() + value.len() + 16);
                    if total > MAX_CONFIG_BYTES {
                        return Err(Error::git(
                            "git config --local --list --includes",
                            format!(
                                "the repository configuration is larger than {MAX_CONFIG_BYTES} bytes"
                            ),
                        ));
                    }
                    entries.push((name, value));
                }
            }
            entries.sort();
            let mut hasher = Sha256::new();
            for (name, value) in entries {
                frame(&mut hasher, &name);
                frame(&mut hasher, &value);
            }
            let mut digest = String::with_capacity(64);
            for byte in hasher.finalize() {
                digest.push_str(&format!("{byte:02x}"));
            }
            Ok(digest)
        })
        .await
    }

    /// Run one blocking Git operation off the async worker under [`GIT_TIMEOUT`].
    async fn run<T, F>(&self, operation: &'static str, operation_fn: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        match tokio::time::timeout(GIT_TIMEOUT, task::spawn_blocking(operation_fn)).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(Error::Process {
                detail: format!("{operation} could not run: {error}"),
            }),
            Err(_) => Err(Error::git(
                operation,
                format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
            )),
        }
    }
}

/// The names of every linked worktree.
fn worktree_names(repo: &Repository, operation: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for name in repo
        .worktrees()
        .map_err(|error| git_error(operation, error))?
        .iter()
    {
        if let Some(name) = name.map_err(|error| git_error(operation, error))? {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Fast-forward the current branch to `reference`.
fn fast_forward(repo: &Repository, reference: &str) -> Result<()> {
    let head = head_commit(repo)?;
    let target = resolve_commit(repo, reference)?;
    if target.id() == head.id() {
        return Ok(());
    }
    if !repo
        .graph_descendant_of(target.id(), head.id())
        .map_err(|error| git_error("git merge --ff-only", error))?
    {
        return Err(Error::git(
            "git merge --ff-only",
            format!("`{reference}` is not a fast-forward of HEAD"),
        ));
    }
    repo.reset(target.as_object(), ResetType::Hard, None)
        .map_err(|error| git_error("git merge --ff-only", error))
}

/// The committer signature from the supplied environment, then the repository config.
fn committer_signature(
    repo: &Repository,
    env: &BTreeMap<String, String>,
) -> Result<Signature<'static>> {
    if let (Some(name), Some(email)) = (
        env.get("GIT_COMMITTER_NAME"),
        env.get("GIT_COMMITTER_EMAIL"),
    ) {
        Signature::now(name, email).map_err(|error| git_error("git signature", error))
    } else {
        repo.signature()
            .map_err(|error| git_error("git signature", error))
    }
}

/// The expected old target of a compare-and-swap update, where an all-zero id means "absent".
fn expected_old(repo: &Repository, old: &str) -> Result<Option<Oid>> {
    if old.is_empty() || old.bytes().all(|byte| byte == b'0') {
        return Ok(None);
    }
    Ok(Some(resolve_commit(repo, old)?.id()))
}

/// Resolve one revision to a commit.
fn resolve_commit<'repo>(repo: &'repo Repository, rev: &str) -> Result<git2::Commit<'repo>> {
    repo.revparse_single(rev)
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| git_error("git rev-parse", error))
}

/// Resolve one revision to a commit id, or `None`.
fn commit_id(repo: &Repository, rev: &str) -> Option<Oid> {
    resolve_commit(repo, rev).ok().map(|commit| commit.id())
}

/// The current commit.
fn head_commit<'repo>(repo: &'repo Repository) -> Result<git2::Commit<'repo>> {
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| git_error("git rev-parse HEAD", error))
}

/// Open the repository rooted at `root`.
fn open(root: &Path) -> Result<Repository> {
    if !root.is_dir() {
        return Err(Error::Process {
            detail: format!(
                "the working tree `{}` does not exist or is not a directory",
                root.display()
            ),
        });
    }
    Repository::open(root).map_err(|error| git_error("git open", error))
}

/// Canonicalize a path, falling back to the path itself when it cannot be resolved.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A path from the raw bytes libgit2 reports (lossless on Unix).
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Hash one length-delimited part.
fn frame(hasher: &mut Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

/// Wrap a libgit2 error with the logical operation, never an environment or a credential.
fn git_error(operation: &str, error: git2::Error) -> Error {
    Error::git(operation, error.message().to_owned())
}

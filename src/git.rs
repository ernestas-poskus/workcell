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
//!
//! The public surface speaks the validated primitives in [`crate::types`]: paths, branch and
//! reference names, revisions, commit ids, digests, and operation labels. The conversion to the
//! untyped values libgit2 takes happens here, at the `git2` boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use git2::{
    BranchType, Config, DiffFindOptions, DiffFormat, DiffOptions, DiffStatsFormat, ErrorCode,
    FetchOptions, FetchPrune, IndexAddOption, Oid, RebaseOperationType, RebaseOptions,
    RemoteCallbacks, Repository, ResetType, Signature, StatusOptions, WorktreeAddOptions,
    WorktreePruneOptions,
};
use sha2::{Digest as Sha256Digest, Sha256};
use tokio::task;

mod auth;

use crate::error::{Error, Result};
use crate::types::{
    BranchName, BranchPrefix, ByteLimit, ByteSize, CommitId, Digest, Env, GitCommonDirectory,
    GitDirectory, GitOperation, ReferenceName, RelativePath, RemoteName, RepoRoot, Revision,
    RevisionRange, WorktreePath,
};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// The resolved base commit the diff compares against.
    pub base: CommitId,
    /// The range as reported, e.g. `<base>..HEAD`.
    pub range: RevisionRange,
    /// The repository stat output (libgit2's format).
    pub stat: String,
    /// The bounded patch text.
    pub patch: String,
    /// Every changed path.
    pub changed_files: Vec<RelativePath>,
    /// The original patch size when [`Diff::patch`] was truncated.
    pub truncated_from: Option<ByteSize>,
}

/// One worktree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The worktree's path.
    pub path: WorktreePath,
    /// Its checked-out branch, when one is checked out.
    pub branch: Option<BranchName>,
}

/// The Git directories of one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirectories {
    /// This worktree's Git directory (`.git`, or `.git/worktrees/<name>` for a linked worktree).
    pub git_dir: GitDirectory,
    /// The repository's common Git directory.
    pub common_dir: GitCommonDirectory,
}

/// A Git runner confined to one working tree.
#[derive(Debug, Clone)]
pub struct Git {
    root: RepoRoot,
    env: Env,
}

impl Git {
    /// A runner for `root`.
    #[must_use]
    pub fn new(root: RepoRoot) -> Self {
        Self {
            root,
            env: Env::empty(),
        }
    }

    /// The environment supplied to commit signatures and credential helpers.
    ///
    /// There is no Git child process: this map is not a full environment replacement. It provides
    /// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` when present, and it is the environment a credential
    /// helper runs with. `HOME`/`PATH` should be included when a caller expects the helper or the
    /// default SSH keys to resolve.
    #[must_use]
    pub fn with_env(mut self, env: Env) -> Self {
        self.env = env;
        self
    }

    /// The working tree this runner is confined to.
    #[must_use]
    pub fn root(&self) -> &RepoRoot {
        &self.root
    }

    /// The current commit, when the tree is a repository with a born HEAD.
    pub async fn head(&self) -> Option<CommitId> {
        let root = self.root.clone();
        self.run(GitOperation::Head, move || {
            let repo = match Repository::open(root.as_path()) {
                Ok(repo) => repo,
                Err(_) => return Ok(None),
            };
            match repo.head().ok().and_then(|head| head.peel_to_commit().ok()) {
                Some(commit) => typed_commit_id(commit.id()).map(Some),
                None => Ok(None),
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// The branch name `HEAD` is on, or `None` when detached or unborn.
    pub async fn branch_name(&self) -> Result<Option<BranchName>> {
        let root = self.root.clone();
        self.run(GitOperation::BranchName, move || {
            let repo = open(root.as_path())?;
            match repo.head() {
                Ok(head) if head.is_branch() => match head.shorthand() {
                    Ok(name) => Ok(Some(BranchName::new(name)?)),
                    // A non-UTF-8 shorthand has no typed name; it is reported as detached, which
                    // is the pre-typing answer for a branch this runner cannot name.
                    Err(_) => Ok(None),
                },
                Ok(_) => Ok(None),
                Err(error)
                    if matches!(error.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) =>
                {
                    Ok(None)
                }
                Err(error) => Err(git_error(GitOperation::BranchName, error)),
            }
        })
        .await
    }

    /// Resolve a revision to a commit id.
    pub async fn rev_parse(&self, rev: &Revision) -> Result<CommitId> {
        let root = self.root.clone();
        let rev = rev.clone();
        self.run(GitOperation::RevParse, move || {
            let repo = open(root.as_path())?;
            typed_commit_id(resolve_commit(&repo, rev.as_str())?.id())
        })
        .await
    }

    /// Whether `reference` exists.
    pub async fn show_ref(&self, reference: &ReferenceName) -> Result<bool> {
        let root = self.root.clone();
        let reference = reference.clone();
        self.run(GitOperation::ShowRef, move || {
            let repo = open(root.as_path())?;
            match repo.find_reference(reference.as_str()) {
                Ok(_) => Ok(true),
                Err(error) if error.code() == ErrorCode::NotFound => Ok(false),
                Err(error) => Err(git_error(GitOperation::ShowRef, error)),
            }
        })
        .await
    }

    /// Whether `ancestor` is an ancestor of `descendant`.
    ///
    /// A revision that does not resolve to a commit answers `false`, matching the old exit-status
    /// contract of `git merge-base --is-ancestor`.
    pub async fn is_ancestor(&self, ancestor: &Revision, descendant: &Revision) -> Result<bool> {
        let root = self.root.clone();
        let ancestor = ancestor.clone();
        let descendant = descendant.clone();
        self.run(GitOperation::IsAncestor, move || {
            let repo = open(root.as_path())?;
            let (Some(ancestor), Some(descendant)) = (
                commit_id(&repo, ancestor.as_str()),
                commit_id(&repo, descendant.as_str()),
            ) else {
                return Ok(false);
            };
            // libgit2's descendant test is strict; Git's `--is-ancestor` answers true for the
            // same commit, which is the contract callers depend on (for example an unchanged base).
            if ancestor == descendant {
                return Ok(true);
            }
            repo.graph_descendant_of(descendant, ancestor)
                .map_err(|error| git_error(GitOperation::IsAncestor, error))
        })
        .await
    }

    /// Fast-forward to `reference`; a diverged history is an error.
    pub async fn merge_ff_only(&self, reference: &BranchName) -> Result<()> {
        let root = self.root.clone();
        let reference = reference.clone();
        self.run(GitOperation::MergeFfOnly, move || {
            let repo = open(root.as_path())?;
            fast_forward(&repo, reference.as_str())
        })
        .await
    }

    /// Merge `reference`, creating a merge commit for a non-fast-forward history.
    ///
    /// The merge commit message is `Merge branch '<reference>'`; there is no editor to consult.
    pub async fn merge(&self, reference: &BranchName) -> Result<()> {
        let root = self.root.clone();
        let reference = reference.clone();
        let env = self.env.clone();
        self.run(GitOperation::Merge, move || {
            let repo = open(root.as_path())?;
            let head = head_commit(&repo)?;
            let target = resolve_commit(&repo, reference.as_str())?;
            if repo
                .graph_descendant_of(head.id(), target.id())
                .map_err(|error| git_error(GitOperation::Merge, error))?
            {
                return Ok(());
            }
            if repo
                .graph_descendant_of(target.id(), head.id())
                .map_err(|error| git_error(GitOperation::Merge, error))?
            {
                return fast_forward(&repo, reference.as_str());
            }
            let annotated = repo
                .find_annotated_commit(target.id())
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            repo.merge(&[&annotated], None, None)
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            let mut index = repo
                .index()
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            if index.has_conflicts() {
                return Err(Error::git(
                    GitOperation::Merge,
                    format!("merging `{reference}` left conflicts"),
                ));
            }
            let tree_id = index
                .write_tree()
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            let tree = repo
                .find_tree(tree_id)
                .map_err(|error| git_error(GitOperation::Merge, error))?;
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
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            let merged = repo
                .find_commit(merged)
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            repo.reset(merged.as_object(), ResetType::Hard, None)
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            repo.cleanup_state()
                .map_err(|error| git_error(GitOperation::Merge, error))?;
            Ok(())
        })
        .await
    }

    /// The merge base of two revisions.
    pub async fn merge_base(&self, a: &Revision, b: &Revision) -> Result<CommitId> {
        let root = self.root.clone();
        let a = a.clone();
        let b = b.clone();
        self.run(GitOperation::MergeBase, move || {
            let repo = open(root.as_path())?;
            let (Some(a), Some(b)) = (commit_id(&repo, a.as_str()), commit_id(&repo, b.as_str()))
            else {
                return Err(Error::git(
                    GitOperation::MergeBase,
                    "one of the revisions does not resolve to a commit",
                ));
            };
            let base = repo
                .merge_base(a, b)
                .map_err(|error| git_error(GitOperation::MergeBase, error))?;
            typed_commit_id(base)
        })
        .await
    }

    /// Rebase the current branch onto `onto`, preserving each commit's author and message.
    pub async fn rebase(&self, onto: &Revision) -> Result<()> {
        let root = self.root.clone();
        let onto = onto.clone();
        let env = self.env.clone();
        self.run(GitOperation::Rebase, move || {
            let repo = open(root.as_path())?;
            let onto = resolve_commit(&repo, onto.as_str())?;
            let onto_annotated = repo
                .find_annotated_commit(onto.id())
                .map_err(|error| git_error(GitOperation::Rebase, error))?;
            let mut options = RebaseOptions::new();
            let mut rebase = repo
                .rebase(
                    None,
                    Some(&onto_annotated),
                    Some(&onto_annotated),
                    Some(&mut options),
                )
                .map_err(|error| git_error(GitOperation::Rebase, error))?;
            let signature = committer_signature(&repo, &env)?;
            loop {
                let exec = {
                    let operation = match rebase.next() {
                        Some(Ok(operation)) => operation,
                        Some(Err(error)) => return Err(git_error(GitOperation::Rebase, error)),
                        None => break,
                    };
                    matches!(operation.kind(), Some(RebaseOperationType::Exec))
                };
                if exec {
                    return Err(Error::git(
                        GitOperation::Rebase,
                        "an exec operation in the rebase plan is not supported",
                    ));
                }
                rebase
                    .commit(None, &signature, None)
                    .map_err(|error| git_error(GitOperation::Rebase, error))?;
            }
            rebase
                .finish(Some(&signature))
                .map_err(|error| git_error(GitOperation::Rebase, error))
        })
        .await
    }

    /// Abort an in-progress rebase.
    pub async fn rebase_abort(&self) -> Result<()> {
        let root = self.root.clone();
        self.run(GitOperation::RebaseAbort, move || {
            let repo = open(root.as_path())?;
            let mut rebase = repo
                .open_rebase(None)
                .map_err(|error| git_error(GitOperation::RebaseAbort, error))?;
            rebase
                .abort()
                .map_err(|error| git_error(GitOperation::RebaseAbort, error))
        })
        .await
    }

    /// Reset the working tree and index to `rev`.
    pub async fn reset_hard(&self, rev: &Revision) -> Result<()> {
        let root = self.root.clone();
        let rev = rev.clone();
        self.run(GitOperation::ResetHard, move || {
            let repo = open(root.as_path())?;
            let commit = resolve_commit(&repo, rev.as_str())?;
            repo.reset(commit.as_object(), ResetType::Hard, None)
                .map_err(|error| git_error(GitOperation::ResetHard, error))
        })
        .await
    }

    /// Compare-and-swap a reference from `old` to `new`.
    ///
    /// The reference is read under libgit2's ref lock; a stale `old` is an error, not a force.
    pub async fn update_ref(
        &self,
        reference: &ReferenceName,
        new: &CommitId,
        old: &CommitId,
    ) -> Result<()> {
        let root = self.root.clone();
        let env = self.env.clone();
        let reference = reference.clone();
        let new = new.clone();
        let old = old.clone();
        self.run(GitOperation::UpdateRef, move || {
            let repo = open(root.as_path())?;
            let new = Oid::from_str(new.as_str())
                .map_err(|error| git_error(GitOperation::UpdateRef, error))?;
            let expected = expected_old(&old)?;
            let signature = committer_signature(&repo, &env)?;
            let mut transaction = repo
                .transaction()
                .map_err(|error| git_error(GitOperation::UpdateRef, error))?;
            transaction
                .lock_ref(reference.as_str())
                .map_err(|error| git_error(GitOperation::UpdateRef, error))?;
            let current = repo
                .find_reference(reference.as_str())
                .ok()
                .and_then(|reference| reference.target());
            if current != expected {
                let current = current.map_or_else(|| "absent".to_owned(), |oid| oid.to_string());
                return Err(Error::git(
                    GitOperation::UpdateRef,
                    format!("`{reference}` is at {current}, not the expected `{old}`"),
                ));
            }
            transaction
                .set_target(
                    reference.as_str(),
                    new,
                    Some(&signature),
                    "update by workcell",
                )
                .map_err(|error| git_error(GitOperation::UpdateRef, error))?;
            transaction
                .commit()
                .map_err(|error| git_error(GitOperation::UpdateRef, error))
        })
        .await
    }

    /// Fetch `remote`, pruning stale remote-tracking refs.
    pub async fn fetch(&self, remote: &RemoteName) -> Result<()> {
        let root = self.root.clone();
        let env = self.env.clone();
        let remote = remote.clone();
        self.run(GitOperation::Fetch, move || {
            let repo = open(root.as_path())?;
            let mut remote = repo
                .find_remote(remote.as_str())
                .map_err(|error| git_error(GitOperation::Fetch, error))?;
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
            let credential_env = env.to_strings();
            let credential_root = root.as_path().to_path_buf();
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
                            GitOperation::Fetch,
                            format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
                        )
                    } else {
                        git_error(GitOperation::Fetch, error)
                    }
                })
        })
        .await
    }

    /// Every worktree Git knows about: the main working tree first, then linked worktrees.
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeEntry>> {
        let root = self.root.clone();
        self.run(GitOperation::ListWorktrees, move || {
            let repo = open(root.as_path())?;
            let mut entries = Vec::new();
            if let Some(workdir) = repo.workdir() {
                let branch = repo
                    .head()
                    .ok()
                    .filter(|head| head.is_branch())
                    .and_then(|head| head.shorthand().ok().map(str::to_owned));
                entries.push(WorktreeEntry {
                    path: WorktreePath::new(canonical(workdir))?,
                    branch: branch.map(BranchName::new).transpose()?,
                });
            }
            let names = worktree_names(&repo, GitOperation::ListWorktrees)?;
            for name in names {
                let worktree = repo
                    .find_worktree(&name)
                    .map_err(|error| git_error(GitOperation::ListWorktrees, error))?;
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
                    path: WorktreePath::new(canonical(worktree.path()))?,
                    branch: branch.map(BranchName::new).transpose()?,
                });
            }
            Ok(entries)
        })
        .await
    }

    /// Every local branch whose name begins with `prefix`, sorted.
    ///
    /// The prefix is matched as the validated text it carries (a trailing `/` is not significant:
    /// [`BranchPrefix`] removes it). Non-UTF-8 branch names are skipped.
    pub async fn branches_with_prefix(&self, prefix: &BranchPrefix) -> Result<Vec<BranchName>> {
        let root = self.root.clone();
        let prefix = prefix.clone();
        self.run(GitOperation::BranchesWithPrefix, move || {
            let repo = open(root.as_path())?;
            let branches = repo
                .branches(Some(BranchType::Local))
                .map_err(|error| git_error(GitOperation::BranchesWithPrefix, error))?;
            let mut names = Vec::new();
            for item in branches {
                let (branch, _) =
                    item.map_err(|error| git_error(GitOperation::BranchesWithPrefix, error))?;
                match branch.name() {
                    Ok(Some(name)) => {
                        if name.starts_with(prefix.as_str()) {
                            names.push(BranchName::new(name)?);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => return Err(git_error(GitOperation::BranchesWithPrefix, error)),
                }
            }
            names.sort();
            Ok(names)
        })
        .await
    }

    /// Create a worktree at `path` on a new `branch` cut from `base`.
    pub async fn worktree_add(
        &self,
        branch: &BranchName,
        path: &WorktreePath,
        base: &CommitId,
    ) -> Result<()> {
        let root = self.root.clone();
        let branch = branch.clone();
        let path = path.clone();
        let base = base.clone();
        self.run(GitOperation::WorktreeAdd, move || {
            let repo = open(root.as_path())?;
            if repo.find_branch(branch.as_str(), BranchType::Local).is_ok() {
                return Err(Error::Path {
                    detail: format!("worktree collision: branch `{branch}` already exists"),
                });
            }
            let base_oid = Oid::from_str(base.as_str())
                .map_err(|error| git_error(GitOperation::WorktreeAdd, error))?;
            let base_commit = repo
                .find_commit(base_oid)
                .map_err(|error| git_error(GitOperation::WorktreeAdd, error))?;
            let branch_ref = repo
                .branch(branch.as_str(), &base_commit, false)
                .map_err(|error| git_error(GitOperation::WorktreeAdd, error))?;
            let reference = branch_ref.into_reference();
            match add_worktree_reference(
                &repo,
                path.as_path(),
                &reference,
                GitOperation::WorktreeAdd,
            ) {
                Ok(()) => Ok(()),
                Err(error) => {
                    if let Ok(mut branch) = repo.find_branch(branch.as_str(), BranchType::Local) {
                        let _ = branch.delete();
                    }
                    Err(error)
                }
            }
        })
        .await
    }

    /// Check out an existing local `branch` into a new worktree at `path`.
    pub async fn worktree_add_existing(
        &self,
        branch: &BranchName,
        path: &WorktreePath,
    ) -> Result<()> {
        let root = self.root.clone();
        let branch = branch.clone();
        let path = path.clone();
        self.run(GitOperation::WorktreeAddExisting, move || {
            let repo = open(root.as_path())?;
            let branch_ref = repo
                .find_branch(branch.as_str(), BranchType::Local)
                .map_err(|_| Error::Path {
                    detail: format!("branch `{branch}` is not a local branch"),
                })?;
            if path.as_path().exists() {
                return Err(Error::Path {
                    detail: format!(
                        "the worktree path `{}` already exists",
                        path.as_path().display()
                    ),
                });
            }
            if let Some(parent) = path
                .as_path()
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .map_err(|error| Error::io(&parent.display().to_string(), error))?;
            }
            let reference = branch_ref.into_reference();
            add_worktree_reference(
                &repo,
                path.as_path(),
                &reference,
                GitOperation::WorktreeAddExisting,
            )
        })
        .await
    }

    /// Remove the worktree at `path`, force-removing a dirty one.
    pub async fn worktree_remove(&self, path: &WorktreePath) -> Result<()> {
        let root = self.root.clone();
        let path = path.clone();
        self.run(GitOperation::WorktreeRemove, move || {
            let repo = open(root.as_path())?;
            let target = canonical(path.as_path());
            let names = worktree_names(&repo, GitOperation::WorktreeRemove)?;
            for name in names {
                let worktree = repo
                    .find_worktree(&name)
                    .map_err(|error| git_error(GitOperation::WorktreeRemove, error))?;
                if canonical(worktree.path()) == target {
                    let mut options = WorktreePruneOptions::new();
                    options.valid(true).working_tree(true);
                    worktree
                        .prune(Some(&mut options))
                        .map_err(|error| git_error(GitOperation::WorktreeRemove, error))?;
                    return Ok(());
                }
            }
            if path.as_path().exists() {
                std::fs::remove_dir_all(path.as_path())
                    .map_err(|error| Error::io(&path.as_path().display().to_string(), error))?;
            }
            Ok(())
        })
        .await
    }

    /// Prune stale worktree metadata.
    pub async fn worktree_prune(&self) -> Result<()> {
        let root = self.root.clone();
        self.run(GitOperation::WorktreePrune, move || {
            let repo = open(root.as_path())?;
            let names = worktree_names(&repo, GitOperation::WorktreePrune)?;
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
    pub async fn branch_delete(&self, branch: &BranchName) -> Result<()> {
        let root = self.root.clone();
        let branch = branch.clone();
        self.run(GitOperation::BranchDelete, move || {
            let repo = open(root.as_path())?;
            let mut branch = repo
                .find_branch(branch.as_str(), BranchType::Local)
                .map_err(|error| git_error(GitOperation::BranchDelete, error))?;
            branch
                .delete()
                .map_err(|error| git_error(GitOperation::BranchDelete, error))
        })
        .await
    }

    /// The changed paths from `git status --porcelain` (plus untracked files and renames).
    pub async fn status(&self) -> Result<Vec<RelativePath>> {
        let root = self.root.clone();
        self.run(GitOperation::Status, move || {
            let repo = open(root.as_path())?;
            let mut options = StatusOptions::new();
            options
                .include_untracked(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true);
            let statuses = repo
                .statuses(Some(&mut options))
                .map_err(|error| git_error(GitOperation::Status, error))?;
            statuses
                .iter()
                .map(|entry| RelativePath::new(path_from_bytes(entry.path_bytes())))
                .collect()
        })
        .await
    }

    /// The tracked changed paths from `git status` (excluding untracked and ignored files).
    pub async fn tracked_status(&self) -> Result<Vec<RelativePath>> {
        let root = self.root.clone();
        self.run(GitOperation::TrackedStatus, move || {
            let repo = open(root.as_path())?;
            let mut options = StatusOptions::new();
            options
                .include_untracked(false)
                .include_ignored(false)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true);
            let statuses = repo
                .statuses(Some(&mut options))
                .map_err(|error| git_error(GitOperation::TrackedStatus, error))?;
            statuses
                .iter()
                .map(|entry| RelativePath::new(path_from_bytes(entry.path_bytes())))
                .collect()
        })
        .await
    }

    /// Stage every change in the working tree and commit it on `HEAD`.
    ///
    /// This is the dispatch plane's completion fallback: an agent that edited the worktree but
    /// did not commit must not lose its work when the run is judged. The commit uses the runner's
    /// configured author/committer signature; callers that need an agent identity should provide
    /// `GIT_COMMITTER_*` in the runner environment.
    ///
    /// A clean tree is not an error: the method commits only when `status()` reports changes.
    /// Returns the new commit id, or `None` when there was nothing to commit.
    pub async fn commit_all(&self, message: &str) -> Result<Option<CommitId>> {
        let root = self.root.clone();
        let env = self.env.clone();
        let message = message.to_owned();
        self.run(GitOperation::CommitAll, move || {
            let repo = open(root.as_path())?;
            let mut options = StatusOptions::new();
            options
                .include_untracked(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true);
            let statuses = repo
                .statuses(Some(&mut options))
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            if statuses.is_empty() {
                return Ok(None);
            }
            let mut index = repo
                .index()
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            // Update tracked entries (including deletions) and add untracked files. The pathspec
            // `*` is intentionally broad: this is the run's own worktree, not the integration
            // checkout.
            index
                .update_all(["*"].iter(), None)
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            index
                .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            index
                .write()
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            let tree_id = index
                .write_tree()
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            let tree = repo
                .find_tree(tree_id)
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            let parent = head_commit(&repo)?;
            let signature = committer_signature(&repo, &env)?;
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    &message,
                    &tree,
                    &[&parent],
                )
                .map_err(|error| git_error(GitOperation::CommitAll, error))?;
            typed_commit_id(oid).map(Some)
        })
        .await
    }

    /// A tracked diff against `base` (default `HEAD`), with the patch bounded to `max_patch_bytes`.
    pub async fn diff(&self, base: Option<&Revision>, max_patch_bytes: ByteLimit) -> Result<Diff> {
        let root = self.root.clone();
        let base = base.cloned();
        self.run(GitOperation::Diff, move || {
            let repo = open(root.as_path())?;
            let base_commit = match base.as_ref() {
                Some(base) => resolve_commit(&repo, base.as_str())?,
                None => head_commit(&repo)?,
            };
            let base_tree = base_commit
                .tree()
                .map_err(|error| git_error(GitOperation::Diff, error))?;
            let mut options = DiffOptions::new();
            let mut diff = repo
                .diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut options))
                .map_err(|error| git_error(GitOperation::Diff, error))?;
            let mut find = DiffFindOptions::new();
            find.renames(true);
            diff.find_similar(Some(&mut find))
                .map_err(|error| git_error(GitOperation::Diff, error))?;
            let stats = diff
                .stats()
                .map_err(|error| git_error(GitOperation::Diff, error))?;
            let stat = if stats.files_changed() == 0 {
                String::new()
            } else {
                String::from_utf8_lossy(
                    &stats
                        .to_buf(DiffStatsFormat::FULL, 80)
                        .map_err(|error| git_error(GitOperation::Diff, error))?,
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
            .map_err(|error| git_error(GitOperation::Diff, error))?;
            let mut patch = String::from_utf8_lossy(&patch).into_owned();
            let truncated_from = (patch.len() > max_patch_bytes.get()).then_some(ByteSize::new(
                u64::try_from(patch.len()).unwrap_or(u64::MAX),
            ));
            if truncated_from.is_some() {
                let mut end = max_patch_bytes.get().min(patch.len());
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
                changed_files.push(RelativePath::new(path_from_bytes(path))?);
            }
            let base = typed_commit_id(base_commit.id())?;
            Ok(Diff {
                range: RevisionRange::new(format!("{base} -> working tree"))?,
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
    pub async fn changed_files_between(
        &self,
        base: &Revision,
        head: &Revision,
    ) -> Result<Vec<RelativePath>> {
        let root = self.root.clone();
        let base = base.clone();
        let head = head.clone();
        self.run(GitOperation::ChangedFilesBetween, move || {
            let repo = open(root.as_path())?;
            let base = resolve_commit(&repo, base.as_str())?;
            let head = resolve_commit(&repo, head.as_str())?;
            let base_tree = base
                .tree()
                .map_err(|error| git_error(GitOperation::ChangedFilesBetween, error))?;
            let head_tree = head
                .tree()
                .map_err(|error| git_error(GitOperation::ChangedFilesBetween, error))?;
            let mut options = DiffOptions::new();
            let mut diff = repo
                .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut options))
                .map_err(|error| git_error(GitOperation::ChangedFilesBetween, error))?;
            let mut find = DiffFindOptions::new();
            find.renames(true);
            diff.find_similar(Some(&mut find))
                .map_err(|error| git_error(GitOperation::ChangedFilesBetween, error))?;
            let mut paths = Vec::new();
            for delta in diff.deltas() {
                let file = delta.new_file();
                let Some(path) = file.path_bytes() else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                paths.push(RelativePath::new(path_from_bytes(path))?);
            }
            paths.sort_unstable();
            paths.dedup();
            Ok(paths)
        })
        .await
    }

    /// The number of commits between `base` and `head`.
    pub async fn commit_count_between(&self, base: &Revision, head: &Revision) -> Result<u64> {
        let root = self.root.clone();
        let base = base.clone();
        let head = head.clone();
        self.run(GitOperation::CommitCountBetween, move || {
            let repo = open(root.as_path())?;
            let base = resolve_commit(&repo, base.as_str())?.id();
            let head = resolve_commit(&repo, head.as_str())?.id();
            let mut walk = repo
                .revwalk()
                .map_err(|error| git_error(GitOperation::CommitCountBetween, error))?;
            walk.push(head)
                .map_err(|error| git_error(GitOperation::CommitCountBetween, error))?;
            walk.hide(base)
                .map_err(|error| git_error(GitOperation::CommitCountBetween, error))?;
            let mut count = 0u64;
            for oid in walk {
                oid.map_err(|error| git_error(GitOperation::CommitCountBetween, error))?;
                count = count.saturating_add(1);
            }
            Ok(count)
        })
        .await
    }

    /// This checkout's Git directory and the repository's common directory.
    pub async fn git_directories(&self) -> Result<GitDirectories> {
        let root = self.root.clone();
        self.run(GitOperation::GitDirectories, move || {
            let repo = open(root.as_path())?;
            Ok(GitDirectories {
                git_dir: GitDirectory::new(canonical(repo.path()))?,
                common_dir: GitCommonDirectory::new(canonical(repo.commondir()))?,
            })
        })
        .await
    }

    /// A digest of this worktree's private config file, or the digest of no configuration.
    ///
    /// A linked worktree may carry `.git/worktrees/<name>/config.worktree`, and the caller grants
    /// the agent write access to that directory. Hashing it (rather than the shared common config,
    /// which the agent cannot write) is the worktree-mode guard's configuration surface. A missing
    /// file and an empty file hash the same, so an untouched worktree has one stable digest.
    pub async fn worktree_config_digest(&self) -> Result<Digest> {
        let root = self.root.clone();
        self.run(GitOperation::WorktreeConfigDigest, move || {
            let repo = open(root.as_path())?;
            let path = repo.path().join("config.worktree");
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(Error::io(&path.display().to_string(), error)),
            };
            sha256_digest(&bytes)
        })
        .await
    }

    /// A digest of the repository-local configuration, includes honored.
    ///
    /// The caller uses this as the shared-checkout guard's configuration surface: any change to the
    /// local config, an included file, or the linked worktree's `config.worktree` changes the
    /// digest.
    pub async fn local_config_digest(&self) -> Result<Digest> {
        let root = self.root.clone();
        self.run(GitOperation::LocalConfigDigest, move || {
            let repo = open(root.as_path())?;
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
                        return Err(git_error(GitOperation::LocalConfigDigest, error));
                    }
                };
                let mut config_entries = config
                    .entries(None)
                    .map_err(|error| git_error(GitOperation::LocalConfigDigest, error))?;
                while let Some(entry) = config_entries.next() {
                    let entry = entry
                        .map_err(|error| git_error(GitOperation::LocalConfigDigest, error))?;
                    let name = entry.name_bytes().to_vec();
                    let value = entry.value_bytes().to_vec();
                    total = total.saturating_add(name.len() + value.len() + 16);
                    if total > MAX_CONFIG_BYTES {
                        return Err(Error::git(
                            GitOperation::LocalConfigDigest,
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
            digest_from_hasher(&hasher)
        })
        .await
    }

    /// Run one blocking Git operation off the async worker under [`GIT_TIMEOUT`].
    async fn run<T, F>(&self, operation: GitOperation, operation_fn: F) -> Result<T>
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

/// Add a worktree checking out `reference` at `path`.
fn add_worktree_reference(
    repo: &Repository,
    path: &Path,
    reference: &git2::Reference<'_>,
    operation: GitOperation,
) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Path {
            detail: format!(
                "the worktree path `{}` has no usable final component",
                path.display()
            ),
        })?;
    let mut options = WorktreeAddOptions::new();
    options.reference(Some(reference));
    repo.worktree(name, path, Some(&options))
        .map_err(|error| git_error(operation, error))?;
    Ok(())
}

/// The names of every linked worktree.
fn worktree_names(repo: &Repository, operation: GitOperation) -> Result<Vec<String>> {
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
        .map_err(|error| git_error(GitOperation::MergeFfOnly, error))?
    {
        return Err(Error::git(
            GitOperation::MergeFfOnly,
            format!("`{reference}` is not a fast-forward of HEAD"),
        ));
    }
    repo.reset(target.as_object(), ResetType::Hard, None)
        .map_err(|error| git_error(GitOperation::MergeFfOnly, error))
}

/// The committer signature from the supplied environment, then the repository config.
fn committer_signature(repo: &Repository, env: &Env) -> Result<Signature<'static>> {
    if let (Some(name), Some(email)) = (
        env.get("GIT_COMMITTER_NAME"),
        env.get("GIT_COMMITTER_EMAIL"),
    ) {
        Signature::now(name.as_str(), email.as_str())
            .map_err(|error| git_error(GitOperation::Other("git signature"), error))
    } else {
        repo.signature()
            .map_err(|error| git_error(GitOperation::Other("git signature"), error))
    }
}

/// The expected old target of a compare-and-swap update, where an all-zero id means "absent".
fn expected_old(old: &CommitId) -> Result<Option<Oid>> {
    if old.as_str().bytes().all(|byte| byte == b'0') {
        return Ok(None);
    }
    Oid::from_str(old.as_str())
        .map(Some)
        .map_err(|error| git_error(GitOperation::UpdateRef, error))
}

/// Resolve one revision to a commit.
fn resolve_commit<'repo>(repo: &'repo Repository, rev: &str) -> Result<git2::Commit<'repo>> {
    repo.revparse_single(rev)
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| git_error(GitOperation::RevParse, error))
}

/// Resolve one revision to a commit id, or `None`.
fn commit_id(repo: &Repository, rev: &str) -> Option<Oid> {
    resolve_commit(repo, rev).ok().map(|commit| commit.id())
}

/// A validated commit id from a libgit2 object id.
fn typed_commit_id(oid: Oid) -> Result<CommitId> {
    CommitId::new(oid.to_string())
}

/// The current commit.
fn head_commit<'repo>(repo: &'repo Repository) -> Result<git2::Commit<'repo>> {
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| git_error(GitOperation::Head, error))
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
    Repository::open(root).map_err(|error| git_error(GitOperation::Other("git open"), error))
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

/// The digest of `bytes`.
fn sha256_digest(bytes: &[u8]) -> Result<Digest> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    digest_from_hasher(&hasher)
}

/// Finish a SHA-256 hasher as a validated digest.
fn digest_from_hasher(hasher: &Sha256) -> Result<Digest> {
    let mut digest = String::with_capacity(64);
    for byte in hasher.clone().finalize() {
        digest.push_str(&format!("{byte:02x}"));
    }
    Digest::new(digest)
}

/// Wrap a libgit2 error with the logical operation, never an environment or a credential.
fn git_error(operation: GitOperation, error: git2::Error) -> Error {
    Error::git(operation, error.message().to_owned())
}

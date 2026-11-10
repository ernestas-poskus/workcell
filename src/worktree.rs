//! One isolated checkout per unit of work: a Git worktree and the branch it carries.
//!
//! [`RepoSpec::prepare`] fetches the configured remote, picks the newest usable base (the local
//! integration branch when it already contains the remote, the remote when the local branch can be
//! fast-forwarded), refuses a dirty integration checkout or a colliding branch/worktree path, and
//! cuts one new branch with `git worktree add -b`. The returned [`Worktree`] records the Git
//! directory of the new checkout, so a caller can grant exactly that worktree and nothing
//! else in the common Git directory.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::types::{
    BranchName, BranchPrefix, CommitId, Env, GitCommonDirectory, GitDirectory, ReferenceName,
    RemoteName, RepoRoot, Revision, WorktreeName, WorktreePath, WorktreeRoot,
};

/// Everything a repository needs to cut and clean up a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    /// The integration checkout that owns the repository.
    pub root: RepoRoot,
    /// The remote fetched before a worktree is cut.
    pub remote: RemoteName,
    /// The branch a worktree is based on.
    pub base_branch: BranchName,
    /// The directory worktrees are created under, relative to [`RepoSpec::root`] or absolute.
    pub worktree_root: WorktreeRoot,
    /// The prefix of generated branch names.
    pub branch_prefix: BranchPrefix,
    /// Refuse to cut a worktree when the integration checkout is dirty.
    pub require_clean_root: bool,
    /// The environment every Git process receives.
    pub env: Env,
}

/// One prepared worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The checkout the agent edits.
    pub path: WorktreePath,
    /// The branch checked out there.
    pub branch: BranchName,
    /// The base commit the branch was cut from.
    pub base_sha: CommitId,
    /// This worktree's Git directory (`.git/worktrees/<name>`).
    pub git_dir: GitDirectory,
    /// The repository's common Git directory.
    pub git_common_dir: GitCommonDirectory,
}

/// A filesystem- and ref-safe slug.
#[must_use]
pub fn slug(raw: &str) -> WorktreeName {
    let mut out = String::with_capacity(raw.len());
    let mut dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    let raw = if out.is_empty() {
        "work".to_owned()
    } else {
        out
    };
    // The slug is built from ASCII alphanumerics and interior dashes only, so it is a valid
    // worktree component by construction.
    WorktreeName::new(raw).expect("a slug is a valid worktree name")
}

impl RepoSpec {
    /// The branch and directory name one label owns.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the configured prefix cannot form a branch name, and [`Error::Path`]
    /// when the resolved worktree root is not an absolute path.
    pub fn names(&self, label: &str) -> Result<(BranchName, WorktreePath)> {
        let name = slug(label);
        let branch = BranchName::new(format!("{}/{name}/head", self.branch_prefix.as_str()))?;
        let path = WorktreePath::new(self.resolved_worktree_root().join(name.as_str()))?;
        Ok((branch, path))
    }

    /// Fetch the base and cut one worktree on a new branch.
    ///
    /// # Errors
    ///
    /// Refuses a dirty integration checkout ([`Error::Path`]), an existing branch or worktree
    /// path (also [`Error::Path`]), and a base that Git cannot resolve or that has diverged
    /// ([`Error::Git`]).
    pub async fn prepare(&self, label: &str) -> Result<Worktree> {
        if self.require_clean_root {
            self.ensure_clean().await?;
        }
        let git = self.git();
        let remote = self.remote.as_str();
        let base_branch = self.base_branch.as_str();
        let (branch, path) = self.names(label)?;
        let branch_name = branch.as_str();
        let (ancestor, _) = branch_name
            .rsplit_once('/')
            .expect("a generated branch contains `/`");
        if base_branch == ancestor || base_branch.starts_with(&format!("{ancestor}/")) {
            return Err(Error::Config {
                detail: format!(
                    "configured base branch `{base_branch}` is at or under the run ref directory `{ancestor}`"
                ),
            });
        }
        git.fetch(&self.remote).await?;
        let remote_base = format!("{remote}/{base_branch}");
        let remote_sha = git.rev_parse(&Revision::new(&remote_base)?).await?;
        let local_ref = format!("refs/heads/{base_branch}");
        let local_sha = git.rev_parse(&Revision::new(&local_ref)?).await?;
        // A local integration queue can advance the base branch beyond the remote; a worktree must
        // start from the newest local integration point, not a stale remote ref.
        let remote_rev = Revision::new(remote_sha.as_str())?;
        let local_rev = Revision::new(local_sha.as_str())?;
        let base_sha = if git.is_ancestor(&remote_rev, &local_rev).await? {
            local_sha
        } else if git.is_ancestor(&local_rev, &remote_rev).await? {
            self.fast_forward_root(&git, &remote_base, base_branch)
                .await?;
            remote_sha
        } else {
            return Err(Error::git(
                "git merge-base --is-ancestor",
                format!("local `{base_branch}` and `{remote_base}` diverged"),
            ));
        };
        if git
            .show_ref(&ReferenceName::new(format!("refs/heads/{ancestor}"))?)
            .await?
        {
            return Err(Error::Path {
                detail: format!("worktree collision: branch `{ancestor}` already exists"),
            });
        }
        if git
            .show_ref(&ReferenceName::new(format!("refs/heads/{branch_name}"))?)
            .await?
        {
            return Err(Error::Path {
                detail: format!("worktree collision: branch `{branch_name}` already exists"),
            });
        }
        if path.as_path().exists() {
            return Err(Error::Path {
                detail: format!(
                    "worktree collision: path {} already exists",
                    path.as_path().display()
                ),
            });
        }
        if let Some(parent) = path.as_path().parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| Error::io(&parent.display().to_string(), error))?;
        }
        git.worktree_add(&branch, &path, &base_sha).await?;
        let (git_dir, git_common_dir) = self.discover_git_dirs(path.as_path()).await?;
        let worktree = Worktree {
            path,
            branch,
            base_sha,
            git_dir: GitDirectory::new(git_dir)?,
            git_common_dir: GitCommonDirectory::new(git_common_dir)?,
        };
        let (run_ref_dir, run_reflog_dir) = worktree.run_ref_dirs()?;
        let base_ref = worktree
            .git_common_dir
            .as_path()
            .join("refs")
            .join("heads")
            .join(base_branch);
        if base_ref.starts_with(&run_ref_dir) {
            let _ = git.worktree_remove(&worktree.path).await;
            let _ = git.branch_delete(&worktree.branch).await;
            return Err(Error::Config {
                detail: format!(
                    "configured base branch `{}` is at or under the run ref directory `{}`",
                    self.base_branch,
                    run_ref_dir.display()
                ),
            });
        }
        tokio::fs::create_dir_all(&run_ref_dir)
            .await
            .map_err(|error| Error::io(&run_ref_dir.display().to_string(), error))?;
        tokio::fs::create_dir_all(&run_reflog_dir)
            .await
            .map_err(|error| Error::io(&run_reflog_dir.display().to_string(), error))?;
        Ok(worktree)
    }

    /// Remove a worktree and its branch.
    ///
    /// Removal is best-effort, as it is for a caller cleaning up after a finished unit of work: a
    /// worktree that is already gone, or a branch that was already deleted, is not a failure.
    pub async fn remove(&self, worktree: &Worktree) -> Result<()> {
        let git = self.git();
        let _ = git.worktree_remove(&worktree.path).await;
        let _ = git.branch_delete(&worktree.branch).await;
        let _ = git.worktree_prune().await;
        Ok(())
    }

    /// Prune stale worktree metadata.
    ///
    /// # Errors
    ///
    /// Returns a Git failure when the repository cannot be read or pruned.
    pub async fn prune(&self) -> Result<()> {
        self.git().worktree_prune().await
    }

    /// Refuse when the integration checkout has uncommitted changes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] naming the first changed paths when the checkout is dirty, and
    /// [`Error::Git`] when its status cannot be read.
    pub async fn ensure_clean(&self) -> Result<()> {
        let status = self.git().status().await?;
        if status.is_empty() {
            return Ok(());
        }
        let changed: Vec<String> = status.iter().take(8).map(ToString::to_string).collect();
        Err(Error::Path {
            detail: format!("the integration checkout is dirty: {}", changed.join(", ")),
        })
    }

    /// The base currently advertised by the configured remote.
    ///
    /// # Errors
    ///
    /// Returns a Git failure when the fetch fails or the remote base cannot be resolved, and
    /// [`Error::Config`] when the resolved value is not a commit id.
    pub async fn remote_base(&self) -> Result<CommitId> {
        let git = self.git();
        git.fetch(&self.remote).await?;
        let revision = Revision::new(format!(
            "{}/{}",
            self.remote.as_str(),
            self.base_branch.as_str()
        ))?;
        git.rev_parse(&revision).await
    }

    /// The current local integration base commit.
    ///
    /// # Errors
    ///
    /// Returns a Git failure when the base branch cannot be resolved, and [`Error::Config`] when
    /// the resolved value is not a commit id.
    pub async fn current_base(&self) -> Result<CommitId> {
        let revision = Revision::new(format!("refs/heads/{}", self.base_branch.as_str()))?;
        self.git().rev_parse(&revision).await
    }

    /// The configured worktree root: absolute, or relative to [`RepoSpec::root`].
    #[must_use]
    pub fn worktree_root(&self) -> &Path {
        self.worktree_root.as_path()
    }

    /// The worktree root as an absolute path.
    fn resolved_worktree_root(&self) -> PathBuf {
        if self.worktree_root.as_path().is_absolute() {
            self.worktree_root.as_path().to_path_buf()
        } else {
            self.root.as_path().join(self.worktree_root.as_path())
        }
    }

    /// A Git runner confined to the integration checkout.
    fn git(&self) -> Git {
        Git::new(self.root.clone()).with_env(self.env.clone())
    }

    /// Fast-forward the integration checkout's base branch to the remote base.
    async fn fast_forward_root(
        &self,
        git: &Git,
        remote_base: &str,
        base_branch: &str,
    ) -> Result<()> {
        let current = git
            .branch_name()
            .await
            .map_err(|error| {
                Error::git(
                    "git rev-parse --abbrev-ref HEAD",
                    format!("the integration checkout is not on a branch: {error}"),
                )
            })?
            .ok_or_else(|| {
                Error::git(
                    "git rev-parse --abbrev-ref HEAD",
                    "the integration checkout is not on a branch",
                )
            })?;
        if current.as_str() != base_branch {
            return Err(Error::git(
                "git symbolic-ref --short HEAD",
                format!("the integration checkout is on `{current}`, not `{base_branch}`"),
            ));
        }
        git.merge_ff_only(&BranchName::new(remote_base)?).await
    }

    /// The new worktree's Git directory and the repository's common Git directory, from one
    /// `git rev-parse --absolute-git-dir --git-common-dir` run inside it.
    async fn discover_git_dirs(&self, path: &Path) -> Result<(PathBuf, PathBuf)> {
        let root = RepoRoot::new(path)?;
        git_directories(&Git::new(root).with_env(self.env.clone())).await
    }
}

impl Worktree {
    /// Adopt an existing checkout as a worktree.
    ///
    /// The branch, current commit, and Git directories come from `git`'s checkout. This is the
    /// entry point for a caller that already owns a checkout (for example a repair pass over a
    /// worktree cut earlier) and needs the same [`Worktree`] grants a caller expects.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `HEAD` is detached or `git` is not confined to a branch; [`Error::Git`]
    /// when the path is not a Git worktree or Git cannot answer; [`Error::Path`] when the Git
    /// directories cannot be named.
    pub async fn discover(git: &Git) -> Result<Worktree> {
        let branch = git
            .branch_name()
            .await
            .map_err(|error| Error::Config {
                detail: format!(
                    "cannot adopt `{}` as a worktree: HEAD is detached or not on a branch ({error})",
                    git.root().as_path().display()
                ),
            })?
            .ok_or_else(|| Error::Config {
                detail: format!(
                    "cannot adopt `{}` as a worktree: HEAD is detached or unborn",
                    git.root().as_path().display()
                ),
            })?;
        let base_sha = git.rev_parse(&Revision::new("HEAD")?).await?;
        let (git_dir, git_common_dir) = git_directories(git).await?;
        let worktree = Worktree {
            path: WorktreePath::new(git.root().as_path())?,
            branch,
            base_sha,
            git_dir: GitDirectory::new(git_dir)?,
            git_common_dir: GitCommonDirectory::new(git_common_dir)?,
        };
        if let Ok((run_ref_dir, run_reflog_dir)) = worktree.run_ref_dirs() {
            tokio::fs::create_dir_all(&run_ref_dir)
                .await
                .map_err(|error| Error::io(&run_ref_dir.display().to_string(), error))?;
            tokio::fs::create_dir_all(&run_reflog_dir)
                .await
                .map_err(|error| Error::io(&run_reflog_dir.display().to_string(), error))?;
        }
        Ok(worktree)
    }

    /// The run's ref directory (`<common>/refs/heads/<namespace>`) and reflog directory
    /// (`<common>/logs/refs/heads/<namespace>`).
    ///
    /// # Errors
    ///
    /// Refuses a branch with no `/`, any `.`/`..`/absolute component, or a result that is not a
    /// strict descendant of the heads roots.
    pub(crate) fn run_ref_dirs(&self) -> Result<(PathBuf, PathBuf)> {
        let branch = self.branch.as_str();
        if !branch.contains('/') {
            return Err(Error::Path {
                detail: format!(
                    "worktree branch `{}` has no namespace: missing `/` separator",
                    self.branch
                ),
            });
        }
        let (namespace, _) = branch.rsplit_once('/').expect("branch contains `/`");
        if namespace.is_empty() {
            return Err(Error::Path {
                detail: format!("worktree branch `{}` has an empty namespace", self.branch),
            });
        }
        for component in Path::new(branch).components() {
            match component {
                std::path::Component::Normal(part) => {
                    let s = part.to_string_lossy();
                    if s == "." || s == ".." {
                        return Err(Error::Path {
                            detail: format!(
                                "worktree branch `{}` contains invalid path component `{s}`",
                                self.branch
                            ),
                        });
                    }
                }
                _ => {
                    return Err(Error::Path {
                        detail: format!(
                            "worktree branch `{}` contains non-normal path components",
                            self.branch
                        ),
                    });
                }
            }
        }

        let heads_root = self.git_common_dir.as_path().join("refs").join("heads");
        let logs_root = self
            .git_common_dir
            .as_path()
            .join("logs")
            .join("refs")
            .join("heads");
        let run_ref_dir = heads_root.join(namespace);
        let run_reflog_dir = logs_root.join(namespace);

        if !run_ref_dir.starts_with(&heads_root) || run_ref_dir == heads_root {
            return Err(Error::Path {
                detail: format!(
                    "run ref directory `{}` is not a strict descendant of `{}`",
                    run_ref_dir.display(),
                    heads_root.display()
                ),
            });
        }
        if !run_reflog_dir.starts_with(&logs_root) || run_reflog_dir == logs_root {
            return Err(Error::Path {
                detail: format!(
                    "run reflog directory `{}` is not a strict descendant of `{}`",
                    run_reflog_dir.display(),
                    logs_root.display()
                ),
            });
        }

        Ok((run_ref_dir, run_reflog_dir))
    }
}

/// The Git directory of the checkout `git` is confined to, and the repository's common directory.
async fn git_directories(git: &Git) -> Result<(PathBuf, PathBuf)> {
    let directories = git.git_directories().await?;
    Ok((
        directories.git_dir.as_path().to_path_buf(),
        directories.common_dir.as_path().to_path_buf(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(root: &str) -> RepoSpec {
        RepoSpec {
            root: RepoRoot::new(root).expect("root"),
            remote: RemoteName::new("origin").expect("remote"),
            base_branch: BranchName::new("main").expect("base branch"),
            worktree_root: WorktreeRoot::new(".worktrees").expect("worktree root"),
            branch_prefix: BranchPrefix::new("agent").expect("branch prefix"),
            require_clean_root: true,
            env: Env::empty(),
        }
    }

    #[test]
    fn a_slug_is_lowercase_and_dash_joined() {
        assert_eq!(slug("DEMO-93").as_str(), "demo-93");
        assert_eq!(slug("  Feature / One  ").as_str(), "feature-one");
        assert_eq!(slug("a..b").as_str(), "a-b");
        assert_eq!(slug("--").as_str(), "work");
        assert_eq!(slug("").as_str(), "work");
    }

    #[test]
    fn names_are_derived_from_the_label() {
        let spec = spec("/repo");
        let (branch, path) = spec.names("demo-93 run-1").expect("names");
        assert_eq!(branch.as_str(), "agent/demo-93-run-1/head");
        assert_eq!(
            path.as_path(),
            PathBuf::from("/repo/.worktrees/demo-93-run-1")
        );
    }

    #[test]
    fn an_absolute_worktree_root_is_used_as_is() {
        let mut spec = spec("/repo");
        spec.worktree_root = WorktreeRoot::new("/elsewhere/trees").expect("worktree root");
        let (_, path) = spec.names("x").expect("names");
        assert_eq!(path.as_path(), PathBuf::from("/elsewhere/trees/x"));
    }

    /// Initialise a repository with one commit on `main`, entirely through `git2`.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init_opts(
            dir.path(),
            git2::RepositoryInitOptions::new().initial_head("main"),
        )
        .expect("init");
        std::fs::write(dir.path().join("tracked.txt"), "one\n").expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("tracked.txt")).expect("add");
        let tree = index.write_tree().expect("tree");
        let tree = repo.find_tree(tree).expect("tree object");
        let signature = git2::Signature::now("test", "test@example.invalid").expect("signature");
        repo.commit(Some("HEAD"), &signature, &signature, "one", &tree, &[])
            .expect("commit");
        dir
    }

    #[tokio::test]
    async fn adopting_a_checkout_reports_its_branch_commit_and_git_directories() {
        let dir = repository();
        let git = Git::new(RepoRoot::new(dir.path()).expect("repository root"));
        let worktree = Worktree::discover(&git).await.expect("adopt");
        assert_eq!(worktree.path.as_path(), dir.path());
        assert_eq!(worktree.branch.as_str(), "main");
        assert_eq!(
            worktree.base_sha.as_str(),
            git.rev_parse(&Revision::new("HEAD").expect("revision"))
                .await
                .expect("head")
                .as_str()
        );
        // Git reports the resolved path, which on macOS resolves `/var` to `/private/var`.
        assert!(
            worktree.git_dir.as_path().ends_with(".git"),
            "git dir: {}",
            worktree.git_dir.as_path().display()
        );
        assert!(
            worktree.git_common_dir.as_path().ends_with(".git"),
            "common dir: {}",
            worktree.git_common_dir.as_path().display()
        );
    }

    #[tokio::test]
    async fn adopting_a_detached_head_is_refused() {
        let dir = repository();
        let git = Git::new(RepoRoot::new(dir.path()).expect("repository root"));
        let head = git
            .rev_parse(&Revision::new("HEAD").expect("revision"))
            .await
            .expect("head");
        let repo = git2::Repository::open(dir.path()).expect("open");
        let oid = git2::Oid::from_str(head.as_str()).expect("oid");
        repo.set_head_detached(oid).expect("detach");
        let error = Worktree::discover(&git).await.expect_err("refused");
        assert!(matches!(error, Error::Config { .. }), "{error}");
    }
}

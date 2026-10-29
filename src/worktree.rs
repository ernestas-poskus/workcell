//! One isolated checkout per unit of work: a Git worktree and the branch it carries.
//!
//! [`RepoSpec::prepare`] fetches the configured remote, picks the newest usable base (the local
//! integration branch when it already contains the remote, the remote when the local branch can be
//! fast-forwarded), refuses a dirty integration checkout or a colliding branch/worktree path, and
//! cuts one new branch with `git worktree add -b`. The returned [`Worktree`] records the Git
//! directory of the new checkout, so a sandbox policy can grant exactly that worktree and nothing
//! else in the common Git directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::Git;

/// Everything a repository needs to cut and clean up a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    /// The integration checkout that owns the repository.
    pub root: PathBuf,
    /// The remote fetched before a worktree is cut.
    pub remote: String,
    /// The branch a worktree is based on.
    pub base_branch: String,
    /// The directory worktrees are created under, relative to [`RepoSpec::root`] or absolute.
    pub worktree_root: PathBuf,
    /// The prefix of generated branch names.
    pub branch_prefix: String,
    /// Refuse to cut a worktree when the integration checkout is dirty.
    pub require_clean_root: bool,
    /// The environment every Git process receives.
    pub env: BTreeMap<String, String>,
}

/// One prepared worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The checkout the agent edits.
    pub path: PathBuf,
    /// The branch checked out there.
    pub branch: String,
    /// The base commit the branch was cut from.
    pub base_sha: String,
    /// This worktree's Git directory (`.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// The repository's common Git directory.
    pub git_common_dir: PathBuf,
}

/// A filesystem- and ref-safe slug.
#[must_use]
pub fn slug(raw: &str) -> String {
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
    if out.is_empty() {
        "work".to_owned()
    } else {
        out
    }
}

impl RepoSpec {
    /// The branch and directory name one label owns: `(branch, absolute worktree path)`.
    #[must_use]
    pub fn names(&self, label: &str) -> (String, PathBuf) {
        let name = slug(label);
        let prefix = self.branch_prefix.trim_end_matches('/');
        let branch = if prefix.is_empty() {
            format!("{name}/head")
        } else {
            format!("{prefix}/{name}/head")
        };
        (branch, self.resolved_worktree_root().join(name))
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
        let remote = self.remote.trim();
        let base_branch = self.base_branch.trim();
        let (branch, path) = self.names(label);
        let (ancestor, _) = branch.rsplit_once('/').expect("branch contains `/`");
        if base_branch == ancestor || base_branch.starts_with(&format!("{ancestor}/")) {
            return Err(Error::Config {
                detail: format!(
                    "configured base branch `{base_branch}` is at or under the run ref directory `{ancestor}`"
                ),
            });
        }
        git.fetch(remote).await?;
        let remote_base = format!("{remote}/{base_branch}");
        let remote_sha = git.rev_parse(&remote_base).await?;
        let local_ref = format!("refs/heads/{base_branch}");
        let local_sha = git.rev_parse(&local_ref).await?;
        // A local integration queue can advance the base branch beyond the remote; a worktree must
        // start from the newest local integration point, not a stale remote ref.
        let base_sha = if git.is_ancestor(&remote_sha, &local_sha).await? {
            local_sha
        } else if git.is_ancestor(&local_sha, &remote_sha).await? {
            self.fast_forward_root(&git, &remote_base, base_branch)
                .await?;
            remote_sha
        } else {
            return Err(Error::git(
                "git merge-base --is-ancestor",
                format!("local `{base_branch}` and `{remote_base}` diverged"),
            ));
        };
        if git.show_ref(&format!("refs/heads/{ancestor}")).await? {
            return Err(Error::Path {
                detail: format!("worktree collision: branch `{ancestor}` already exists"),
            });
        }
        if git.show_ref(&format!("refs/heads/{branch}")).await? {
            return Err(Error::Path {
                detail: format!("worktree collision: branch `{branch}` already exists"),
            });
        }
        if path.exists() {
            return Err(Error::Path {
                detail: format!("worktree collision: path {} already exists", path.display()),
            });
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| Error::io(&parent.display().to_string(), error))?;
        }
        git.worktree_add(&branch, &path, &base_sha).await?;
        let (git_dir, git_common_dir) = self.discover_git_dirs(&path).await?;
        let worktree = Worktree {
            path,
            branch,
            base_sha,
            git_dir,
            git_common_dir,
        };
        let (run_ref_dir, run_reflog_dir) = worktree.run_ref_dirs()?;
        let base_ref = worktree
            .git_common_dir
            .join("refs")
            .join("heads")
            .join(self.base_branch.trim());
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
        let changed: Vec<String> = status
            .iter()
            .take(8)
            .map(|path| path.display().to_string())
            .collect();
        Err(Error::Path {
            detail: format!("the integration checkout is dirty: {}", changed.join(", ")),
        })
    }

    /// The base currently advertised by the configured remote.
    ///
    /// # Errors
    ///
    /// Returns a Git failure when the fetch fails or the remote base cannot be resolved.
    pub async fn remote_base(&self) -> Result<String> {
        let git = self.git();
        git.fetch(self.remote.trim()).await?;
        git.rev_parse(&format!(
            "{}/{}",
            self.remote.trim(),
            self.base_branch.trim()
        ))
        .await
    }

    /// The current local integration base commit.
    ///
    /// # Errors
    ///
    /// Returns a Git failure when the base branch cannot be resolved.
    pub async fn current_base(&self) -> Result<String> {
        self.git()
            .rev_parse(&format!("refs/heads/{}", self.base_branch.trim()))
            .await
    }

    /// The configured worktree root: absolute, or relative to [`RepoSpec::root`].
    #[must_use]
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// The worktree root as an absolute path.
    fn resolved_worktree_root(&self) -> PathBuf {
        if self.worktree_root.is_absolute() {
            self.worktree_root.clone()
        } else {
            self.root.join(&self.worktree_root)
        }
    }

    /// A Git runner confined to the integration checkout.
    fn git(&self) -> Git {
        Git::new(&self.root).with_env(self.env.clone())
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
        if current != base_branch {
            return Err(Error::git(
                "git symbolic-ref --short HEAD",
                format!("the integration checkout is on `{current}`, not `{base_branch}`"),
            ));
        }
        git.merge_ff_only(remote_base).await
    }

    /// The new worktree's Git directory and the repository's common Git directory, from one
    /// `git rev-parse --absolute-git-dir --git-common-dir` run inside it.
    async fn discover_git_dirs(&self, path: &Path) -> Result<(PathBuf, PathBuf)> {
        git_directories(&Git::new(path).with_env(self.env.clone())).await
    }
}

impl Worktree {
    /// Adopt an existing checkout as a worktree.
    ///
    /// The branch, current commit, and Git directories come from `git`'s checkout. This is the
    /// entry point for a caller that already owns a checkout (for example a repair pass over a
    /// worktree cut earlier) and needs the same [`Worktree`] grants the sandbox policy expects.
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
                    git.root().display()
                ),
            })?
            .ok_or_else(|| Error::Config {
                detail: format!(
                    "cannot adopt `{}` as a worktree: HEAD is detached or unborn",
                    git.root().display()
                ),
            })?;
        let base_sha = git.rev_parse("HEAD").await?;
        let (git_dir, git_common_dir) = git_directories(git).await?;
        let worktree = Worktree {
            path: git.root().to_path_buf(),
            branch,
            base_sha,
            git_dir,
            git_common_dir,
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
        if !self.branch.contains('/') {
            return Err(Error::Path {
                detail: format!(
                    "worktree branch `{}` has no namespace: missing `/` separator",
                    self.branch
                ),
            });
        }
        let (namespace, _) = self.branch.rsplit_once('/').expect("branch contains `/`");
        if namespace.is_empty() {
            return Err(Error::Path {
                detail: format!("worktree branch `{}` has an empty namespace", self.branch),
            });
        }
        for component in Path::new(&self.branch).components() {
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

        let heads_root = self.git_common_dir.join("refs").join("heads");
        let logs_root = self.git_common_dir.join("logs").join("refs").join("heads");
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
    Ok((directories.git_dir, directories.common_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(root: &str) -> RepoSpec {
        RepoSpec {
            root: PathBuf::from(root),
            remote: "origin".to_owned(),
            base_branch: "main".to_owned(),
            worktree_root: PathBuf::from(".worktrees"),
            branch_prefix: "agent".to_owned(),
            require_clean_root: true,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn a_slug_is_lowercase_and_dash_joined() {
        assert_eq!(slug("DEMO-93"), "demo-93");
        assert_eq!(slug("  Feature / One  "), "feature-one");
        assert_eq!(slug("a..b"), "a-b");
        assert_eq!(slug("--"), "work");
        assert_eq!(slug(""), "work");
    }

    #[test]
    fn names_are_derived_from_the_label() {
        let spec = spec("/repo");
        let (branch, path) = spec.names("demo-93 run-1");
        assert_eq!(branch, "agent/demo-93-run-1/head");
        assert_eq!(path, PathBuf::from("/repo/.worktrees/demo-93-run-1"));
    }

    #[test]
    fn an_absolute_worktree_root_is_used_as_is() {
        let mut spec = spec("/repo");
        spec.worktree_root = PathBuf::from("/elsewhere/trees");
        let (_, path) = spec.names("x");
        assert_eq!(path, PathBuf::from("/elsewhere/trees/x"));
    }

    /// Initialise a repository with one commit on `main`.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("tracked.txt"), "one\n").expect("write");
        run(&["add", "tracked.txt"]);
        run(&["commit", "-q", "-m", "one"]);
        dir
    }

    #[tokio::test]
    async fn adopting_a_checkout_reports_its_branch_commit_and_git_directories() {
        let dir = repository();
        let git = Git::new(dir.path());
        let worktree = Worktree::discover(&git).await.expect("adopt");
        assert_eq!(worktree.path, dir.path());
        assert_eq!(worktree.branch, "main");
        assert_eq!(
            worktree.base_sha,
            git.rev_parse("HEAD").await.expect("head")
        );
        // Git reports the resolved path, which on macOS resolves `/var` to `/private/var`.
        assert!(
            worktree.git_dir.ends_with(".git"),
            "git dir: {}",
            worktree.git_dir.display()
        );
        assert!(
            worktree.git_common_dir.ends_with(".git"),
            "common dir: {}",
            worktree.git_common_dir.display()
        );
    }

    #[tokio::test]
    async fn adopting_a_detached_head_is_refused() {
        let dir = repository();
        let git = Git::new(dir.path());
        let head = git.rev_parse("HEAD").await.expect("head");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["checkout", "-q", "--detach", &head])
            .status()
            .expect("git checkout --detach");
        assert!(status.success());
        let error = Worktree::discover(&git).await.expect_err("refused");
        assert!(matches!(error, Error::Config { .. }), "{error}");
    }
}

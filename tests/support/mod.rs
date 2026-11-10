//! In-process `git2` fixtures for the integration tests and the worktree lifecycle example.
//!
//! These helpers create the ordinary states a fixture needs — a repository on `main`, a bare
//! remote, a clone, branches, commits, and linked worktrees — through `git2`. Fixture setup
//! therefore never spawns a Git child process. The three hardening tests in `tests/git.rs`
//! deliberately keep a plain `git` child as a negative control: they must first prove that the
//! system Git runs a planted hook, external diff driver, or textconv driver, and only then assert
//! that the crate's in-process runner does not.
//!
//! Every commit uses one fixed signature, so a fixture's identity never depends on the ambient Git
//! configuration of the machine that runs the tests.

#![allow(dead_code)]

use std::path::Path;

use git2::{
    Commit, IndexAddOption, ObjectType, Oid, Repository, RepositoryInitOptions, Signature,
    StatusOptions, TreeWalkMode, WorktreeAddOptions, WorktreePruneOptions, build::CheckoutBuilder,
};

/// Author and committer every fixture commit uses.
const FIXTURE_NAME: &str = "t";
const FIXTURE_EMAIL: &str = "t@example.invalid";

fn open(dir: &Path) -> Repository {
    Repository::open(dir).expect("open fixture repository")
}

fn signature() -> Signature<'static> {
    Signature::now(FIXTURE_NAME, FIXTURE_EMAIL).expect("fixture signature")
}

fn config_set(repo: &Repository, key: &str, value: &str) {
    repo.config()
        .and_then(|mut config| config.set_str(key, value))
        .unwrap_or_else(|error| panic!("set {key}: {error}"));
}

/// Initialize a repository whose initial branch is `main`.
pub fn init(dir: &Path) -> Repository {
    std::fs::create_dir_all(dir).expect("create fixture checkout");
    Repository::init_opts(dir, RepositoryInitOptions::new().initial_head("main"))
        .expect("init fixture repository")
}

/// Initialize a bare repository whose default branch is `main`.
pub fn init_bare(dir: &Path) -> Repository {
    std::fs::create_dir_all(dir).expect("create bare fixture repository");
    Repository::init_opts(
        dir,
        RepositoryInitOptions::new().bare(true).initial_head("main"),
    )
    .expect("init bare fixture repository")
}

/// Set the repository-local `user.name` and `user.email`.
pub fn configure_user(dir: &Path, name: &str, email: &str) {
    let repo = open(dir);
    config_set(&repo, "user.name", name);
    config_set(&repo, "user.email", email);
}

/// Set one repository-local configuration entry.
pub fn set_config(dir: &Path, key: &str, value: &str) {
    config_set(&open(dir), key, value);
}

/// Stage paths relative to the repository worktree.
pub fn stage(dir: &Path, paths: &[&str]) {
    let repo = open(dir);
    let mut index = repo.index().expect("fixture index");
    for path in paths {
        index
            .add_path(Path::new(path))
            .unwrap_or_else(|error| panic!("stage {path}: {error}"));
    }
    index.write().expect("write fixture index");
}

/// Stage every change, like `git add -A`.
pub fn stage_all(dir: &Path) {
    let repo = open(dir);
    let mut index = repo.index().expect("fixture index");
    index
        .add_all(["*"], IndexAddOption::DEFAULT, None)
        .expect("stage all fixture changes");
    index.write().expect("write fixture index");
}

/// Commit the current index on `HEAD` with the fixed fixture signature.
pub fn commit_staged(dir: &Path, message: &str) -> Oid {
    let repo = open(dir);
    let mut index = repo.index().expect("fixture index");
    let tree_id = index.write_tree().expect("write fixture tree");
    let tree = repo.find_tree(tree_id).expect("fixture tree");
    let signature = signature();
    let parents = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<&Commit<'_>> = parents.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap_or_else(|error| panic!("commit {message}: {error}"))
}

/// Write, stage, and commit one file under `dir`.
pub fn commit_file(dir: &Path, name: &str, body: &str, message: &str) -> Oid {
    std::fs::write(dir.join(name), body).unwrap_or_else(|error| panic!("write {name}: {error}"));
    stage(dir, &[name]);
    commit_staged(dir, message)
}

/// Write, stage, and commit one file with the message `add <name>`.
pub fn commit(dir: &Path, name: &str, body: &str) -> Oid {
    commit_file(dir, name, body, &format!("add {name}"))
}

/// Add a remote pointing at the local `url` path.
pub fn add_remote(dir: &Path, name: &str, url: &Path) {
    let repo = open(dir);
    repo.remote(name, url.to_str().expect("utf-8 remote path"))
        .unwrap_or_else(|error| panic!("add remote {name}: {error}"));
}

/// Push a local branch to its bare remote and record its upstream, like `git push -u`.
pub fn push_branch(dir: &Path, remote_name: &str, branch: &str) {
    let repo = open(dir);
    let mut remote = repo
        .find_remote(remote_name)
        .unwrap_or_else(|error| panic!("find remote {remote_name}: {error}"));
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], None)
        .unwrap_or_else(|error| panic!("push {branch}: {error}"));
    let oid = repo
        .refname_to_id(&format!("refs/heads/{branch}"))
        .unwrap_or_else(|error| panic!("branch {branch}: {error}"));
    repo.reference(
        &format!("refs/remotes/{remote_name}/{branch}"),
        oid,
        true,
        "fixture push",
    )
    .expect("write remote-tracking ref");
    config_set(&repo, &format!("branch.{branch}.remote"), remote_name);
    config_set(
        &repo,
        &format!("branch.{branch}.merge"),
        &format!("refs/heads/{branch}"),
    );
}

/// Clone the bare `remote` into `into`.
pub fn clone(remote: &Path, into: &Path) -> Repository {
    Repository::clone(remote.to_str().expect("utf-8 remote path"), into)
        .unwrap_or_else(|error| panic!("clone: {error}"))
}

/// Create a branch at `from` (a revision the repository can resolve).
pub fn create_branch(dir: &Path, branch: &str, from: &str) {
    let repo = open(dir);
    let commit = repo
        .revparse_single(from)
        .unwrap_or_else(|error| panic!("rev-parse {from}: {error}"))
        .peel_to_commit()
        .expect("branch base commit");
    repo.branch(branch, &commit, false)
        .unwrap_or_else(|error| panic!("create branch {branch}: {error}"));
}

/// Check out an existing local branch.
pub fn checkout(dir: &Path, branch: &str) {
    let repo = open(dir);
    repo.set_head(&format!("refs/heads/{branch}"))
        .unwrap_or_else(|error| panic!("set head {branch}: {error}"));
    repo.checkout_head(Some(CheckoutBuilder::new().force()))
        .unwrap_or_else(|error| panic!("checkout {branch}: {error}"));
}

/// Create a branch at `HEAD` and check it out, like `git checkout -b`.
pub fn checkout_new(dir: &Path, branch: &str) {
    let repo = open(dir);
    let commit = repo
        .head()
        .unwrap_or_else(|error| panic!("head for {branch}: {error}"))
        .peel_to_commit()
        .expect("head commit");
    repo.branch(branch, &commit, false)
        .unwrap_or_else(|error| panic!("create branch {branch}: {error}"));
    checkout(dir, branch);
}

/// Add a linked worktree at `path` for the existing local `branch`.
pub fn worktree_add(dir: &Path, branch: &str, path: &Path) {
    let repo = open(dir);
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("worktree path has a name");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create worktree parent");
    }
    let reference = repo
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap_or_else(|error| panic!("worktree branch {branch}: {error}"));
    let mut options = WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repo.worktree(name, path, Some(&options))
        .unwrap_or_else(|error| panic!("add worktree {name}: {error}"));
}

/// Remove a linked worktree and its metadata.
pub fn worktree_remove(dir: &Path, path: &Path) {
    let repo = open(dir);
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("worktree path has a name");
    let worktree = repo
        .find_worktree(name)
        .unwrap_or_else(|error| panic!("find worktree {name}: {error}"));
    let mut options = WorktreePruneOptions::new();
    options.valid(true).locked(true).working_tree(true);
    worktree
        .prune(Some(&mut options))
        .unwrap_or_else(|error| panic!("remove worktree {name}: {error}"));
}

/// List the linked worktrees, one `worktree <name>` line each.
pub fn worktree_list_porcelain(dir: &Path) -> String {
    let repo = open(dir);
    let names = repo.worktrees().expect("list worktrees");
    let mut out = String::new();
    for name in names.iter() {
        if let Ok(Some(name)) = name {
            out.push_str(&format!("worktree {name}\n"));
        }
    }
    out
}

/// Resolve a revision to its full object id.
pub fn rev_parse(dir: &Path, spec: &str) -> String {
    open(dir)
        .revparse_single(spec)
        .unwrap_or_else(|error| panic!("rev-parse {spec}: {error}"))
        .id()
        .to_string()
}

/// Whether a reference exists.
pub fn show_ref_exists(dir: &Path, name: &str) -> bool {
    open(dir).find_reference(name).is_ok()
}

/// The symbolic target of a reference, such as `refs/heads/main` for `HEAD`.
pub fn symbolic_ref(dir: &Path, name: &str) -> Option<String> {
    open(dir)
        .find_reference(name)
        .ok()?
        .symbolic_target()
        .ok()?
        .map(str::to_owned)
}

/// Create a direct reference without force, like `git update-ref`.
pub fn create_ref(dir: &Path, name: &str, target: &str) {
    let repo = open(dir);
    let oid = Oid::from_str(target).unwrap_or_else(|error| panic!("oid {target}: {error}"));
    repo.reference(name, oid, false, "fixture ref")
        .unwrap_or_else(|error| panic!("create ref {name}: {error}"));
}

/// Reset `HEAD` and the worktree to `rev`, like `git reset --hard`.
pub fn reset_hard(dir: &Path, rev: &str) {
    let repo = open(dir);
    let object = repo
        .revparse_single(rev)
        .unwrap_or_else(|error| panic!("rev-parse {rev}: {error}"));
    repo.reset(&object, git2::ResetType::Hard, None)
        .unwrap_or_else(|error| panic!("reset --hard {rev}: {error}"));
}

/// The current status as porcelain-like `<index><worktree> <path>` lines.
pub fn status_porcelain(dir: &Path) -> String {
    let repo = open(dir);
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut options)).expect("fixture status");
    let mut out = String::new();
    for entry in statuses.iter() {
        let status = entry.status();
        if status.is_empty() {
            continue;
        }
        let (index, worktree) = if status.is_wt_new() {
            ('?', '?')
        } else if status.is_conflicted() {
            ('U', 'U')
        } else {
            (
                if status.is_index_new() {
                    'A'
                } else if status.is_index_modified() {
                    'M'
                } else if status.is_index_deleted() {
                    'D'
                } else if status.is_index_renamed() {
                    'R'
                } else if status.is_index_typechange() {
                    'T'
                } else {
                    ' '
                },
                if status.is_wt_modified() {
                    'M'
                } else if status.is_wt_deleted() {
                    'D'
                } else if status.is_wt_renamed() {
                    'R'
                } else if status.is_wt_typechange() {
                    'T'
                } else {
                    ' '
                },
            )
        };
        out.push_str(&format!(
            "{index}{worktree} {}\n",
            entry.path().unwrap_or("<non-utf8 path>")
        ));
    }
    out
}

/// The subject line of the commit at `HEAD`.
pub fn head_subject(dir: &Path) -> String {
    open(dir)
        .head()
        .expect("fixture head")
        .peel_to_commit()
        .expect("fixture head commit")
        .summary()
        .expect("fixture head summary")
        .unwrap_or_default()
        .to_owned()
}

/// A short one-line log of `HEAD`, newest first.
pub fn log_oneline(dir: &Path, limit: usize) -> String {
    let repo = open(dir);
    let mut walk = repo.revwalk().expect("fixture revwalk");
    if walk.push_head().is_err() {
        return String::new();
    }
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid.expect("fixture revwalk entry");
        let summary = repo
            .find_commit(oid)
            .ok()
            .and_then(|commit| commit.summary().ok().flatten().map(str::to_owned))
            .unwrap_or_default();
        out.push(format!("{} {summary}", &oid.to_string()[..7]));
    }
    out.join("\n")
}

/// The blobs reachable from the `HEAD` tree, one path per line.
pub fn ls_tree(dir: &Path) -> String {
    let repo = open(dir);
    let tree = repo
        .head()
        .expect("fixture head")
        .peel_to_tree()
        .expect("fixture head tree");
    let mut names = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            names.push(format!("{root}{}", entry.name().unwrap_or("")));
        }
        git2::TreeWalkResult::Ok
    })
    .expect("walk fixture tree");
    names.join("\n")
}

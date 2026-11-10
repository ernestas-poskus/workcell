//! Validated primitive types shared by the workcell modules.
//!
//! These are deliberately small newtypes over the strings, paths, and numbers the Git, process,
//! launch, and verification APIs exchange. Constructors are fallible; a value that exists has
//! passed the invariant its operation relies on. Opaque payloads (diff text, output, reasons) stay
//! `String`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

macro_rules! path_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(PathBuf);

        impl $name {
            /// The underlying path.
            #[must_use]
            pub fn as_path(&self) -> &Path {
                &self.0
            }

            /// Consume this value.
            #[must_use]
            pub fn into_path_buf(self) -> PathBuf {
                self.0
            }
        }

        impl AsRef<Path> for $name {
            fn as_ref(&self) -> &Path {
                self.as_path()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.display().fmt(f)
            }
        }
    };
}

path_newtype!(
    RepoRoot,
    "A repository root, relative or absolute; existence is checked by the operation."
);
path_newtype!(AbsolutePath, "An absolute filesystem path.");

impl RepoRoot {
    /// Wrap a repository root.
    ///
    /// # Errors
    ///
    /// An empty path is refused.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(Error::Path {
                detail: "repository root is empty".to_owned(),
            });
        }
        Ok(Self(path))
    }
}

impl AbsolutePath {
    /// Wrap an absolute path.
    ///
    /// # Errors
    ///
    /// Relative or empty paths are refused.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(Error::Path {
                detail: format!("path is not absolute: {}", path.display()),
            });
        }
        Ok(Self(path))
    }
}

/// A path below a checkout root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelativePath(PathBuf);

impl RelativePath {
    /// Validate a relative path.
    ///
    /// Absolute paths, empty paths, NUL bytes, parent traversal, and prefix/root components are
    /// refused. Symlink confinement remains an operation-time check.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(Error::Path {
                detail: "relative path is empty".to_owned(),
            });
        }
        if path.is_absolute() {
            return Err(Error::Path {
                detail: format!("path is absolute: {}", path.display()),
            });
        }
        for component in path.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(Error::Path {
                    detail: format!("unsafe relative path: {}", path.display()),
                });
            }
        }
        Ok(Self(path))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume this value.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for RelativePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

/// An absolute worktree path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorktreePath(PathBuf);

impl WorktreePath {
    /// Wrap an absolute worktree path.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(Error::Path {
                detail: format!("worktree path is not absolute: {}", path.display()),
            });
        }
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(Error::Path {
                detail: format!("worktree path contains `..`: {}", path.display()),
            });
        }
        Ok(Self(path))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume this value.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for WorktreePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for WorktreePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

/// A worktree root, relative to the repository or absolute.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorktreeRoot(PathBuf);

impl WorktreeRoot {
    /// Validate a worktree root.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(Error::Path {
                detail: "worktree root is empty".to_owned(),
            });
        }
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(Error::Path {
                detail: format!("worktree root contains `..`: {}", path.display()),
            });
        }
        Ok(Self(path))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume this value.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for WorktreeRoot {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// A Git metadata directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitDirectory(PathBuf);

impl GitDirectory {
    /// Wrap an absolute Git directory.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        AbsolutePath::new(path).map(|path| Self(path.into_path_buf()))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for GitDirectory {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// The repository's common Git directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitCommonDirectory(PathBuf);

impl GitCommonDirectory {
    /// Wrap an absolute common directory.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        AbsolutePath::new(path).map(|path| Self(path.into_path_buf()))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for GitCommonDirectory {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// The log path for one child process.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogPath(PathBuf);

impl LogPath {
    /// Wrap an absolute log path.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        AbsolutePath::new(path).map(|path| Self(path.into_path_buf()))
    }

    /// The underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume this value.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for LogPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

macro_rules! string_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume this value.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_newtype!(
    BranchName,
    "A branch name validated as a Git branch reference."
);
string_newtype!(BranchPrefix, "A safe prefix for generated branch names.");
string_newtype!(RemoteName, "A configured Git remote name.");
string_newtype!(
    ReferenceName,
    "A fully qualified reference name, or `HEAD`."
);
string_newtype!(Revision, "A Git revision expression.");
string_newtype!(CommitId, "A 40- or 64-hex object id.");
string_newtype!(Digest, "A 64-character lowercase hex digest.");
string_newtype!(
    WorktreeName,
    "A filesystem- and ref-safe worktree component."
);
string_newtype!(CheckName, "A human-readable verification check name.");
string_newtype!(CheckId, "A caller-assigned verification check id.");

fn reject_control(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) || value.contains('\0') {
        return Err(Error::Config {
            detail: format!("{what} is empty or contains a control character"),
        });
    }
    Ok(())
}

impl BranchName {
    /// Validate a branch name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "branch name")?;
        if !git2::Reference::is_valid_name(&format!("refs/heads/{value}")) {
            return Err(Error::Config {
                detail: format!("invalid branch name: {value}"),
            });
        }
        Ok(Self(value))
    }
}

impl BranchPrefix {
    /// Validate a branch prefix.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "branch prefix")?;
        let trimmed = value.trim_end_matches('/');
        if trimmed.is_empty() || trimmed.contains("..") || trimmed.starts_with('-') {
            return Err(Error::Config {
                detail: format!("invalid branch prefix: {value}"),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl RemoteName {
    /// Validate a remote name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "remote name")?;
        if value.contains('/') || value.chars().any(char::is_whitespace) {
            return Err(Error::Config {
                detail: format!("invalid remote name: {value}"),
            });
        }
        Ok(Self(value))
    }
}

impl ReferenceName {
    /// Validate a reference name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "reference name")?;
        if value != "HEAD" && !git2::Reference::is_valid_name(&value) {
            return Err(Error::Config {
                detail: format!("invalid reference name: {value}"),
            });
        }
        Ok(Self(value))
    }
}

impl Revision {
    /// Validate a revision expression. The expression itself is intentionally opaque.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "revision")?;
        if value.len() > 1024 {
            return Err(Error::Config {
                detail: "revision is longer than 1024 bytes".to_owned(),
            });
        }
        Ok(Self(value))
    }
}

impl CommitId {
    /// Validate a commit id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if !(value.len() == 40 || value.len() == 64)
            || !value.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(Error::Config {
                detail: format!("invalid commit id: {value}"),
            });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl Digest {
    /// Validate a SHA-256 hex digest.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Config {
                detail: format!("invalid digest: {value}"),
            });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl WorktreeName {
    /// Validate a worktree component.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "worktree name")?;
        if value.contains('/') || value == "." || value == ".." {
            return Err(Error::Config {
                detail: format!("invalid worktree name: {value}"),
            });
        }
        Ok(Self(value))
    }
}

impl CheckName {
    /// A synthetic or empty check name, used for the empty-command record.
    #[must_use]
    pub fn synthetic(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl CheckId {
    /// A caller-assigned id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "check id")?;
        Ok(Self(value))
    }
}

/// One process argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Arg(String);

impl Arg {
    /// Validate an argument.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.contains('\0') {
            return Err(Error::Config {
                detail: "argument contains NUL".to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// The argument text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Arg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A complete argv. Empty argv is allowed at the type level so verification can report an empty
/// declared command as a failed check; the process runner still refuses to spawn it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Argv(Vec<Arg>);

impl Argv {
    /// Build an argv.
    pub fn new<I, S>(values: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        values
            .into_iter()
            .map(|value| Arg::new(value.into()))
            .collect::<Result<Vec<_>>>()
            .map(Self)
    }

    /// The arguments.
    #[must_use]
    pub fn as_slice(&self) -> &[Arg] {
        &self.0
    }

    /// Whether there are no arguments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of arguments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Consume into owned arguments.
    #[must_use]
    pub fn into_vec(self) -> Vec<Arg> {
        self.0
    }

    /// Render as the strings the process launcher needs.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        self.0.iter().map(|arg| arg.0.clone()).collect()
    }
}

/// One environment variable name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvName(String);

impl EnvName {
    /// Validate a variable name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.contains('=') || value.contains('\0') {
            return Err(Error::Config {
                detail: format!("invalid environment variable name: {value}"),
            });
        }
        Ok(Self(value))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One environment variable value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvValue(String);

impl EnvValue {
    /// Validate a value.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.contains('\0') {
            return Err(Error::Config {
                detail: "environment value contains NUL".to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// The value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated environment map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env(BTreeMap<EnvName, EnvValue>);

impl Env {
    /// Validate a map of names and values.
    pub fn new(
        values: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Result<Self> {
        let mut map = BTreeMap::new();
        for (name, value) in values {
            map.insert(EnvName::new(name.into())?, EnvValue::new(value.into())?);
        }
        Ok(Self(map))
    }

    /// An empty environment.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over entries.
    pub fn iter(&self) -> impl Iterator<Item = (&EnvName, &EnvValue)> {
        self.0.iter()
    }

    /// Look up one variable by its raw name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&EnvValue> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (candidate.as_str() == name).then_some(value))
    }

    /// Insert a validated entry.
    pub fn insert(&mut self, name: EnvName, value: EnvValue) {
        self.0.insert(name, value);
    }

    /// Render as the strings the process launcher needs.
    #[must_use]
    pub fn to_strings(&self) -> BTreeMap<String, String> {
        self.0
            .iter()
            .map(|(name, value)| (name.0.clone(), value.0.clone()))
            .collect()
    }
}

impl From<BTreeMap<String, String>> for Env {
    fn from(values: BTreeMap<String, String>) -> Self {
        let mut env = Self::default();
        for (name, value) in values {
            if let (Ok(name), Ok(value)) = (EnvName::new(name), EnvValue::new(value)) {
                env.0.insert(name, value);
            }
        }
        env
    }
}

/// A wall-clock timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timeout(Duration);

impl Timeout {
    /// Wrap a timeout. Zero keeps the immediate-deadline behavior.
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self(duration)
    }

    /// The duration.
    #[must_use]
    pub fn as_duration(self) -> Duration {
        self.0
    }
}

/// Milliseconds spent doing work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DurationMillis(u64);

impl DurationMillis {
    /// Wrap a millisecond count.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// The count.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Saturating conversion from a standard duration.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        Self(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }
}

/// A byte cap. Zero is meaningful (“keep none”).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ByteLimit(usize);

impl ByteLimit {
    /// Wrap a byte cap.
    #[must_use]
    pub fn new(value: usize) -> Self {
        Self(value)
    }

    /// The cap.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for ByteLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A byte quantity. Zero is allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ByteSize(u64);

impl ByteSize {
    /// Wrap a byte quantity.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// The quantity.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A child process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExitCode(i32);

impl ExitCode {
    /// Wrap an exit code.
    #[must_use]
    pub fn new(value: i32) -> Self {
        Self(value)
    }

    /// The code.
    #[must_use]
    pub fn get(self) -> i32 {
        self.0
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A process id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessId(u32);

impl ProcessId {
    /// Wrap a pid.
    #[must_use]
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// The pid.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Seconds(u64);

impl Seconds {
    /// Wrap a second count.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// The count.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A revision range for a diff, rendered as the resolved text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionRange(String);

impl RevisionRange {
    /// Wrap a rendered range.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        reject_control(&value, "revision range")?;
        Ok(Self(value))
    }

    /// The range text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A Git operation, used as the typed operation label in errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitOperation {
    /// `rev-parse HEAD`.
    Head,
    /// `rev-parse --abbrev-ref HEAD`.
    BranchName,
    /// `rev-parse <revision>`.
    RevParse,
    /// `show-ref`.
    ShowRef,
    /// `merge-base --is-ancestor`.
    IsAncestor,
    /// `merge --ff-only`.
    MergeFfOnly,
    /// `merge`.
    Merge,
    /// `merge-base`.
    MergeBase,
    /// `rebase`.
    Rebase,
    /// `rebase --abort`.
    RebaseAbort,
    /// `reset --hard`.
    ResetHard,
    /// `update-ref`.
    UpdateRef,
    /// `fetch`.
    Fetch,
    /// `worktree list`.
    ListWorktrees,
    /// Branches with a prefix.
    BranchesWithPrefix,
    /// `worktree add -b`.
    WorktreeAdd,
    /// `worktree add` an existing branch.
    WorktreeAddExisting,
    /// `worktree remove`.
    WorktreeRemove,
    /// `worktree prune`.
    WorktreePrune,
    /// Branch delete.
    BranchDelete,
    /// `status`.
    Status,
    /// `status` for tracked changes.
    TrackedStatus,
    /// Commit all changes.
    CommitAll,
    /// Diff.
    Diff,
    /// Changed files between commits.
    ChangedFilesBetween,
    /// Commit count between commits.
    CommitCountBetween,
    /// Git directories.
    GitDirectories,
    /// Worktree config digest.
    WorktreeConfigDigest,
    /// Local config digest.
    LocalConfigDigest,
    /// A legacy or not-yet-classified operation label.
    Other(&'static str),
}

impl GitOperation {
    /// The stable operator-facing label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "git rev-parse HEAD",
            Self::BranchName => "git rev-parse --abbrev-ref HEAD",
            Self::RevParse => "git rev-parse",
            Self::ShowRef => "git show-ref",
            Self::IsAncestor => "git merge-base --is-ancestor",
            Self::MergeFfOnly => "git merge --ff-only",
            Self::Merge => "git merge",
            Self::MergeBase => "git merge-base",
            Self::Rebase => "git rebase",
            Self::RebaseAbort => "git rebase --abort",
            Self::ResetHard => "git reset --hard",
            Self::UpdateRef => "git update-ref",
            Self::Fetch => "git fetch",
            Self::ListWorktrees => "git worktree list",
            Self::BranchesWithPrefix => "git branch --list",
            Self::WorktreeAdd => "git worktree add",
            Self::WorktreeAddExisting => "git worktree add",
            Self::WorktreeRemove => "git worktree remove",
            Self::WorktreePrune => "git worktree prune",
            Self::BranchDelete => "git branch -D",
            Self::Status => "git status",
            Self::TrackedStatus => "git status --untracked-files=no",
            Self::CommitAll => "git commit",
            Self::Diff => "git diff",
            Self::ChangedFilesBetween => "git diff --name-only",
            Self::CommitCountBetween => "git rev-list --count",
            Self::GitDirectories => "git rev-parse --git-dir",
            Self::WorktreeConfigDigest => "git config digest",
            Self::LocalConfigDigest => "git config digest",
            Self::Other(label) => label,
        }
    }
}

impl From<&'static str> for GitOperation {
    fn from(value: &'static str) -> Self {
        Self::Other(value)
    }
}

impl fmt::Display for GitOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

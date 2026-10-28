//! The OS sandbox around a coding agent: the binary prerequisite, the profile, and the launch.
//!
//! The sandbox is not optional. [`Sandbox::resolve`] fails when the sandbox binary is not on
//! `PATH`, and [`Sandbox::preflight`] fails when this host cannot enforce a sandbox, so a caller
//! cannot accidentally run an agent unconfined: there is no configuration switch and no fallback
//! path that runs the child directly.
//!
//! The policy is a pair of reviewed, embedded profiles (agent and review) plus per-run grants the
//! caller supplies in [`SandboxSpec`]. The agent profile makes the run's checkout writable, leaves
//! the repository's execution-semantics paths (`.git/hooks`, `.git/config`, `.git/info`), the
//! control plane's state directory, and agent instruction/config files unwritable or absent,
//! leaves host credential stores denied, and does not restrict egress. The review profile makes
//! the checkout read-only.
//!
//! The profiles are compiled into this crate and rendered with [`SandboxOptions`], then published
//! content-addressed under the caller's profile directory, so concurrent runs share one reviewed
//! copy and an edited copy is refused rather than silently trusted. Rendering happens before the
//! content address is computed, so the digest covers the bytes `nono` will actually enforce.
//! Preflight also proves that the sandbox binary's own state root
//! (`$XDG_STATE_HOME/nono`, or `$HOME/.local/state/nono`) is usable, because `nono` refuses to
//! start when it cannot write its audit state.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command as TokioCommand;

use crate::error::{Error, Result};
use crate::git::{Git, GitDirectories};
use crate::worktree::Worktree;

/// The oldest sandbox binary this crate's launch contract is written against.
pub const MIN_NONO_VERSION: &str = "0.80.0";

/// The same floor as a comparable tuple, for version checks.
const MIN_NONO_VERSION_PARTS: (u64, u64, u64) = (0, 80, 0);

/// The sandbox binary's name as it is looked up on `PATH`.
const SANDBOX_BINARY: &str = "nono";

/// The embedded agent profile: a writable checkout, toolchain reads, denied credentials.
const AGENT_PROFILE: &str = include_str!("sandbox/profiles/agent.profile.json");

/// The embedded review profile: the same policy with a read-only checkout.
const REVIEW_PROFILE: &str = include_str!("sandbox/profiles/review.profile.json");

/// The in-checkout state directory both embedded profiles deny by default.
///
/// A caller that keeps its state under a different directory below the run's checkout passes its
/// own value in [`SandboxOptions::state_dir`] instead of asking for a hand-edited profile: the
/// reviewed profile text is compiled in, and only this one path segment is substituted.
pub const DEFAULT_STATE_DIR: &str = ".workcell";

/// The token the embedded profiles use for the caller's state directory.
///
/// It appears in the six state-path denials (`state.json`, `logs`, `runs/*.log`, `control`,
/// `sandbox`, `integration.lock`) and nowhere else. The artifact paths the caller grants per run
/// (for example a directory holding a brief or a review) are deliberately not covered, so a
/// caller can still hand the agent a specific file inside the state directory.
const STATE_DIR_TOKEN: &str = "__STATE_DIR__";

/// How many hex characters of the profile digest name a published profile.
const PROFILE_HASH_CHARS: usize = 16;

/// The most operator-facing diagnostic text one error keeps.
const MAX_DIAGNOSTIC_CHARS: usize = 1_200;

/// How many bytes of one hook entry the shared-checkout guard hashes.
///
/// A hook is normally a short script; the bound keeps a hostile repository from turning the
/// snapshot into an unbounded read. The entry's full length is hashed too, so appending or
/// truncating beyond the bound still changes the digest.
const HOOK_CONTENT_LIMIT: u64 = 64 * 1024;

/// The most hooks entries the shared-checkout guard will hash.
const MAX_HOOK_ENTRIES: usize = 256;

/// How many bytes of one `.git/info` entry the shared-checkout guard hashes.
const INFO_CONTENT_LIMIT: u64 = 64 * 1024;

/// The most `.git/info` entries the shared-checkout guard will hash.
const MAX_INFO_ENTRIES: usize = 256;

/// The most bytes a published profile may occupy. A profile is a few KiB; anything larger is a
/// substituted file or a device, and reading it would be unbounded.
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

/// The most bytes the `.git` pointer file may occupy. Worktree mode writes a single `gitdir:`
/// line there, and the file sits inside the writable checkout, so an agent can grow it.
const MAX_DOT_GIT_ENTRY_BYTES: u64 = 64 * 1024;

/// How long reading a published profile may take before it is treated as an unusable target.
///
/// A pre-created FIFO at the content-addressed path is refused by the regular-file check; this
/// deadline covers the narrower race where the path is swapped between the check and the read,
/// so a substitution cannot hang a launch.
const PROFILE_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest symlink target the shared-checkout guard will hash. A hook can be a symlink, and
/// the target Git would execute is what matters; a longer target is treated as unreadable.
const MAX_SYMLINK_TARGET_BYTES: usize = 4096;

/// How long the `nono --version` probe may take.
const VERSION_PROBE_TIMEOUT_SECS: u64 = 30;

/// How long the host-support probe (`nono setup --check-only`) may take.
const HOST_PROBE_TIMEOUT_SECS: u64 = 60;

/// How long one `nono profile validate` may take.
const PROFILE_VALIDATE_TIMEOUT_SECS: u64 = 30;

/// The prefix that versions a [`Sandbox::guard_snapshot`] string.
const GUARD_SNAPSHOT_PREFIX: &str = "v4";

/// Makes temporary file names unique inside one process.
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Severity of one [`DoctorLine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorLevel {
    /// Healthy.
    Ok,
    /// Usable but worth an operator's attention.
    Warn,
    /// Dispatch must not start.
    Fail,
}

/// One line of sandbox diagnostics, as `doctor`-style commands print them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorLine {
    /// How serious the line is.
    pub level: DoctorLevel,
    /// The human-facing text.
    pub text: String,
}

impl DoctorLine {
    /// A healthy line.
    fn ok(text: impl Into<String>) -> Self {
        Self {
            level: DoctorLevel::Ok,
            text: text.into(),
        }
    }

    /// A usable-but-noteworthy line.
    fn warn(text: impl Into<String>) -> Self {
        Self {
            level: DoctorLevel::Warn,
            text: text.into(),
        }
    }

    /// A launch-blocking line.
    fn fail(text: impl Into<String>) -> Self {
        Self {
            level: DoctorLevel::Fail,
            text: text.into(),
        }
    }
}

impl SandboxSpec<'_> {
    /// Refuse a spec whose grants would resolve somewhere other than where the caller thinks.
    ///
    /// `nono` resolves a relative grant against the child's cwd, and the child's cwd is the run's
    /// checkout (a per-run worktree in worktree mode), not the caller's. A relative grant therefore
    /// silently points at a different directory than the caller intended — a widening in the wrong
    /// place. Every grant must be absolute; existence is left to `nono`, which reports it precisely.
    fn validate(&self) -> Result<()> {
        if !self.checkout.is_absolute() {
            return Err(Error::Sandbox {
                detail: format!(
                    "the sandbox checkout `{}` is not absolute; `nono` resolves a relative path against the child's cwd, so a relative checkout is refused",
                    self.checkout.display()
                ),
            });
        }
        for (label, path) in self
            .read_paths
            .iter()
            .map(|path| ("read", *path))
            .chain(self.read_files.iter().map(|path| ("read-file", *path)))
            .chain(self.writable_paths.iter().map(|path| ("writable", *path)))
        {
            if !path.is_absolute() {
                return Err(Error::Sandbox {
                    detail: format!(
                        "the sandbox {label} grant `{}` is not absolute; `nono` resolves a relative grant against the child's cwd, so it is refused",
                        path.display()
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The runtime facts one sandboxed launch needs.
#[derive(Debug, Clone)]
pub struct SandboxSpec<'a> {
    /// The run's checkout (the working tree the agent edits).
    pub checkout: &'a Path,
    /// The prepared worktree, when the caller isolates each run in one.
    pub worktree: Option<&'a Worktree>,
    /// Extra directories the child may read (for example the directory holding its brief).
    pub read_paths: Vec<&'a Path>,
    /// Extra directories the child may write (for example a per-run harness home outside the
    /// checkout). These are passed as `--allow`, so they carry read+write; a caller that only
    /// needs reads belongs in [`SandboxSpec::read_paths`].
    pub writable_paths: Vec<&'a Path>,
    /// Extra files the child may read (for example its MCP configuration and CA certificate).
    pub read_files: Vec<&'a Path>,
    /// Loopback ports the child may reach (for example a local compression proxy).
    pub loopback_ports: Vec<u16>,
    /// Loopback ports the child may **listen** on without reaching them.
    ///
    /// Measured: some harnesses need an arbitrary-port bind but not the connect half, and
    /// `--listen-port` grants the bind alone. A caller that does not know should grant
    /// [`SandboxSpec::loopback_ports`] instead, which is the wider capability.
    pub listen_ports: Vec<u16>,
}

/// What [`Sandbox::resolve_with`] renders the embedded profiles with.
///
/// The profiles are reviewed policy, compiled into the crate; the state directory is the one
/// value that differs between callers. It is a *path segment*, not a grant: the six entries it
/// completes stay denials, and the artifact paths a caller may grant inside the directory are
/// unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxOptions {
    /// The caller's in-checkout state directory, relative to the run's checkout.
    ///
    /// Defaults to [`DEFAULT_STATE_DIR`] (`.workcell`). It must be a relative path of plain
    /// components: no leading `/`, no `.` or `..` component, no empty component, no whitespace,
    /// and only ASCII letters, digits, `-`, `_`, `.` and `/`. A value that cannot name a directory
    /// safely is refused at [`Sandbox::resolve_with`] rather than rendered into a policy document.
    pub state_dir: String,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            state_dir: DEFAULT_STATE_DIR.to_owned(),
        }
    }
}

/// A resolved sandbox binary plus the content-addressed profile paths it publishes on preflight.
#[derive(Debug, Clone)]
pub struct Sandbox {
    binary: PathBuf,
    /// Filled by [`Sandbox::preflight`] (and by `doctor`); resolution itself spawns nothing.
    version: OnceLock<String>,
    profile_dir: PathBuf,
    agent_profile: PathBuf,
    review_profile: PathBuf,
    /// The rendered bytes `agent_profile` addresses. Preflight publishes these and the read-back
    /// checks compare against them, so the digest, the published bytes, and the verification all
    /// come from one rendering rather than three.
    agent_contents: String,
    /// The rendered bytes `review_profile` addresses.
    review_contents: String,
}

impl Sandbox {
    /// Resolve the sandbox binary on `PATH` and compute the content-addressed profile paths with
    /// the default [`SandboxOptions`].
    ///
    /// Resolution is deliberately a cheap, synchronous *lookup*: it stats `PATH`, renders the
    /// embedded profiles, and computes their content addresses, and it spawns no process and
    /// writes no file. Every side effect (version probe, profile publication, host proof) belongs
    /// to [`Sandbox::preflight`], which is asynchronous and runs on Tokio, so a runtime is never
    /// blocked by `nono` or by profile I/O.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] when `nono` is missing or is not executable; [`Error::Config`] when the
    /// profile directory cannot be made absolute.
    pub fn resolve(profile_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::resolve_with(profile_dir, SandboxOptions::default())
    }

    /// Resolve the sandbox binary on `PATH` and render the embedded profiles with `options`.
    ///
    /// The rendered bytes are the ones that are hashed into the content address, published on
    /// preflight, handed to the binary, and compared on read-back, so a custom state directory is
    /// covered by the digest instead of producing a profile that disagrees with its own name.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] when `nono` is missing or is not executable; [`Error::Config`] when
    /// [`SandboxOptions::state_dir`] cannot name a directory safely or the profile directory
    /// cannot be made absolute.
    pub fn resolve_with(profile_dir: impl Into<PathBuf>, options: SandboxOptions) -> Result<Self> {
        validate_state_dir(&options.state_dir)?;
        let agent_contents = render_profile(AGENT_PROFILE, &options.state_dir);
        let review_contents = render_profile(REVIEW_PROFILE, &options.state_dir);
        let binary = resolve_binary(SANDBOX_BINARY)?;
        let profile_dir = absolute_profile_dir(&profile_dir.into())?;
        Ok(Self {
            binary,
            version: OnceLock::new(),
            agent_profile: profile_path(&profile_dir, "agent", &agent_contents),
            review_profile: profile_path(&profile_dir, "review", &review_contents),
            profile_dir,
            agent_contents,
            review_contents,
        })
    }

    /// The resolved binary path.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The binary version, once [`Sandbox::preflight`] (or `doctor`) has probed it.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.get().map(String::as_str)
    }

    /// The content-addressed agent profile path. It exists after `preflight` has published it.
    #[must_use]
    pub fn agent_profile(&self) -> &Path {
        &self.agent_profile
    }

    /// The content-addressed review profile path. It exists after `preflight` has published it.
    #[must_use]
    pub fn review_profile(&self) -> &Path {
        &self.review_profile
    }

    /// The directory the profiles were published under.
    #[must_use]
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// Publish the embedded profiles and prove that this host can enforce them.
    ///
    /// Everything that touches the outside world is asynchronous and bounded: the profiles are
    /// published under `profile_dir`, `nono --version` is probed with a deadline, the host-support
    /// probe runs (`nono setup --check-only`; a nested sandbox cannot initialize a second one),
    /// both published profiles are validated, and the `nono` state root is proved writable. A
    /// binary that cannot write its audit state refuses to run, so that check is mandatory too.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] with operator-facing detail naming the likely cause: an unsupported
    /// host, a nested sandbox, an unusable or too-old binary, a rejected profile, or an unusable
    /// state root; [`Error::Io`] when the profile directory cannot be written.
    pub async fn preflight(&self) -> Result<()> {
        // The policy denies paths inside the writable checkout. macOS Seatbelt can express that
        // (a regex rule that also covers files created after start); Linux Landlock is a strict
        // allow-list with no deny primitive and refuses such a profile at startup. Running an
        // agent without a sandbox is not an option, so dispatch is refused here instead.
        #[cfg(not(target_os = "macos"))]
        {
            return Err(Error::Sandbox {
                detail: "the agent sandbox policy is written for macOS Seatbelt: it denies paths inside the writable checkout, and Linux's Landlock cannot enforce a deny inside a grant (nono refuses that profile at startup). Running an agent without a sandbox is not an option, so dispatch is refused on this platform".to_owned(),
            });
        }
        #[cfg(target_os = "macos")]
        {
            self.publish_profiles().await?;
            self.probe_version().await?;
            self.probe_host_support().await?;
            self.validate_profiles().await?;
            ensure_state_root_usable(&nono_state_location()?, true).await?;
            Ok(())
        }
    }

    /// Diagnostics for a caller's `doctor` command. Never fails; reports what it observes.
    ///
    /// Probes the binary (`--version`, bounded), re-reads the published profiles, and inspects
    /// the `nono` state path, so it is asynchronous like [`Sandbox::preflight`] but never
    /// publishes or writes anything itself.
    pub async fn doctor_lines(&self) -> Vec<DoctorLine> {
        let mut lines = Vec::with_capacity(6);

        #[cfg(not(target_os = "macos"))]
        lines.push(DoctorLine::fail(
            "the agent sandbox policy requires macOS Seatbelt; Linux's Landlock cannot enforce a deny inside the writable checkout, so dispatch is refused on this platform",
        ));

        if is_executable_file(&self.binary) {
            lines.push(DoctorLine::ok(format!(
                "sandbox binary: `{}`",
                self.binary.display()
            )));
        } else {
            lines.push(DoctorLine::fail(format!(
                "sandbox binary `{}` is missing or not executable; install nono {MIN_NONO_VERSION} or newer",
                self.binary.display()
            )));
        }

        match self.probe_version().await {
            Err(error) => lines.push(DoctorLine::fail(error.to_string())),
            Ok(version) => match parse_version(&version) {
                Some(parts) if parts > MIN_NONO_VERSION_PARTS => {
                    lines.push(DoctorLine::warn(format!(
                        "nono {version} is newer than the tested {MIN_NONO_VERSION}; re-run the sandbox drill after an upgrade"
                    )));
                }
                _ => lines.push(DoctorLine::ok(format!(
                    "sandbox version: nono {version} (tested)"
                ))),
            },
        }

        for (kind, path, expected) in [
            ("agent", &self.agent_profile, self.agent_contents.as_str()),
            (
                "review",
                &self.review_profile,
                self.review_contents.as_str(),
            ),
        ] {
            match tokio::fs::read(path).await {
                Ok(bytes) if bytes == expected.as_bytes() => lines.push(DoctorLine::ok(format!(
                    "{kind} sandbox profile: `{}`",
                    path.display()
                ))),
                Ok(_) => lines.push(DoctorLine::fail(format!(
                    "{kind} sandbox profile `{}` does not match the embedded profile; delete it and re-run `Sandbox::resolve`",
                    path.display()
                ))),
                Err(error) => lines.push(DoctorLine::fail(format!(
                    "{kind} sandbox profile `{}` cannot be read ({error}); re-run `Sandbox::resolve`",
                    path.display()
                ))),
            }
        }

        match nono_state_location() {
            Ok(location) => match ensure_state_root_usable(&location, false).await {
                Ok(()) => lines.push(DoctorLine::ok("nono state directory: usable")),
                Err(error) => lines.push(DoctorLine::fail(error.to_string())),
            },
            Err(error) => lines.push(DoctorLine::fail(error.to_string())),
        }

        lines
    }

    /// Wrap an agent invocation for the sandbox.
    ///
    /// The argv is deterministic: the same spec produces the same wrapper, so a launch can be
    /// logged and reproduced. The child's environment is the caller's business — this function
    /// only builds argv.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] for an empty `argv`; [`Error::Path`] when a path that must appear in the
    /// child's argv is not valid UTF-8.
    pub fn wrap_agent(&self, argv: Vec<String>, spec: &SandboxSpec<'_>) -> Result<Vec<String>> {
        spec.validate()?;
        self.wrap(argv, spec, &self.agent_profile, true, false)
    }

    /// Wrap a read-only review invocation for the sandbox.
    ///
    /// The review profile makes the checkout read-only and the wrapper grants no Git metadata
    /// paths, so a review cannot write or commit.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] for an empty `argv`; [`Error::Path`] when a path that must appear in the
    /// child's argv is not valid UTF-8.
    pub fn wrap_review(&self, argv: Vec<String>, spec: &SandboxSpec<'_>) -> Result<Vec<String>> {
        spec.validate()?;
        self.wrap(argv, spec, &self.review_profile, false, true)
    }

    /// Remove environment variables that would reconfigure the sandbox or inject code into it.
    ///
    /// Three case-sensitive prefixes are removed: `NONO_*` reconfigures nono itself, and `DYLD_*`
    /// and `LD_*` make the loader run code in whichever process loads them. That includes the
    /// **supervisor**: a constructor in `DYLD_INSERT_LIBRARIES` runs before any profile is
    /// applied, so scrubbing only the child would be too late. Everything else is left to the
    /// caller; the child is configured by the published profile and the wrapped argv.
    pub fn scrub_env(env: &mut BTreeMap<String, String>) {
        env.retain(|key, _| {
            !(key.starts_with("NONO_") || key.starts_with("DYLD_") || key.starts_with("LD_"))
        });
    }

    /// Whether a failed child is a sandbox refusal rather than an agent failure.
    ///
    /// The exact shape `nono` produces when it cannot apply a profile or start the child: empty
    /// stdout and a first non-empty stderr line beginning `nono:`. An agent's own permission
    /// denial, or any run that produced stdout, is the agent's failure.
    ///
    /// The prefix is matched at column zero: a line that merely contains `nono:` after other
    /// text, or after leading whitespace, is not the sandbox's own refusal shape and is left to
    /// the agent-failure path. Widening this test would turn agent failures into configuration
    /// failures; narrowing it would retry a refusal as a transient fault.
    #[must_use]
    pub fn classify_refusal(stdout: &str, stderr: &str) -> bool {
        if !stdout.trim().is_empty() {
            return false;
        }
        stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|line| line.starts_with("nono:"))
    }

    /// Snapshot the repository identity, configuration, hooks, and info files that an
    /// unsandboxed Git call must not see changed.
    ///
    /// The snapshot is a version tag plus four SHA-256 values: the resolved repository-local
    /// configuration (`git config --local --list --includes`), a framed listing of the `hooks`
    /// entries in the repository's common Git directory (name, kind, mode, full length, bounded
    /// contents; a symlink is followed so the target's bytes are what Git would execute), the
    /// repository identity (`--absolute-git-dir`, `--git-common-dir`, and the `.git` entry's
    /// kind/pointer bytes), and a framed listing of the common Git directory's `info` entries
    /// (name, kind, mode, full length, bounded contents; symlinks are not followed). Hashing
    /// contents as well as names catches an overwritten existing hook; hashing the repository
    /// identity catches a worktree whose `.git` pointer was repointed at a fake repository that
    /// copies the original configuration and hooks; hashing `.git/info` catches the attributes
    /// and exclude files that change what an unsandboxed Git invocation executes and ignores.
    /// Every surface is read without executing repository code.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] when Git cannot read the configuration or the common directory, when a
    /// guard surface directory cannot be listed or exceeds its entry/content caps, or when an
    /// entry name or symlink target is not valid UTF-8 — a guard that cannot observe fails closed
    /// instead of returning a retryable Git error.
    pub async fn guard_snapshot(git: &Git) -> Result<String> {
        let surfaces = guard_surfaces(git).await?;
        Ok(snapshot_text(&surfaces))
    }

    /// Refuse when the tracked Git surfaces changed since [`Sandbox::guard_snapshot`].
    ///
    /// Detection is the whole contract: this compares the four surfaces (repository-local
    /// configuration, the bounded hook and `.git/info` entries with their modes, and the repository
    /// identity) and refuses on any difference. It does **not** repair the checkout — restoring from
    /// a backup the same user (or the sandboxed child) could have written is a write path with its
    /// own authority, and the larger fix is to run in worktree mode, where `.git` is outside the
    /// writable checkout.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] naming the changed surface, when a surface differs; when the snapshot is
    /// not this guard's format; or when a surface cannot be read back at all — a guard that cannot
    /// observe fails closed instead of returning a retryable Git error.
    pub async fn guard_verify(git: &Git, expected: &str) -> Result<()> {
        let current = guard_surfaces(git).await?;
        if snapshot_text(&current) == expected {
            return Ok(());
        }

        let parsed = parse_guard_snapshot(expected).ok_or_else(invalid_guard_snapshot)?;
        let mut changed = changed_surfaces(&parsed, &current);
        if changed.is_empty() {
            changed
                .push("the tracked git configuration, hooks, info files, or repository identity");
        }

        Err(Error::Sandbox {
            detail: format!(
                "{} changed while the agent ran (shared-checkout `.git` guard); refusing to continue, because a hook or repository configuration could execute outside the sandbox — re-run under a fresh checkout, or in worktree mode where `.git` is outside the writable checkout",
                changed.join(" and ")
            ),
        })
    }

    /// Build the deterministic `nono` argv for one launch.
    fn wrap(
        &self,
        argv: Vec<String>,
        spec: &SandboxSpec<'_>,
        profile: &Path,
        git_grants: bool,
        allow_cwd: bool,
    ) -> Result<Vec<String>> {
        if argv.is_empty() {
            return Err(Error::Sandbox {
                detail: "refusing to build a sandboxed launch from an empty argv; the caller must supply the agent command".to_owned(),
            });
        }

        let binary = path_text(&self.binary, "the sandbox binary")?;
        let profile = path_text(profile, "the sandbox profile")?;
        let checkout = path_text(spec.checkout, "the run checkout")?;

        let mut wrapped = Vec::with_capacity(argv.len() + 24);
        wrapped.push(binary.to_owned());
        for flag in ["-s", "run", "--no-diagnostics"] {
            wrapped.push(flag.to_owned());
        }
        wrapped.push("--profile".to_owned());
        wrapped.push(profile.to_owned());
        wrapped.push("--workdir".to_owned());
        wrapped.push(checkout.to_owned());
        // With a read-only workdir, nono refuses a non-interactive child unless read access to
        // the current directory is granted explicitly. The review profile is read-only by
        // design, so every review launch must carry this or nono exits before the agent starts.
        if allow_cwd {
            wrapped.push("--allow-cwd".to_owned());
        }
        for path in &spec.writable_paths {
            wrapped.push("--allow".to_owned());
            wrapped.push(path_text(path, "a sandbox write grant")?.to_owned());
        }

        // `.git` is an allow-list: only the metadata Git itself needs to commit is writable, so
        // `hooks`, `config`, `info`, and every other worktree stay outside the grant.
        if git_grants && let Some(worktree) = spec.worktree {
            let common = &worktree.git_common_dir;
            wrapped.push("--read".to_owned());
            wrapped.push(path_text(common, "the worktree git common directory")?.to_owned());
            for metadata in ["objects", "refs", "logs"] {
                wrapped.push("--allow".to_owned());
                wrapped.push(path_text(&common.join(metadata), "a git metadata grant")?.to_owned());
            }
            wrapped.push("--allow".to_owned());
            wrapped.push(
                path_text(&worktree_git_dir_grant(worktree)?, "the worktree git grant")?.to_owned(),
            );
        }

        for path in &spec.read_paths {
            wrapped.push("--read".to_owned());
            wrapped.push(path_text(path, "a sandbox read grant")?.to_owned());
        }
        for path in &spec.read_files {
            wrapped.push("--read-file".to_owned());
            wrapped.push(path_text(path, "a sandbox read-file grant")?.to_owned());
        }
        for port in &spec.loopback_ports {
            wrapped.push("--open-port".to_owned());
            wrapped.push(port.to_string());
        }
        for port in &spec.listen_ports {
            wrapped.push("--listen-port".to_owned());
            wrapped.push(port.to_string());
        }

        wrapped.push("--".to_owned());
        wrapped.extend(argv);
        Ok(wrapped)
    }

    /// Publish both embedded profiles content-addressed, then verify the bytes on disk.
    ///
    /// A concurrent run may publish the same content first; the no-clobber write keeps the first
    /// copy and the verification below refuses a copy that is not the reviewed one.
    async fn publish_profiles(&self) -> Result<()> {
        publish_profile(
            &self.profile_dir,
            "agent",
            &self.agent_contents,
            &self.agent_profile,
        )
        .await?;
        publish_profile(
            &self.profile_dir,
            "review",
            &self.review_contents,
            &self.review_profile,
        )
        .await?;
        self.verify_profiles().await
    }

    /// Re-read the published profiles and refuse a copy that no longer matches the embedded,
    /// reviewed policy.
    ///
    /// Verify the published profiles still match the embedded policy, immediately before a launch.
    ///
    /// The names are content addresses, but the bytes are what `nono` will enforce, and a launch
    /// hands `nono` a *path*. Preflight verifies once; a writer with access to the profile
    /// directory could substitute a weaker file between preflight and launch. Re-running this
    /// immediately before spawning the child closes that window, and it reads through the same
    /// bounded path as publication, so a FIFO or device cannot stall it or widen the check.
    ///
    /// # Errors
    ///
    /// [`Error::Sandbox`] when a published profile is missing, is not a regular file under the
    /// read bound, or no longer matches the embedded bytes.
    pub async fn verify_profiles(&self) -> Result<()> {
        for (kind, path, expected) in [
            ("agent", &self.agent_profile, self.agent_contents.as_str()),
            (
                "review",
                &self.review_profile,
                self.review_contents.as_str(),
            ),
        ] {
            let published = read_published_profile(path, kind).await?.ok_or_else(|| {
                Error::Sandbox {
                    detail: format!(
                        "the published {kind} sandbox profile `{}` is missing; re-run preflight before launching an agent",
                        path.display()
                    ),
                }
            })?;
            if published != expected.as_bytes() {
                return Err(Error::Sandbox {
                    detail: format!(
                        "the published {kind} sandbox profile `{}` no longer matches the embedded reviewed policy; it is content-addressed, so this means the file was edited — delete it and re-run preflight before launching an agent",
                        path.display()
                    ),
                });
            }
        }
        Ok(())
    }

    /// Run `nono --version` with a deadline and require a supported semantic version.
    ///
    /// The probe is a Tokio child with `kill_on_drop`, an empty environment, and a timeout, so a
    /// hung or hostile binary cannot stall the runtime or inherit sandbox controls.
    async fn probe_version(&self) -> Result<String> {
        let version = probe_nono_version(&self.binary).await?;
        let _ = self.version.set(version.clone());
        Ok(version)
    }

    /// `nono setup --check-only`: the host-support probe.
    async fn probe_host_support(&self) -> Result<()> {
        let mut command = probe_command(&self.binary);
        command.arg("setup").arg("--check-only");
        let output = output_with_deadline(
            command,
            Duration::from_secs(HOST_PROBE_TIMEOUT_SECS),
            format!("`{} setup --check-only`", self.binary.display()),
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::Sandbox {
            detail: format!(
                "this host cannot enforce a nono sandbox (`nono setup --check-only` exited {}): {}; likely causes: an unsupported macOS or Linux version, a kernel without the required sandbox API, or a process that is already inside an OS sandbox — a nested sandbox cannot be initialized, so run the caller from a plain host shell",
                output.status,
                diagnostic_tail(
                    &String::from_utf8_lossy(&output.stdout),
                    &String::from_utf8_lossy(&output.stderr),
                ),
            ),
        })
    }

    /// `nono profile validate <published agent profile>`.
    async fn validate_profiles(&self) -> Result<()> {
        for (kind, profile) in [
            ("agent", &self.agent_profile),
            ("review", &self.review_profile),
        ] {
            validate_profile(&self.binary, kind, profile).await?;
        }
        Ok(())
    }
}

/// `nono profile validate <published profile>` for one embedded policy document.
async fn validate_profile(binary: &Path, kind: &str, profile: &Path) -> Result<()> {
    let mut command = probe_command(binary);
    command.arg("profile").arg("validate").arg(profile);
    let output = output_with_deadline(
        command,
        Duration::from_secs(PROFILE_VALIDATE_TIMEOUT_SECS),
        format!("`{} profile validate`", binary.display()),
    )
    .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Sandbox {
        detail: format!(
            "the embedded {kind} sandbox profile was rejected (`nono profile validate {}` exited {}): {}; the profile is compiled into workcell, so this usually means the installed nono does not support this profile schema (workcell requires nono {MIN_NONO_VERSION} or newer)",
            profile.display(),
            output.status,
            diagnostic_tail(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
            ),
        ),
    })
}

/// Where `nono` keeps its session and audit state.
struct StateLocation {
    /// The state directory itself: `<base>/nono`.
    root: PathBuf,
    /// The caller's state base: `$XDG_STATE_HOME` or `$HOME/.local/state`.
    base: PathBuf,
}

/// Resolve the sandbox binary, searching `PATH` for a bare name.
fn resolve_binary(name: &str) -> Result<PathBuf> {
    if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        let candidate = PathBuf::from(name);
        if is_executable_file(&candidate) {
            return absolute_binary(&candidate);
        }
        return Err(Error::Sandbox {
            detail: format!(
                "the sandbox binary `{}` is missing or not executable; the sandbox is a hard prerequisite (nono {MIN_NONO_VERSION} or newer)",
                candidate.display()
            ),
        });
    }

    let path = std::env::var_os("PATH").ok_or_else(|| Error::Sandbox {
        detail: format!(
            "`{name}` cannot be located because PATH is unset; the agent sandbox is a hard prerequisite — install nono {MIN_NONO_VERSION} or newer and ensure it resolves on PATH"
        ),
    })?;
    for directory in std::env::split_paths(&path) {
        let candidate = if directory.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            directory.join(name)
        };
        if is_executable_file(&candidate) {
            return absolute_binary(&candidate);
        }
    }
    Err(Error::Sandbox {
        detail: format!(
            "`{name}` was not found as an executable on PATH; the agent sandbox is a hard prerequisite — install nono {MIN_NONO_VERSION} or newer and ensure it is executable and resolves on PATH"
        ),
    })
}

/// A `nono` probe command with the environment made inert.
///
/// The probes decide "can this host enforce" and "is this profile valid", so they must not be
/// steerable by the caller's environment: `NONO_*` variables reconfigure nono, and `DYLD_*`/`LD_*`
/// inject code into the probe before it applies anything. Everything else (notably `HOME` and
/// `XDG_STATE_HOME`, which select nono's state root) is kept, because preflight proves that same
/// state root.
fn probe_command(binary: &Path) -> TokioCommand {
    let mut command = TokioCommand::new(binary);
    command.stdin(Stdio::null()).kill_on_drop(true);
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("NONO_") || key.starts_with("DYLD_") || key.starts_with("LD_") {
            command.env_remove(key.as_ref());
        }
    }
    command
}

/// Run a probe command with a deadline, mapping a timeout to a refusal.
async fn output_with_deadline(
    mut command: TokioCommand,
    deadline: Duration,
    label: String,
) -> Result<std::process::Output> {
    tokio::time::timeout(deadline, command.output())
        .await
        .map_err(|_| Error::Sandbox {
            detail: format!(
                "{label} did not finish within {}s; refusing to launch an agent behind an unproven sandbox",
                deadline.as_secs()
            ),
        })?
        .map_err(|error| Error::Sandbox {
            detail: format!("{label}: {error}; the sandbox binary is not usable"),
        })
}

/// Make a resolved binary path absolute, so a wrapped launch does not depend on its cwd.
fn absolute_binary(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::path::absolute(path).map_err(|error| Error::Sandbox {
        detail: format!(
            "could not resolve the sandbox binary path `{}` to an absolute path: {error}",
            path.display()
        ),
    })
}

/// Make the published-profile directory absolute.
///
/// The profile paths are computed at preflight and handed to `nono` at launch, but the child runs
/// with the checkout as its working directory. A relative directory would therefore name one path
/// to preflight (the caller's cwd) and another to `nono` (the checkout), so an agent that can write
/// the checkout could plant a weaker profile at the launch-resolved path.
fn absolute_profile_dir(directory: &Path) -> Result<PathBuf> {
    if directory.is_absolute() {
        return Ok(directory.to_path_buf());
    }
    std::path::absolute(directory).map_err(|error| Error::Sandbox {
        detail: format!(
            "could not resolve the sandbox profile directory `{}` to an absolute path: {error}; the published profile path must not depend on the caller's working directory",
            directory.display()
        ),
    })
}

/// Whether `path` is a regular file the current user may execute.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run `nono --version` and require a semantic version at or above [`MIN_NONO_VERSION`].
async fn probe_nono_version(binary: &Path) -> Result<String> {
    let mut command = TokioCommand::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .env_clear()
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(VERSION_PROBE_TIMEOUT_SECS), command.output())
        .await
        .map_err(|_| Error::Sandbox {
            detail: format!(
                "`{} --version` did not finish within {VERSION_PROBE_TIMEOUT_SECS}s; refusing to launch an agent against an unverified sandbox",
                binary.display()
            ),
        })?
        .map_err(|error| Error::Sandbox {
            detail: format!(
                "could not run `{} --version`: {error}; the sandbox binary is not usable — install nono {MIN_NONO_VERSION} or newer",
                binary.display()
            ),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(Error::Sandbox {
            detail: format!(
                "`{} --version` exited {}: {}; refusing to launch an agent against an unverified sandbox",
                binary.display(),
                output.status,
                diagnostic_tail(&stdout, &stderr)
            ),
        });
    }

    let Some(reported) = parse_version(&stdout).or_else(|| parse_version(&stderr)) else {
        return Err(Error::Sandbox {
            detail: format!(
                "could not read a semantic version from `{} --version`: {}; refusing to launch an agent against an unverified sandbox",
                binary.display(),
                diagnostic_tail(&stdout, &stderr)
            ),
        });
    };
    if reported < MIN_NONO_VERSION_PARTS {
        return Err(Error::Sandbox {
            detail: format!(
                "nono {}.{}.{} at `{}` is older than the required {MIN_NONO_VERSION}; upgrade nono and retry",
                reported.0,
                reported.1,
                reported.2,
                binary.display()
            ),
        });
    }
    Ok(format!("{}.{}.{}", reported.0, reported.1, reported.2))
}

/// Read the first semver-looking token in version output.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(parse_version_token)
}

/// Parse one `MAJOR.MINOR.PATCH` token, tolerating a leading `v`.
fn parse_version_token(token: &str) -> Option<(u64, u64, u64)> {
    let token = token.strip_prefix('v').unwrap_or(token);
    let mut parts = token.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Render one embedded profile with the caller's state-directory segment.
///
/// One pass substitutes [`STATE_DIR_TOKEN`]. The token appears only in the six state-path
/// denials, so the artifact paths a caller may grant individually stay untouched.
fn render_profile(template: &str, state_dir: &str) -> String {
    template.replace(STATE_DIR_TOKEN, state_dir)
}

/// Refuse a state directory that cannot be rendered into the profile safely.
///
/// The caller supplies this value, not the agent, but it is substituted into a JSON document: a
/// quote or a backslash would change the policy text, and a `..` component would name a directory
/// outside the checkout the denial is meant to cover. Only a plain relative path is accepted.
fn validate_state_dir(state_dir: &str) -> Result<()> {
    let invalid = |reason: &str| Error::Config {
        detail: format!(
            "the sandbox state directory `{state_dir}` is not usable: {reason}; pass a plain relative path below the run checkout (the default is `{DEFAULT_STATE_DIR}`)"
        ),
    };
    if state_dir.is_empty() {
        return Err(invalid("it is empty"));
    }
    if state_dir.contains(STATE_DIR_TOKEN) {
        return Err(invalid("it contains the profile's substitution token"));
    }
    for component in state_dir.split('/') {
        if component.is_empty() {
            return Err(invalid("it has an empty path component"));
        }
        if component == "." || component == ".." {
            return Err(invalid("it has a `.` or `..` path component"));
        }
        if !component
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(invalid(
                "it contains a character outside ASCII letters, digits, `-`, `_` and `.`",
            ));
        }
    }
    Ok(())
}

/// The content-addressed path of one embedded profile, without touching the filesystem.
fn profile_path(directory: &Path, kind: &str, contents: &str) -> PathBuf {
    let digest = sha256_hex(contents.as_bytes());
    directory.join(format!("{}-{kind}.json", &digest[..PROFILE_HASH_CHARS]))
}

/// Publish one embedded profile content-addressed, atomically, and without clobbering.
async fn publish_profile(
    directory: &Path,
    kind: &str,
    contents: &str,
    target: &Path,
) -> Result<()> {
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| {
            Error::io(
                &format!(
                    "creating the sandbox profile directory `{}`",
                    directory.display()
                ),
                error,
            )
        })?;

    if let Some(existing) = read_published_profile(target, kind).await? {
        if existing == contents.as_bytes() {
            return Ok(());
        }
        return Err(Error::Sandbox {
            detail: format!(
                "the published {kind} sandbox profile `{}` does not match the embedded profile; workcell never overwrites a published profile — remove it and retry",
                target.display()
            ),
        });
    }

    write_new_file(target, contents.as_bytes()).await
}

/// Read an already-published profile, refusing a target the read could not be bounded against.
///
/// `nono` is given this path at every launch, so whoever can write the profile directory could
/// plant something that is not the published bytes: a FIFO blocks the read forever, a character
/// device makes it unbounded. Both are refused before any content is consumed, and the deadline
/// covers a swap between the metadata check and the read.
async fn read_published_profile(target: &Path, kind: &str) -> Result<Option<Vec<u8>>> {
    let metadata = match tokio::fs::metadata(target).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::io(
                &format!(
                    "reading the published sandbox profile `{}`",
                    target.display()
                ),
                error,
            ));
        }
    };
    if !metadata.is_file() {
        return Err(Error::Sandbox {
            detail: format!(
                "the published {kind} sandbox profile `{}` is not a regular file; a profile must be the published bytes, not a link target, device, or pipe — remove it and retry",
                target.display()
            ),
        });
    }
    if metadata.len() > MAX_PROFILE_BYTES {
        return Err(Error::Sandbox {
            detail: format!(
                "the published {kind} sandbox profile `{}` is {} bytes, past the {MAX_PROFILE_BYTES}-byte bound workcell will read; remove it and retry",
                target.display(),
                metadata.len()
            ),
        });
    }

    let read = tokio::time::timeout(PROFILE_READ_TIMEOUT, tokio::fs::read(target)).await;
    match read {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(error)) => Err(Error::io(
            &format!(
                "reading the published sandbox profile `{}`",
                target.display()
            ),
            error,
        )),
        Err(_elapsed) => Err(Error::Sandbox {
            detail: format!(
                "the published {kind} sandbox profile `{}` did not become readable within {}s; a substituted or blocking target must not stall a launch — remove it and retry",
                target.display(),
                PROFILE_READ_TIMEOUT.as_secs()
            ),
        }),
    }
}

/// Write `bytes` at `path` once, atomically, through a same-directory temporary file.
///
/// The temporary file is created `0600` (the profiles may name host paths and must not be
/// world-readable), filled, and hard-linked into place, which fails rather than clobbers if
/// another process published the same content-addressed name first.
async fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or_else(|| Error::Path {
        detail: format!(
            "the sandbox profile path `{}` has no parent directory",
            path.display()
        ),
    })?;
    let temporary = temporary_path(directory, "profile");

    let written = async {
        write_temporary(&temporary, bytes).await?;
        match tokio::fs::hard_link(&temporary, path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = tokio::fs::read(path).await.map_err(|error| {
                    Error::io(&format!("reading `{}`", path.display()), error)
                })?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(Error::Sandbox {
                        detail: format!(
                            "another process published different bytes at the sandbox profile path `{}`; refusing to overwrite it",
                            path.display()
                        ),
                    })
                }
            }
            Err(error) => Err(Error::io(
                &format!("publishing the sandbox profile `{}`", path.display()),
                error,
            )),
        }
    }
    .await;
    let _ = tokio::fs::remove_file(&temporary).await;
    written
}

/// Fill a fresh temporary file with `bytes`, readable only by the owner.
async fn write_temporary(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| Error::io(&format!("creating `{}`", path.display()), error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| Error::io(&format!("securing `{}`", path.display()), error))?;
    }
    file.write_all(bytes)
        .await
        .map_err(|error| Error::io(&format!("writing `{}`", path.display()), error))?;
    file.sync_all()
        .await
        .map_err(|error| Error::io(&format!("syncing `{}`", path.display()), error))
}

/// A unique temporary path in `directory`, named for this process and a counter.
fn temporary_path(directory: &Path, label: &str) -> PathBuf {
    let counter = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        ".workcell-{label}-{}-{counter}.tmp",
        std::process::id()
    ))
}

/// Resolve the directory `nono` writes its state under.
fn nono_state_location() -> Result<StateLocation> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        let base = PathBuf::from(xdg);
        if base.is_absolute() {
            return Ok(StateLocation {
                root: base.join("nono"),
                base,
            });
        }
    }

    let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) else {
        return Err(Error::Sandbox {
            detail: "cannot locate nono's state directory: HOME and an absolute XDG_STATE_HOME are both unset, but nono records session state under `$XDG_STATE_HOME/nono` (default `$HOME/.local/state/nono`) — set HOME or XDG_STATE_HOME to a writable absolute path".to_owned(),
        });
    };
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(Error::Sandbox {
            detail: format!(
                "HOME is `{}`, which is not absolute; nono resolves its state root under `$HOME/.local/state/nono` — set an absolute HOME or XDG_STATE_HOME",
                home.display()
            ),
        });
    }
    let base = home.join(".local").join("state");
    Ok(StateLocation {
        root: base.join("nono"),
        base,
    })
}

/// Prove the nearest existing ancestor of the state root is a writable directory.
///
/// `probe` asks the filesystem directly by creating and removing a temporary file; it is used by
/// preflight, where a wrong answer is a launch that fails later. Doctor stays side-effect-free
/// and reads permission bits instead.
async fn ensure_state_root_usable(location: &StateLocation, probe: bool) -> Result<()> {
    let existing = nearest_existing_ancestor(&location.root)
        .await
        .ok_or_else(|| Error::Sandbox {
        detail: format!(
            "nono's state root `{}` has no existing ancestor directory; set XDG_STATE_HOME to a writable absolute path",
            location.root.display()
        ),
    })?;
    let metadata = tokio::fs::metadata(&existing).await.map_err(|error| {
        Error::io(
            &format!("inspecting the nono state path `{}`", existing.display()),
            error,
        )
    })?;
    if !metadata.is_dir() {
        return Err(Error::Sandbox {
            detail: format!(
                "nono's state path `{}` is not a directory: `{}` exists and is not one; nono records session state under `$XDG_STATE_HOME/nono` (default `$HOME/.local/state/nono`) — remove the file or point XDG_STATE_HOME at a writable directory",
                location.root.display(),
                existing.display()
            ),
        });
    }

    // The probe writes a throwaway file in the nearest existing ancestor, so it only runs when
    // that ancestor is the caller's state area (the state base or below). Anything further up is
    // inspected through permission bits instead, so preflight never writes outside that area.
    let writable = if probe && existing.starts_with(&location.base) {
        probe_directory_writable(&existing).await
    } else {
        !metadata.permissions().readonly()
    };
    if writable {
        return Ok(());
    }
    Err(Error::Sandbox {
        detail: format!(
            "the nono state directory is unusable: `{}` is not writable; nono writes its session and audit state under `$XDG_STATE_HOME/nono` (default `$HOME/.local/state/nono`) — fix that directory's permissions or point XDG_STATE_HOME at a writable absolute directory",
            existing.display()
        ),
    })
}

/// The nearest existing path at or above `path`.
async fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if current.as_os_str().is_empty() {
            return None;
        }
        if tokio::fs::symlink_metadata(current).await.is_ok() {
            return Some(current.to_path_buf());
        }
        candidate = current.parent();
    }
    None
}

/// Create and remove a temporary file in `directory`; true when the write succeeded.
async fn probe_directory_writable(directory: &Path) -> bool {
    let probe = temporary_path(directory, "probe");
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .await
    {
        Ok(_) => {
            let _ = tokio::fs::remove_file(&probe).await;
            true
        }
        Err(_) => false,
    }
}

/// The `git_common_dir/worktrees/<name>` grant for a linked worktree.
fn worktree_git_dir_grant(worktree: &Worktree) -> Result<PathBuf> {
    // The grant is a write capability and `Worktree`'s fields are public, so the value is
    // checked rather than trusted. A git directory of `worktrees/..` (or anything that is not
    // the named worktree entry) would widen the grant to the whole common Git directory and
    // defeat worktree mode's "hooks and config are not granted" property. Accept only the
    // layout Git itself creates: `<common>/worktrees/<name>`.
    //
    // The parent-directory check is separate from the name check below: `Path::file_name`
    // returns `None` for a path ending in `..`, so a name-only check would reject it for the
    // wrong reason and would not cover a `..` in the middle of the path.
    if worktree
        .git_dir
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(Error::Path {
            detail: format!(
                "the worktree git directory `{}` contains a parent-directory component; refusing to derive a write grant from it, because it could name the whole common Git directory",
                worktree.git_dir.display()
            ),
        });
    }
    let name = worktree.git_dir.file_name().ok_or_else(|| Error::Path {
        detail: format!(
            "the worktree git directory `{}` has no final path component, so its sandbox grant cannot be named",
            worktree.git_dir.display()
        ),
    })?;
    let name_text = name.to_string_lossy();
    if name_text.is_empty() || name_text == "." {
        return Err(Error::Path {
            detail: format!(
                "the worktree git directory `{}` is not a named entry under `<common>/worktrees`; refusing to derive a write grant from it",
                worktree.git_dir.display()
            ),
        });
    }
    let grant = worktree.git_common_dir.join("worktrees").join(name);
    if worktree.git_dir != grant {
        return Err(Error::Path {
            detail: format!(
                "the worktree git directory `{}` is not the expected `{}`; refusing to widen the sandbox grant",
                worktree.git_dir.display(),
                grant.display()
            ),
        });
    }
    Ok(grant)
}

/// The four surfaces the shared-checkout guard tracks, each as a hex SHA-256.
struct GuardSurfaces {
    /// The resolved repository-local `git config --local --list --includes` digest.
    config: String,
    /// A framed listing of the common Git directory's `hooks` entries.
    hooks: String,
    /// The repository identity: the absolute Git directories and the `.git` entry.
    repo: String,
    /// A framed listing of the common Git directory's `info` entries.
    info: String,
}

/// The parsed fields of one versioned guard snapshot.
#[derive(Clone, Copy)]
struct GuardSnapshot<'a> {
    /// The configuration surface digest.
    config: &'a str,
    /// The hooks surface digest.
    hooks: &'a str,
    /// The repository-identity surface digest.
    repo: &'a str,
    /// The info surface digest.
    info: &'a str,
}

/// An owned copy of a parsed snapshot, for use across an await point.
struct ParsedSnapshot {
    /// The configuration surface digest.
    config: String,
    /// The hooks surface digest.
    hooks: String,
    /// The repository-identity surface digest.
    repo: String,
    /// The info surface digest.
    info: String,
}

impl From<&GuardSnapshot<'_>> for ParsedSnapshot {
    fn from(parsed: &GuardSnapshot<'_>) -> Self {
        Self {
            config: parsed.config.to_owned(),
            hooks: parsed.hooks.to_owned(),
            repo: parsed.repo.to_owned(),
            info: parsed.info.to_owned(),
        }
    }
}

impl ParsedSnapshot {
    /// The surfaces whose digest differs from `current`, as operator-facing names.
    fn changed(&self, current: &GuardSurfaces) -> Vec<&'static str> {
        let mut changed = Vec::with_capacity(4);
        if self.config != current.config {
            changed.push("git configuration (`git config --local --list --includes`)");
        }
        if self.hooks != current.hooks {
            changed.push("repository hooks (`.git/hooks` entries)");
        }
        if self.repo != current.repo {
            changed.push("repository identity (Git directories or the `.git` entry)");
        }
        if self.info != current.info {
            changed.push("repository info files (`.git/info` entries)");
        }
        changed
    }
}

/// The surfaces a snapshot's fields differ on, as operator-facing names.
fn changed_surfaces(parsed: &GuardSnapshot<'_>, current: &GuardSurfaces) -> Vec<&'static str> {
    ParsedSnapshot::from(parsed).changed(current)
}

/// The error for a snapshot this guard version cannot parse (including a v3 snapshot).
fn invalid_guard_snapshot() -> Error {
    Error::Sandbox {
        detail: format!(
            "unrecognised guard snapshot; only the {GUARD_SNAPSHOT_PREFIX} format is supported, so a snapshot taken by an older guard must be re-taken before it can be verified"
        ),
    }
}

/// One entry of a guard surface directory, reduced to a deterministic, restorable record.
struct SurfaceEntry {
    /// The entry name inside the surface directory.
    name: String,
    /// What the entry is.
    kind: SurfaceEntryKind,
}

/// What one guard-surface directory entry is.
enum SurfaceEntryKind {
    /// A regular file, or (for hooks) a symlink that resolved to one.
    File {
        /// Unix permission bits, or zero where the platform has none.
        mode: u32,
        /// The full file length in bytes.
        length: u64,
        /// The first `content_limit` bytes.
        contents: Vec<u8>,
    },
    /// A symlink. Hooks follow the target; `info` entries do not.
    Symlink {
        /// The raw link target.
        target: PathBuf,
        /// The followed target's record, when the surface follows symlinks.
        followed: Option<FollowedFile>,
    },
    /// A directory (Git does not execute one).
    Directory,
    /// A device, socket, or FIFO; never opened.
    Special,
    /// An entry whose metadata or contents could not be read.
    Unreadable,
}

/// The followed target of a symlinked hook.
struct FollowedFile {
    /// Unix permission bits of the target.
    mode: u32,
    /// The target's full length in bytes.
    length: u64,
    /// The first `content_limit` bytes of the target.
    contents: Vec<u8>,
}

/// Read and hash the guard surfaces.
///
/// Every read failure is [`Error::Sandbox`]: a guard that cannot observe is not a guard, and the
/// caller must treat that as the non-retryable sandbox class rather than a transient Git fault.
async fn guard_surfaces(git: &Git) -> Result<GuardSurfaces> {
    // Repository-local configuration only: a concurrent change to `~/.gitconfig` or the system
    // config is not this repository's attack surface and must not fail a run as `sandbox`.
    let config = git
        .local_config_digest()
        .await
        .map_err(|error| Error::Sandbox {
            detail: format!(
                "the shared-checkout guard cannot read the repository configuration (`git config --local --list --includes`): {error}; refusing to continue because a changed configuration could execute outside the sandbox"
            ),
        })?;
    let directories = guard_git_directories(git).await?;
    let git_dir = directories.git_dir.display().to_string();
    let common_reported = directories.common_dir.display().to_string();
    // Git executes hooks from `core.hooksPath` when it is set; hashing only `<common>/hooks`
    // would leave the configured directory outside the guarded surface.
    let hooks_dir = match git.config_get("core.hooksPath").await? {
        None => directories.common_dir.join("hooks"),
        Some(configured) => {
            let configured = PathBuf::from(configured);
            if configured.is_absolute() {
                configured
            } else {
                git.root().join(configured)
            }
        }
    };
    let hooks = read_surface_entries(
        &hooks_dir,
        "hooks",
        MAX_HOOK_ENTRIES,
        HOOK_CONTENT_LIMIT,
        true,
    )
    .await?;
    let info = read_surface_entries(
        &directories.common_dir.join("info"),
        "info",
        MAX_INFO_ENTRIES,
        INFO_CONTENT_LIMIT,
        false,
    )
    .await?;
    let pointer = read_dot_git_entry(&git.root().join(".git")).await?;
    Ok(GuardSurfaces {
        config,
        hooks: sha256_hex(&frame_surface_entries(&hooks)),
        repo: framed_sha(&[
            git_dir.as_bytes(),
            common_reported.as_bytes(),
            pointer.as_slice(),
        ]),
        info: sha256_hex(&frame_surface_entries(&info)),
    })
}

/// Resolve the repository's Git directories, mapping a failure to the guard's sandbox class.
async fn guard_git_directories(git: &Git) -> Result<GitDirectories> {
    git.git_directories().await.map_err(|error| Error::Sandbox {
        detail: format!(
            "the shared-checkout guard cannot resolve the repository's Git directories: {error}; refusing to continue because the repository's hooks cannot be observed"
        ),
    })
}

/// The framed snapshot string, versioned so a future surface change is a new tag.
fn snapshot_text(surfaces: &GuardSurfaces) -> String {
    format!(
        "{GUARD_SNAPSHOT_PREFIX}:{}:{}:{}:{}",
        surfaces.config, surfaces.hooks, surfaces.repo, surfaces.info
    )
}

/// The `.git` entry's identity: a directory marker, a symlink's target, a pointer file's bytes,
/// or an explicit absence.
async fn read_dot_git_entry(path: &Path) -> Result<Vec<u8>> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(b"absent".to_vec()),
        Err(error) => {
            return Err(Error::Sandbox {
                detail: format!(
                    "the shared-checkout guard cannot inspect `{}`: {error}; refusing to continue because a `.git` repoint could not be detected",
                    path.display()
                ),
            });
        }
    };
    if metadata.is_dir() {
        return Ok(b"directory".to_vec());
    }
    if metadata.file_type().is_symlink() {
        let target = tokio::fs::read_link(path)
            .await
            .map_err(|error| Error::io(&path.display().to_string(), error))?;
        return Ok(target.to_string_lossy().as_bytes().to_vec());
    }
    if metadata.is_file() {
        if metadata.len() > MAX_DOT_GIT_ENTRY_BYTES {
            return Err(Error::Sandbox {
                detail: format!(
                    "the `.git` entry `{}` is {} bytes, past the {MAX_DOT_GIT_ENTRY_BYTES}-byte bound the shared-checkout guard will read; a `.git` pointer is one short line, so an oversized one is refused rather than hashed",
                    path.display(),
                    metadata.len()
                ),
            });
        }
        return tokio::fs::read(path)
            .await
            .map_err(|error| Error::io(&path.display().to_string(), error));
    }
    Ok(b"special".to_vec())
}

/// A framed SHA-256 over length-delimited parts, so no part boundary is ambiguous.
fn framed_sha(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Read one guard-surface directory, bounding entries and per-entry content.
async fn read_surface_entries(
    directory: &Path,
    label: &'static str,
    max_entries: usize,
    content_limit: u64,
    follow_symlinks: bool,
) -> Result<Vec<SurfaceEntry>> {
    let directory = directory.to_path_buf();
    run_blocking("reading a repository guard surface", move || {
        read_surface_entries_sync(
            &directory,
            label,
            max_entries,
            content_limit,
            follow_symlinks,
        )
    })
    .await
}

/// The blocking body of [`read_surface_entries`].
fn read_surface_entries_sync(
    directory: &Path,
    label: &str,
    max_entries: usize,
    content_limit: u64,
    follow_symlinks: bool,
) -> Result<Vec<SurfaceEntry>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::Sandbox {
                detail: format!(
                    "the shared-checkout guard cannot read the repository {label} directory `{}`: {error}; refusing to continue because a hook or repository configuration could execute outside the sandbox",
                    directory.display()
                ),
            });
        }
    };
    let mut surface = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| Error::Sandbox {
            detail: format!(
                "the shared-checkout guard cannot read an entry of the repository {label} directory `{}`: {error}",
                directory.display()
            ),
        })?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| Error::Sandbox {
            detail: format!(
                "the repository {label} directory `{}` contains a non-UTF-8 entry name; the shared-checkout guard cannot track it, so it refuses to continue",
                directory.display()
            ),
        })?;
        // The cap is enforced before the entry is read. Reading every entry and checking the count
        // afterwards lets a hostile repository size the guard's memory, and `.git/info` and
        // `.git/hooks` are inside the writable checkout in shared mode.
        if surface.len() >= max_entries {
            return Err(Error::Sandbox {
                detail: format!(
                    "the repository {label} directory `{}` holds more than the {max_entries}-entry cap; refusing to continue rather than let a hostile repository make the guard unreadable",
                    directory.display()
                ),
            });
        }
        let recorded = surface_entry(
            &entry.path(),
            name.to_owned(),
            content_limit,
            follow_symlinks,
        );
        // An entry the guard cannot observe must not be hashed by name alone: an unreadable hook
        // could then be rewritten while staying unreadable and the digest would not move.
        // The digest keeps the first `content_limit` bytes, so an entry longer than that could be
        // rewritten past the cap without moving the digest. A hook that large is abnormal; refuse
        // rather than hash a prefix and call the surface covered.
        let over_cap = match &recorded.kind {
            SurfaceEntryKind::File { length, .. } => *length > content_limit,
            SurfaceEntryKind::Symlink {
                followed: Some(file),
                ..
            } => file.length > content_limit,
            _ => false,
        };
        if over_cap {
            return Err(Error::Sandbox {
                detail: format!(
                    "the repository {label} entry `{}` is larger than the {content_limit}-byte cap the shared-checkout guard will hash; refusing to continue, because a rewrite past the cap would not change the digest",
                    recorded.name
                ),
            });
        }
        if matches!(&recorded.kind, SurfaceEntryKind::Unreadable) {
            return Err(Error::Sandbox {
                detail: format!(
                    "the repository {label} directory `{}` contains an entry `{}` the guard cannot read; refusing to continue because an unobservable hook or info file could be rewritten without changing the digest",
                    directory.display(),
                    recorded.name
                ),
            });
        }
        surface.push(recorded);
    }
    surface.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(surface)
}

/// Classify one surface directory entry and, for a readable file, read its bounded contents.
///
/// Reads never execute repository code. A symlink is either followed (hooks: the target's bytes
/// are what Git would execute) or recorded as a target only (`info`), and special entries are
/// markers rather than reads, so a FIFO or device cannot block the guard.
fn surface_entry(
    path: &Path,
    name: String,
    content_limit: u64,
    follow_symlinks: bool,
) -> SurfaceEntry {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return SurfaceEntry {
                name,
                kind: SurfaceEntryKind::Unreadable,
            };
        }
    };
    if metadata.file_type().is_symlink() {
        let target = match fs::read_link(path) {
            Ok(target) if target.as_os_str().len() <= MAX_SYMLINK_TARGET_BYTES => target,
            _ => {
                return SurfaceEntry {
                    name,
                    kind: SurfaceEntryKind::Unreadable,
                };
            }
        };
        let followed = if follow_symlinks {
            match fs::metadata(path) {
                Ok(target_metadata) if target_metadata.is_file() => {
                    read_bounded_file(path, &target_metadata, content_limit).map(
                        |(length, contents)| FollowedFile {
                            mode: mode_of(&target_metadata),
                            length,
                            contents,
                        },
                    )
                }
                _ => None,
            }
        } else {
            None
        };
        return SurfaceEntry {
            name,
            kind: SurfaceEntryKind::Symlink { target, followed },
        };
    }
    if metadata.is_dir() {
        return SurfaceEntry {
            name,
            kind: SurfaceEntryKind::Directory,
        };
    }
    if !metadata.is_file() {
        return SurfaceEntry {
            name,
            kind: SurfaceEntryKind::Special,
        };
    }
    match read_bounded_file(path, &metadata, content_limit) {
        Some((length, contents)) => SurfaceEntry {
            name,
            kind: SurfaceEntryKind::File {
                mode: mode_of(&metadata),
                length,
                contents,
            },
        },
        None => SurfaceEntry {
            name,
            kind: SurfaceEntryKind::Unreadable,
        },
    }
}

/// Read at most `content_limit` bytes of a file, returning its full length and the bytes read.
fn read_bounded_file(
    path: &Path,
    metadata: &fs::Metadata,
    content_limit: u64,
) -> Option<(u64, Vec<u8>)> {
    let file = fs::File::open(path).ok()?;
    let mut contents = Vec::new();
    file.take(content_limit).read_to_end(&mut contents).ok()?;
    Some((metadata.len(), contents))
}

/// The Unix permission bits of `metadata`, or zero on a platform without them.
#[cfg(unix)]
fn mode_of(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode()
}

/// The Unix permission bits of `metadata`, or zero on a platform without them.
#[cfg(not(unix))]
fn mode_of(_metadata: &fs::Metadata) -> u32 {
    0
}

/// Frame surface entries so a same-named rewrite changes the digest.
///
/// Each record carries the name, kind, mode, full length, and bounded contents (plus a symlink's
/// target and followed target), all length-delimited so no content bytes can be read as a record
/// boundary. Hashing contents as well as names is what catches an overwritten existing hook, and
/// hashing the mode catches a planted executable bit on a file that was not executable.
fn frame_surface_entries(entries: &[SurfaceEntry]) -> Vec<u8> {
    let mut framed = Vec::new();
    for entry in entries {
        put_length(&mut framed, entry.name.len());
        framed.extend_from_slice(entry.name.as_bytes());
        match &entry.kind {
            SurfaceEntryKind::File {
                mode,
                length,
                contents,
            } => {
                framed.push(b'F');
                put_u32(&mut framed, *mode);
                put_u64(&mut framed, *length);
                put_length(&mut framed, contents.len());
                framed.extend_from_slice(contents);
            }
            SurfaceEntryKind::Symlink { target, followed } => {
                framed.push(b'L');
                let target = target.to_string_lossy();
                put_length(&mut framed, target.len());
                framed.extend_from_slice(target.as_bytes());
                match followed {
                    Some(followed) => {
                        framed.push(b'f');
                        put_u32(&mut framed, followed.mode);
                        put_u64(&mut framed, followed.length);
                        put_length(&mut framed, followed.contents.len());
                        framed.extend_from_slice(&followed.contents);
                    }
                    None => framed.push(b'-'),
                }
            }
            SurfaceEntryKind::Directory => framed.push(b'D'),
            SurfaceEntryKind::Special => framed.push(b'X'),
            SurfaceEntryKind::Unreadable => framed.push(b'U'),
        }
    }
    framed
}

/// Run a blocking filesystem or Git operation off the async worker.
async fn run_blocking<T, F>(operation: &'static str, operation_fn: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation_fn).await {
        Ok(result) => result,
        Err(error) => Err(Error::Sandbox {
            detail: format!("{operation} could not run: {error}"),
        }),
    }
}

/// Append a framing `u32` (little-endian, so the framing is deterministic across hosts).
fn put_u32(framed: &mut Vec<u8>, value: u32) {
    framed.extend_from_slice(&value.to_le_bytes());
}

/// Append a framing `u64` (little-endian, so the framing is deterministic across hosts).
fn put_u64(framed: &mut Vec<u8>, value: u64) {
    framed.extend_from_slice(&value.to_le_bytes());
}

/// Append a framing length (little-endian, so the framing is deterministic across hosts).
fn put_length(framed: &mut Vec<u8>, value: usize) {
    framed.extend_from_slice(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
}

/// Parse a versioned guard snapshot: `v4:<config>:<hooks>:<repo>:<info>`.
///
/// Anything else is `None` — including an older format — so a snapshot the guard cannot fully
/// attribute is refused rather than partially trusted.
fn parse_guard_snapshot(snapshot: &str) -> Option<GuardSnapshot<'_>> {
    let mut parts = snapshot.split(':');
    if parts.next()? != GUARD_SNAPSHOT_PREFIX {
        return None;
    }
    let config = parts.next()?;
    let hooks = parts.next()?;
    let repo = parts.next()?;
    let info = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if [config, hooks, repo, info]
        .iter()
        .any(|part| part.is_empty())
    {
        return None;
    }
    Some(GuardSnapshot {
        config,
        hooks,
        repo,
        info,
    })
}

/// The hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One argv element for a path; the wrapper's argv is UTF-8.
fn path_text<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| Error::Path {
        detail: format!(
            "{what} path `{}` is not valid UTF-8; the sandbox wrapper passes argv as strings",
            path.display()
        ),
    })
}

/// Bound and join the child streams a failed sandbox command reported.
fn diagnostic_tail(stdout: &str, stderr: &str) -> String {
    let mut parts = Vec::with_capacity(2);
    for (label, text) in [("stdout", stdout), ("stderr", stderr)] {
        let text = text.trim();
        if !text.is_empty() {
            parts.push(format!("{label}: {text}"));
        }
    }
    let mut combined = parts.join("; ");
    let excess = combined
        .chars()
        .count()
        .saturating_sub(MAX_DIAGNOSTIC_CHARS);
    if excess > 0 {
        combined = combined.chars().skip(excess).collect();
        combined.insert(0, '…');
    }
    if combined.is_empty() {
        "no output".to_owned()
    } else {
        combined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_floor_string_agrees_with_the_comparison_tuple() {
        assert_eq!(
            parse_version(MIN_NONO_VERSION),
            Some(MIN_NONO_VERSION_PARTS)
        );
    }

    #[test]
    fn a_version_token_must_be_exactly_three_numeric_components() {
        assert_eq!(parse_version_token("0.80.0"), Some((0, 80, 0)));
        assert_eq!(parse_version_token("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version_token("0.80"), None);
        assert_eq!(parse_version_token("0.80.0.1"), None);
        assert_eq!(parse_version_token("0.80.0-rc1"), None);
        assert_eq!(parse_version_token("nono"), None);
    }

    #[test]
    fn a_snapshot_round_trips_through_its_parser() {
        let parsed = parse_guard_snapshot("v4:abc:def:ghi:jkl").expect("a v4 snapshot");
        assert_eq!(parsed.config, "abc");
        assert_eq!(parsed.hooks, "def");
        assert_eq!(parsed.repo, "ghi");
        assert_eq!(parsed.info, "jkl");

        // A v3 (or any other) snapshot is rejected as invalid, never guessed at.
        assert!(parse_guard_snapshot("v3:abc:def:ghi").is_none());
        assert!(parse_guard_snapshot("v1:abc:def:ghi").is_none());
        assert!(parse_guard_snapshot("abc:def:ghi:jkl").is_none());
        assert!(parse_guard_snapshot("v4::def:ghi:jkl").is_none());
        assert!(parse_guard_snapshot("v4:abc::ghi:jkl").is_none());
        assert!(parse_guard_snapshot("v4:abc:def:ghi:").is_none());
        assert!(parse_guard_snapshot("v4:abc:def:ghi:jkl:extra").is_none());
    }

    #[test]
    fn the_sha256_helper_matches_its_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn a_probe_that_never_finishes_is_refused() {
        let mut command = TokioCommand::new("/bin/sleep");
        command.arg("30").kill_on_drop(true);
        let error =
            output_with_deadline(command, Duration::from_millis(50), "the probe".to_owned())
                .await
                .expect_err("a hung probe must not block preflight");
        assert!(error.to_string().contains("did not finish"), "{error}");
    }

    #[tokio::test]
    async fn a_probe_that_finishes_is_returned() {
        let mut command = TokioCommand::new("/bin/echo");
        command.arg("ok");
        let output = output_with_deadline(command, Duration::from_secs(10), "echo".to_owned())
            .await
            .expect("echo finishes");
        assert!(output.status.success());
    }

    #[test]
    fn the_default_options_use_the_documented_state_directory() {
        assert_eq!(
            SandboxOptions::default().state_dir.as_str(),
            DEFAULT_STATE_DIR
        );
        assert_eq!(DEFAULT_STATE_DIR, ".workcell");
    }

    #[test]
    fn rendering_substitutes_the_token_and_leaves_the_artifact_denials_alone() {
        for template in [AGENT_PROFILE, REVIEW_PROFILE] {
            let rendered = render_profile(template, ".state");
            assert!(
                !rendered.contains(STATE_DIR_TOKEN),
                "no token may survive rendering"
            );
            for name in [
                "state.json",
                "logs",
                "runs/*.log",
                "control",
                "sandbox",
                "integration.lock",
            ] {
                assert!(
                    rendered.contains(&format!("\"$WORKDIR/.state/{name}\"")),
                    "the {name} denial must carry the rendered state directory"
                );
            }
            // The instruction and harness-configuration denials are not template values.
            assert!(rendered.contains("\"$WORKDIR/**/AGENTS.md\""));
            assert!(rendered.contains("\"$WORKDIR/**/.agents/**\""));
        }
    }

    #[test]
    fn a_state_directory_must_be_a_plain_relative_path() {
        for valid in [".workcell", ".state/workcell", "state-1", "a_b.c"] {
            assert_eq!(
                validate_state_dir(valid)
                    .as_ref()
                    .err()
                    .map(ToString::to_string),
                None,
                "`{valid}` must be accepted"
            );
        }
        for invalid in [
            "",
            "/absolute",
            "..",
            "../escape",
            "a/../b",
            "a//b",
            "./a",
            "a/",
            "with space",
            "quote\"",
            "back\\slash",
            STATE_DIR_TOKEN,
        ] {
            let error = validate_state_dir(invalid).expect_err("must be refused");
            assert!(
                matches!(error, Error::Config { .. }),
                "expected Error::Config for `{invalid}`, got {error:?}"
            );
        }
    }
}

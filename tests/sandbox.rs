//! What the sandbox contract promises, asserted without a real sandbox.
//!
//! Everything here is offline. A fake `nono` script stands in for the sandbox binary: it records
//! its argv and environment into absolute paths baked into the script, answers the preflight
//! subcommands (`--version`, `setup --check-only`, `profile validate`), and then executes the
//! command after the `--` separator. That is what makes the wrapper, the published profile bytes,
//! the environment scrub, and the refusal classification testable on a host where `nono` cannot
//! initialize a nested sandbox (macOS Seatbelt: `Operation not permitted`).
//!
//! # Environment mutation
//!
//! `Sandbox::resolve` is a `PATH` lookup by contract, so these tests must own `PATH`. A few also
//! own `HOME`/`XDG_STATE_HOME` so preflight's state-root check is exercised against a temporary
//! directory instead of the machine's real state root. Each mutation is `unsafe` under Rust 2024,
//! and the `SAFETY` note on each helper is honest only because `cargo nextest` runs every test in
//! its own process; run these under nextest, not under a threaded `cargo test`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use workcell::error::Error;
use workcell::git::Git;
use workcell::sandbox::{DoctorLevel, Sandbox, SandboxOptions, SandboxSpec};
use workcell::worktree::Worktree;

const AGENT_PROFILE: &str = include_str!("../src/sandbox/profiles/agent.profile.json");
const REVIEW_PROFILE: &str = include_str!("../src/sandbox/profiles/review.profile.json");
const VERIFY_PROFILE: &str = include_str!("../src/sandbox/profiles/verify.profile.json");

/// The token the embedded profiles use for the caller's in-checkout state directory.
const STATE_DIR_TOKEN: &str = "__STATE_DIR__";

/// The default state directory `Sandbox::resolve` renders the profiles with.
const DEFAULT_STATE_DIR: &str = ".workcell";

/// One embedded profile rendered the way [`Sandbox::resolve`] renders it by default.
fn rendered(profile: &str) -> String {
    profile.replace(STATE_DIR_TOKEN, DEFAULT_STATE_DIR)
}

/// The reviewed agent-instruction and credential denials both profiles must carry.
///
/// The six state-path entries keep the token: the raw policy text is caller-independent, and the
/// rendered bytes are asserted separately by the published-profile tests.
const REQUIRED_DENIES: [&str; 19] = [
    "$WORKDIR/**/AGENTS.md",
    "$WORKDIR/**/CLAUDE.md",
    "$WORKDIR/**/GEMINI.md",
    "$WORKDIR/**/.cursorrules",
    "$WORKDIR/**/.mcp.json",
    "$WORKDIR/**/.claude/**",
    "$WORKDIR/**/.codex/**",
    "$WORKDIR/**/.cursor/**",
    "$WORKDIR/**/.gemini/**",
    "$WORKDIR/**/.grok/**",
    "$WORKDIR/**/.agents/**",
    "$WORKDIR/__STATE_DIR__/state.json",
    "$WORKDIR/__STATE_DIR__/logs",
    "$WORKDIR/__STATE_DIR__/runs/*.log",
    "$WORKDIR/__STATE_DIR__/control",
    "$WORKDIR/__STATE_DIR__/sandbox",
    "$WORKDIR/__STATE_DIR__/integration.lock",
    "$HOME/.config/gh",
    "$HOME/.cargo/credentials.toml",
];

/// The toolchain policy groups both profiles must include.
const TOOLCHAIN_GROUPS: [&str; 6] = [
    "rust_runtime",
    "node_runtime",
    "user_tools",
    "git_config",
    "homebrew_macos",
    "homebrew_linux",
];

/// The package caches the agent profile grants **read-only**.
///
/// Measured (2026-10-28): a full offline rebuild from a warm cache wrote zero files under
/// `~/.cargo`, so a build needs to read the caches, not write them. Making them writable was the
/// mechanism by which a clean-diff run could poison a later unsandboxed build.
const AGENT_READ_ONLY_CACHES: &[&str] = &["$HOME/.cargo/registry", "$HOME/.cargo/git"];

/// The writable harness-state and cache grants the agent profile carries.
///
/// Harness *configuration* (`~/.claude/settings.json`, `~/.codex/config.toml`, …) is deliberately
/// absent: it is readable so the harness can start, and not writable, so an agent cannot plant a
/// hook that executes in the human's next session.
const AGENT_ALLOW: &[&str] = &[
    "$WORKDIR",
    "$HOME/.cache",
    "$HOME/.claude/projects",
    "$HOME/.claude/todos",
    "$HOME/.claude/shell-snapshots",
    "$HOME/.claude/statsig",
    "$HOME/.claude/debug",
    "$HOME/.grok/sessions",
    "$HOME/.grok/log",
    "$HOME/.grok/crash",
    "$HOME/.gemini/antigravity-cli/cache",
    "$HOME/.gemini/antigravity-cli/conversations",
    "$HOME/.gemini/antigravity-cli/crashes",
    "$HOME/.gemini/antigravity-cli/log",
    "$HOME/.gemini/antigravity-cli/scratch",
    "$HOME/.gemini/antigravity-cli/presence",
    "$HOME/.gemini/antigravity-cli/knowledge",
    "$HOME/.gemini/antigravity-cli/implicit",
    "$HOME/.gemini/antigravity-cli/annotations",
    "$HOME/.gemini/antigravity-cli/brain",
    "$HOME/.gemini/history",
];

/// The two file grants the agent profile carries: a Cargo lock file and the harness auth token it
/// must refresh.
const AGENT_ALLOW_FILE: &[&str] = &[
    "$HOME/.cargo/.package-cache",
    "$HOME/.gemini/state.json",
    "$HOME/.gemini/antigravity-cli/antigravity-oauth-token",
    "$HOME/.gemini/antigravity-cli/history.jsonl",
    "$HOME/.gemini/antigravity-cli/conversation_summaries.db",
    "$HOME/.gemini/antigravity-cli/jetski_state.pbtxt",
    "$HOME/.gemini/antigravity-cli/last_check.timestamp",
];

/// The keychain files a caller may grant per run; the profiles must not carry them.
const KEYCHAIN_FILES: [&str; 2] = [
    "$HOME/Library/Keychains/login.keychain-db",
    "$HOME/Library/Keychains/metadata.keychain-db",
];

// ---------------------------------------------------------------------------------------------
// The fake sandbox binary
// ---------------------------------------------------------------------------------------------

/// A fake `nono` on a private `PATH`.
struct FakeNono {
    _root: TempDir,
    bin_dir: PathBuf,
    record: PathBuf,
    env_record: PathBuf,
}

impl FakeNono {
    /// A fake that reports `version` and succeeds every preflight subcommand.
    fn new(version: &str) -> Self {
        Self::with_exits(version, 0, 0)
    }

    /// A fake that reports `version` and exits with the given preflight statuses.
    fn with_exits(version: &str, setup_exit: i32, validate_exit: i32) -> Self {
        let root = TempDir::new().expect("a temporary directory");
        let bin_dir = root.path().join("bin");
        std::fs::create_dir_all(&bin_dir).expect("the fake bin directory");
        let record = root.path().join("argv.log");
        let env_record = root.path().join("env.log");
        let record_quoted = shell_quote(&record);
        let env_quoted = shell_quote(&env_record);

        let script = format!(
            "#!/bin/sh\n\
             printf '@@\\n' >> {record_quoted}\n\
             for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> {record_quoted}; done\n\
             env > {env_quoted}\n\
             case \"$1\" in\n\
               --version)\n\
                 printf 'nono {version}\\n'\n\
                 exit 0\n\
                 ;;\n\
               setup)\n\
                 printf 'fake setup: checking host support\\n'\n\
                 printf 'fake setup: the sandbox probe failed\\n' >&2\n\
                 exit {setup_exit}\n\
                 ;;\n\
               profile)\n\
                 if [ \"$2\" = validate ]; then\n\
                   printf 'fake profile validate %s\\n' \"$3\"\n\
                   printf 'fake profile validate: rejected\\n' >&2\n\
                   exit {validate_exit}\n\
                 fi\n\
                 ;;\n\
             esac\n\
             while [ \"$#\" -gt 0 ] && [ \"$1\" != \"--\" ]; do shift; done\n\
             if [ \"$#\" -gt 0 ]; then shift; fi\n\
             exec \"$@\"\n"
        );
        let binary = bin_dir.join("nono");
        std::fs::write(&binary, script).expect("the fake nono script");
        make_executable(&binary);

        Self {
            _root: root,
            bin_dir,
            record,
            env_record,
        }
    }

    /// Prepend this fake's directory to `PATH` for the rest of the test process.
    fn activate(&self) {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![self.bin_dir.clone()];
        entries.extend(std::env::split_paths(&current));
        let joined = std::env::join_paths(entries).expect("a valid PATH");
        // SAFETY: `cargo nextest` runs one test per process, so no other thread in this process
        // reads or writes the environment while this test runs.
        unsafe {
            std::env::set_var("PATH", joined);
        }
    }

    /// Every invocation the fake has seen, in order, as argv lists.
    fn recorded_invocations(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(&self.record).unwrap_or_default();
        let mut invocations = Vec::new();
        let mut current = Vec::new();
        for line in text.lines() {
            if line == "@@" {
                if !current.is_empty() {
                    invocations.push(std::mem::take(&mut current));
                }
            } else {
                current.push(line.to_owned());
            }
        }
        if !current.is_empty() {
            invocations.push(current);
        }
        invocations
    }

    /// The most recent invocation's argv.
    fn recorded_argv(&self) -> Vec<String> {
        self.recorded_invocations().pop().unwrap_or_default()
    }

    /// The environment the most recent invocation received.
    fn recorded_env(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.env_record)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                line.split_once('=')
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
            })
            .collect()
    }
}

/// The full hex SHA-256 of `bytes`, for content-address assertions.
fn digest_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A single-quoted shell word for `text`.
fn shell_quote(path: &Path) -> String {
    let text = path.display().to_string();
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Set the executable bits on a script or binary.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("executable permissions");
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Replace `PATH` with exactly `directory`.
fn set_path_to(directory: &Path) {
    let joined = std::env::join_paths([directory]).expect("a valid PATH");
    // SAFETY: `cargo nextest` runs one test per process; see the module docs.
    unsafe {
        std::env::set_var("PATH", joined);
    }
}

/// Point `nono`'s state root at a temporary base directory.
fn set_state_root(directory: &Path) {
    // SAFETY: `cargo nextest` runs one test per process; see the module docs.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", directory);
    }
}

/// Unset both variables `nono`'s state root resolution reads.
fn unset_state_location_variables() {
    // SAFETY: `cargo nextest` runs one test per process; see the module docs.
    unsafe {
        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("HOME");
    }
}

/// Resolve a sandbox against a fake binary and a temporary profile directory.
fn resolve(subject: &FakeNono, root: &TempDir) -> Sandbox {
    subject.activate();
    Sandbox::resolve(root.path().join("state").join("sandbox")).expect("resolve the fake sandbox")
}

// ---------------------------------------------------------------------------------------------
// The embedded profile bytes
// ---------------------------------------------------------------------------------------------

/// The typed subset of the `nono` profile schema this crate's policy is written against.
///
/// `serde_json::Value` is disallowed for contract types in this repository: a profile gate that
/// walks untyped JSON could silently stop asserting a field the schema renamed. A typed subset
/// makes a renamed or removed field a parse failure in the test.
#[derive(serde::Deserialize)]
struct EmbeddedProfile {
    extends: String,
    groups: ProfileGroups,
    workdir: ProfileWorkdir,
    network: ProfileNetwork,
    filesystem: ProfileFilesystem,
}

/// The `groups` section of a profile.
#[derive(serde::Deserialize)]
struct ProfileGroups {
    include: Vec<String>,
}

/// The `workdir` section of a profile.
#[derive(serde::Deserialize)]
struct ProfileWorkdir {
    access: String,
}

/// The `network` section of a profile.
#[derive(serde::Deserialize)]
struct ProfileNetwork {
    block: bool,
    #[serde(default)]
    allow_domain: Vec<String>,
    #[serde(default)]
    open_port: Vec<u16>,
}

/// The `filesystem` section of a profile.
#[derive(serde::Deserialize, Default)]
struct ProfileFilesystem {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    allow_file: Vec<String>,
    #[serde(default)]
    read: Vec<String>,
    #[serde(default)]
    bypass_protection: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// Parse one embedded profile against the typed policy subset.
fn embedded_profile(text: &str) -> EmbeddedProfile {
    serde_json::from_str(text).expect("the embedded profile matches the typed policy subset")
}

/// Assert the profile restricts egress to the provider endpoints the supported harnesses use.
fn assert_provider_egress(network: &ProfileNetwork) {
    assert!(
        !network.block,
        "the provider allowlist replaces a blanket block; `block: true` breaks every harness"
    );
    for domain in [
        "api.anthropic.com",
        "api.openai.com",
        "chatgpt.com",
        "cloudcode-pa.googleapis.com",
        "generativelanguage.googleapis.com",
    ] {
        assert_contains(&network.allow_domain, domain);
    }
    assert!(
        network.open_port.is_empty(),
        "the profile grants no loopback: a caller that needs an arbitrary-port bind asks for it per          run, because measured harnesses differ (claude fails with EPERM without the wildcard,          agy binds a local server, codex uses its explicit TLS-relay port)"
    );
}

/// Assert one required string appears in a profile grant or denial list.
fn assert_contains(paths: &[String], required: &str) {
    assert!(
        paths.iter().any(|path| path == required),
        "the embedded profile must list `{required}`: {paths:?}"
    );
}

#[test]
fn the_agent_profile_grants_the_checkout_and_denies_the_escape_paths() {
    let profile = embedded_profile(AGENT_PROFILE);
    assert_eq!(profile.extends, "default");
    assert_eq!(profile.workdir.access, "readwrite");
    assert_provider_egress(&profile.network);

    for required in TOOLCHAIN_GROUPS {
        assert_contains(&profile.groups.include, required);
    }
    for required in AGENT_ALLOW {
        assert_contains(&profile.filesystem.allow, required);
    }
    for required in AGENT_ALLOW_FILE {
        assert_contains(&profile.filesystem.allow_file, required);
    }
    for required in AGENT_READ_ONLY_CACHES {
        assert_contains(&profile.filesystem.read, required);
        assert!(
            !profile.filesystem.allow.iter().any(|path| path == required),
            "{required} must not be writable: a run that can write a cached dependency poisons the \
             caller's next unsandboxed build"
        );
    }
    for required in ["$HOME/.cargo", "$HOME/.rustup", "$HOME/.local/bin"] {
        assert_contains(&profile.filesystem.read, required);
    }
    // Harness configuration and credentials are caller-supplied per run, never profile grants:
    // that is what stops a codex child from reading Claude's configuration or keychain.
    for forbidden in [
        "$HOME/.claude",
        "$HOME/.claude.json",
        "$HOME/.codex",
        "$HOME/.gemini",
        "$HOME/.antigravity",
        "$HOME/.grok",
        "$HOME/.dsh",
    ] {
        assert!(
            !profile.filesystem.read.iter().any(|path| path == forbidden),
            "harness path `{forbidden}` must be granted by the caller, not the profile"
        );
    }
    // Codex state is granted per run by the caller (CODEX_HOME), never by the static profile: the
    // operator's ~/.codex is not writable at all.
    assert!(
        !profile
            .filesystem
            .allow
            .iter()
            .any(|path| path.starts_with("$HOME/.codex")),
        "the profile must not grant the shared codex home: {:?}",
        profile.filesystem.allow
    );
    assert!(
        !profile
            .filesystem
            .allow_file
            .iter()
            .any(|path| path.starts_with("$HOME/.codex")),
        "the profile must not grant a shared codex state file: {:?}",
        profile.filesystem.allow_file
    );
    // Harness configuration is never writable: a planted hook in `~/.claude/settings.json` or
    // `~/.codex/config.toml` executes in the human's next session, outside this sandbox. Session
    // state is the only writable part.
    for config in [
        "$HOME/.claude",
        "$HOME/.claude.json",
        "$HOME/.gemini",
        "$HOME/.grok",
    ] {
        assert!(
            !profile.filesystem.allow.iter().any(|path| path == config),
            "harness configuration `{config}` must not be writable: {:?}",
            profile.filesystem.allow
        );
    }
    assert!(
        profile.filesystem.bypass_protection.is_empty(),
        "a protected file is named by the caller per run, not by the profile: {:?}",
        profile.filesystem.bypass_protection
    );
    for forbidden in KEYCHAIN_FILES {
        assert!(
            !profile.filesystem.read.iter().any(|path| path == forbidden),
            "the login keychain must be caller-supplied, not a profile grant: {forbidden}"
        );
    }
    for required in REQUIRED_DENIES {
        assert_contains(&profile.filesystem.deny, required);
    }
}

#[test]
fn the_review_profile_is_read_only_and_carries_the_same_denials() {
    let agent = embedded_profile(AGENT_PROFILE);
    let review = embedded_profile(REVIEW_PROFILE);
    assert_eq!(review.extends, "default");
    assert_eq!(review.workdir.access, "read");
    assert_provider_egress(&review.network);

    for required in TOOLCHAIN_GROUPS {
        assert_contains(&review.groups.include, required);
    }
    assert!(
        !review
            .filesystem
            .allow
            .iter()
            .any(|path| path == "$WORKDIR"),
        "the review profile must not make the checkout writable: {:?}",
        review.filesystem.allow
    );
    for required in ["$HOME/.cargo", "$HOME/.rustup", "$HOME/.local/bin"] {
        assert_contains(&review.filesystem.read, required);
    }
    assert!(
        review.filesystem.bypass_protection.is_empty(),
        "the review profile must not grant a protection bypass: {:?}",
        review.filesystem.bypass_protection
    );
    for forbidden in KEYCHAIN_FILES
        .iter()
        .copied()
        .chain(["$HOME/.claude", "$HOME/.codex"])
    {
        assert!(
            !review.filesystem.read.iter().any(|path| path == forbidden),
            "harness and keychain path `{forbidden}` must be caller-supplied"
        );
    }
    for required in REQUIRED_DENIES {
        assert_contains(&review.filesystem.deny, required);
    }
    assert_eq!(
        agent.filesystem.deny, review.filesystem.deny,
        "both profiles must carry the same agent-instruction and credential denials"
    );
}

// ---------------------------------------------------------------------------------------------
// Resolve and publish
// ---------------------------------------------------------------------------------------------

#[test]
fn resolve_refuses_a_host_without_the_sandbox_binary_on_path() {
    let root = TempDir::new().expect("a temporary directory");
    let empty = root.path().join("empty-bin");
    std::fs::create_dir_all(&empty).expect("an empty bin directory");
    set_path_to(&empty);

    let error = Sandbox::resolve(root.path().join("profiles")).expect_err("no nono on PATH");
    let text = error.to_string();
    assert!(text.contains("PATH"), "{text}");
    assert!(text.contains("nono"), "{text}");
    assert!(text.contains("prerequisite"), "{text}");
}

#[tokio::test]
async fn preflight_refuses_an_unparsable_binary_version() {
    let fake = FakeNono::new("not-a-version");
    fake.activate();
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);

    let error = subject.preflight().await.expect_err("no semantic version");
    let text = error.to_string();
    assert!(text.contains("semantic version"), "{text}");
    assert!(text.contains("--version"), "{text}");
}

#[tokio::test]
async fn preflight_refuses_a_binary_older_than_the_floor() {
    let fake = FakeNono::new("0.79.9");
    fake.activate();
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);

    let error = subject.preflight().await.expect_err("too old");
    let text = error.to_string();
    assert!(text.contains("0.79.9"), "{text}");
    assert!(text.contains("0.80.0"), "{text}");
}

#[tokio::test]
async fn preflight_accepts_a_binary_newer_than_the_floor() {
    let fake = FakeNono::new("0.81.2");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");
    assert_eq!(subject.version(), Some("0.81.2"));
}

#[test]
fn the_profile_address_is_versioned_against_the_scheme_and_nono_floor() {
    // The address must not be a raw content hash: a scheme change (a different digest meaning) or
    // a higher nono floor has to produce a different published name.
    assert_ne!(
        workcell::sandbox::profile_digest("{}"),
        digest_hex(b"{}"),
        "the address must cover the scheme and the nono floor, not only the bytes"
    );
    assert_eq!(
        workcell::sandbox::profile_digest("{}"),
        workcell::sandbox::profile_digest("{}"),
        "the address is deterministic"
    );
}

#[tokio::test]
async fn preflight_publishes_every_profile_content_addressed_and_unreadable_by_others() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let profile_dir = root.path().join("state").join("sandbox");
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");
    assert_eq!(subject.profile_dir(), profile_dir);
    assert_eq!(subject.version(), Some("0.80.0"));
    assert!(subject.binary().ends_with("nono"), "{:?}", subject.binary());

    // A second resolve computes the same content-addressed paths; preflight reuses those files.
    let again = Sandbox::resolve(&profile_dir).expect("a second resolve");
    assert_eq!(subject.agent_profile(), again.agent_profile());
    assert_eq!(subject.review_profile(), again.review_profile());
    assert_eq!(subject.verify_profile(), again.verify_profile());
    again.preflight().await.expect("a second preflight");

    let agent = std::fs::read_to_string(subject.agent_profile()).expect("the agent profile");
    assert_eq!(agent, rendered(AGENT_PROFILE));
    let review = std::fs::read_to_string(subject.review_profile()).expect("the review profile");
    assert_eq!(review, rendered(REVIEW_PROFILE));
    let verify = std::fs::read_to_string(subject.verify_profile()).expect("the verify profile");
    assert_eq!(verify, rendered(VERIFY_PROFILE));

    for (path, contents, suffix) in [
        (
            subject.agent_profile(),
            rendered(AGENT_PROFILE),
            "-agent.json",
        ),
        (
            subject.review_profile(),
            rendered(REVIEW_PROFILE),
            "-review.json",
        ),
        (
            subject.verify_profile(),
            rendered(VERIFY_PROFILE),
            "-verify.json",
        ),
    ] {
        let name = path
            .file_name()
            .expect("a profile file name")
            .to_string_lossy();
        assert!(name.ends_with(suffix), "unexpected profile name {name}");
        let digest = workcell::sandbox::profile_digest(&contents);
        assert!(
            name.starts_with(&digest[..16]),
            "the profile name must be the versioned content address of its rendered bytes: {name} vs {digest}"
        );
        assert_eq!(
            name.len(),
            suffix.len() + 16,
            "the profile name must be a 16-hex-character content address: {name}"
        );
        assert!(
            !contents.contains(STATE_DIR_TOKEN),
            "the published {suffix} profile must not carry the substitution token"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path)
                .expect("profile metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "profile {name} must be mode 0600");
        }
    }

    let published = std::fs::read_dir(&profile_dir)
        .expect("the profile directory")
        .count();
    assert_eq!(
        published, 3,
        "exactly the three reviewed profiles are published"
    );
}

#[tokio::test]
async fn the_default_published_profiles_deny_the_default_state_directory() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");

    for path in [
        subject.agent_profile(),
        subject.review_profile(),
        subject.verify_profile(),
    ] {
        let published = std::fs::read_to_string(path).expect("the published profile");
        assert!(
            published.contains(&format!("\"$WORKDIR/{DEFAULT_STATE_DIR}/state.json\"")),
            "the default resolve must render `{DEFAULT_STATE_DIR}` into the deny list: {published}"
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
                published.contains(&format!("\"$WORKDIR/{DEFAULT_STATE_DIR}/{name}\"")),
                "the published profile must deny `$WORKDIR/{DEFAULT_STATE_DIR}/{name}`"
            );
        }
        assert!(
            !published.contains(STATE_DIR_TOKEN),
            "a published profile must not carry the substitution token"
        );
        let state_denies: Vec<&str> = published
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("\"$WORKDIR/."))
            .collect();
        assert!(
            state_denies
                .iter()
                .all(|line| line.contains(&format!("/{DEFAULT_STATE_DIR}/"))),
            "every in-checkout state deny must name the default state directory: {state_denies:?}"
        );
    }
}

#[tokio::test]
async fn a_custom_state_directory_is_rendered_denied_and_content_addressed() {
    let fake = FakeNono::new("0.80.0");
    fake.activate();
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);

    let custom = Sandbox::resolve_with(
        root.path().join("custom-profiles"),
        SandboxOptions {
            state_dir: ".custom".to_owned(),
        },
    )
    .expect("resolve with a custom state directory");
    let default = Sandbox::resolve(root.path().join("default-profiles")).expect("default resolve");
    assert_ne!(
        custom.agent_profile().file_name(),
        default.agent_profile().file_name(),
        "a different rendered policy must have a different content address"
    );
    assert_ne!(
        custom.review_profile().file_name(),
        default.review_profile().file_name()
    );

    fake.activate();
    custom
        .preflight()
        .await
        .expect("preflight the custom policy");

    let published = std::fs::read_to_string(custom.agent_profile()).expect("the custom profile");
    for name in [
        "state.json",
        "logs",
        "runs/*.log",
        "control",
        "sandbox",
        "integration.lock",
    ] {
        assert!(
            published.contains(&format!("\"$WORKDIR/.custom/{name}\"")),
            "the caller's state directory must be the one denied for `{name}`"
        );
        assert!(
            !published.contains(&format!("\"$WORKDIR/{DEFAULT_STATE_DIR}/{name}\"")),
            "the default must not leak into a custom rendering for `{name}`"
        );
    }
    // The artifact paths the caller grants per run are untouched by the rendering.
    assert!(published.contains("\"$WORKDIR/**/AGENTS.md\""));
    let address = custom
        .agent_profile()
        .file_name()
        .expect("a profile file name")
        .to_string_lossy()
        .to_string();
    let digest = workcell::sandbox::profile_digest(&published);
    assert_eq!(
        &address[..16],
        &digest[..16],
        "the name must be the versioned address of the bytes nono will enforce"
    );
    // The fake nono was asked to validate exactly the rendered file.
    let invocations = fake.recorded_invocations();
    let validated = invocations
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("profile"))
        .expect("a profile validate invocation");
    let expected = custom.agent_profile().display().to_string();
    assert_eq!(
        validated.get(2).map(String::as_str),
        Some(expected.as_str()),
        "preflight validates the rendered, published profile: {validated:?}"
    );
}

#[test]
fn resolve_with_refuses_a_state_directory_that_cannot_be_rendered() {
    let fake = FakeNono::new("0.80.0");
    fake.activate();
    let root = TempDir::new().expect("a temporary directory");

    for state_dir in ["", "/absolute", "../escape", "with space", STATE_DIR_TOKEN] {
        let error = Sandbox::resolve_with(
            root.path().join("profiles"),
            SandboxOptions {
                state_dir: state_dir.to_owned(),
            },
        )
        .expect_err("an unusable state directory must be refused");
        assert!(
            matches!(error, Error::Config { .. }),
            "expected Error::Config for `{state_dir}`, got {error:?}"
        );
        assert!(
            error.to_string().contains("state directory"),
            "{state_dir}: {error}"
        );
    }
}

#[tokio::test]
async fn preflight_refuses_a_published_profile_that_was_edited() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("first preflight");
    std::fs::write(subject.agent_profile(), "{\"edited\":true}\n").expect("tamper the profile");

    let error = subject.preflight().await.expect_err("tampered profile");
    let text = error.to_string();
    assert!(text.contains("does not match"), "{text}");
    assert!(text.contains("agent"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn preflight_probes_the_host_and_validates_the_agent_profile() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);

    subject.preflight().await.expect("preflight");

    let invocations = fake.recorded_invocations();
    assert_eq!(
        invocations.len(),
        5,
        "resolve probes the version, then preflight runs the host probe and validates every published profile: {invocations:?}"
    );
    assert_eq!(invocations[0], vec!["--version".to_owned()]);
    assert_eq!(
        invocations[1],
        vec!["setup".to_owned(), "--check-only".to_owned()]
    );
    assert_eq!(
        invocations[2],
        vec![
            "profile".to_owned(),
            "validate".to_owned(),
            subject.agent_profile().display().to_string(),
        ]
    );
    assert_eq!(
        invocations[3],
        vec![
            "profile".to_owned(),
            "validate".to_owned(),
            subject.review_profile().display().to_string(),
        ]
    );
    assert_eq!(
        invocations[4],
        vec![
            "profile".to_owned(),
            "validate".to_owned(),
            subject.verify_profile().display().to_string(),
        ]
    );
}

#[tokio::test]
async fn preflight_refuses_a_published_profile_that_no_longer_matches_the_policy() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);
    subject
        .preflight()
        .await
        .expect("the first preflight publishes");
    let before = fake.recorded_invocations().len();
    std::fs::write(subject.agent_profile(), "{\"edited\":true}\n").expect("tamper the profile");

    let error = subject.preflight().await.expect_err("tampered profile");
    let text = error.to_string();
    assert!(
        text.contains("does not match") || text.contains("no longer matches"),
        "{text}"
    );
    assert_eq!(
        fake.recorded_invocations().len(),
        before,
        "the second preflight must refuse before invoking nono: {:?}",
        fake.recorded_invocations()
    );
}

#[tokio::test]
async fn preflight_fails_closed_when_the_host_cannot_enforce_a_sandbox() {
    let fake = FakeNono::with_exits("0.80.0", 1, 0);
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);

    let error = subject.preflight().await.expect_err("unsupported host");
    let text = error.to_string();
    assert!(text.contains("cannot enforce"), "{text}");
    assert!(text.contains("nested sandbox"), "{text}");

    let last = fake.recorded_argv();
    assert_eq!(
        last.first().map(String::as_str),
        Some("setup"),
        "preflight must stop at the host probe: {last:?}"
    );
}

#[tokio::test]
async fn preflight_fails_closed_when_the_agent_profile_is_rejected() {
    let fake = FakeNono::with_exits("0.80.0", 0, 1);
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);

    let error = subject.preflight().await.expect_err("invalid profile");
    let text = error.to_string();
    assert!(text.contains("rejected"), "{text}");
    assert!(text.contains("0.80.0"), "{text}");

    let last = fake.recorded_argv();
    assert_eq!(
        last.first().map(String::as_str),
        Some("profile"),
        "preflight must validate the published profile: {last:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn preflight_refuses_an_unusable_nono_state_root() {
    use std::os::unix::fs::PermissionsExt as _;

    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("read-only-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555))
        .expect("make the state base read-only");
    set_state_root(&state);

    let error = subject.preflight().await.expect_err("unusable state root");
    let text = error.to_string();
    assert!(text.contains("not writable"), "{text}");
    assert!(text.contains("XDG_STATE_HOME"), "{text}");

    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755))
        .expect("restore the state base permissions");
}

#[tokio::test]
async fn preflight_requires_a_state_location_it_can_name() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    unset_state_location_variables();

    let error = subject.preflight().await.expect_err("no state location");
    let text = error.to_string();
    assert!(text.contains("HOME"), "{text}");
    assert!(text.contains("XDG_STATE_HOME"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// The wrapper
// ---------------------------------------------------------------------------------------------

/// A worktree layout under `root`, for the wrapper tests.
fn worktree_under(root: &TempDir) -> Worktree {
    let git_common_dir = root.path().join("repo").join(".git");
    Worktree {
        path: root.path().join("checkout"),
        branch: "agent/demo/head".to_owned(),
        base_sha: "0".repeat(40),
        git_dir: git_common_dir.join("worktrees").join("demo"),
        git_common_dir,
    }
}

#[test]
fn wrap_agent_is_deterministic_and_grants_only_the_git_metadata() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let worktree = worktree_under(&root);
    let brief_dir = root.path().join("brief");
    let mcp = root.path().join("mcp.json");
    let spec = SandboxSpec {
        checkout: &worktree.path,
        worktree: Some(&worktree),
        read_paths: vec![brief_dir.as_path()],
        read_files: vec![mcp.as_path()],
        loopback_ports: vec![8787, 9876],
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };

    let argv = vec!["codex".to_owned(), "exec".to_owned(), "-".to_owned()];
    let first = subject.wrap_agent(argv.clone(), &spec).expect("wrap");
    let second = subject.wrap_agent(argv.clone(), &spec).expect("wrap again");
    assert_eq!(first, second, "the wrapper must be deterministic");

    let common = &worktree.git_common_dir;
    let expected = vec![
        subject.binary().display().to_string(),
        "-s".to_owned(),
        "run".to_owned(),
        "--no-diagnostics".to_owned(),
        "--profile".to_owned(),
        subject.agent_profile().display().to_string(),
        "--workdir".to_owned(),
        worktree.path.display().to_string(),
        "--read".to_owned(),
        common.display().to_string(),
        "--allow".to_owned(),
        common.join("objects").display().to_string(),
        "--allow".to_owned(),
        common
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo")
            .display()
            .to_string(),
        "--allow".to_owned(),
        common
            .join("logs")
            .join("refs")
            .join("heads")
            .join("agent")
            .join("demo")
            .display()
            .to_string(),
        "--allow".to_owned(),
        common.join("worktrees").join("demo").display().to_string(),
        "--read".to_owned(),
        brief_dir.display().to_string(),
        "--read-file".to_owned(),
        mcp.display().to_string(),
        "--open-port".to_owned(),
        "8787".to_owned(),
        "--open-port".to_owned(),
        "9876".to_owned(),
        "--".to_owned(),
        "codex".to_owned(),
        "exec".to_owned(),
        "-".to_owned(),
    ];
    assert_eq!(first, expected);

    let grants: Vec<&String> = first
        .iter()
        .zip(first.iter().skip(1))
        .filter(|(flag, _)| flag.as_str() == "--allow")
        .map(|(_, path)| path)
        .collect();
    let refs_root = common.join("refs").display().to_string();
    let logs_root = common.join("logs").display().to_string();
    let run_ref = common
        .join("refs")
        .join("heads")
        .join("agent")
        .join("demo")
        .display()
        .to_string();
    let run_reflog = common
        .join("logs")
        .join("refs")
        .join("heads")
        .join("agent")
        .join("demo")
        .display()
        .to_string();
    assert!(
        !grants.contains(&&refs_root),
        "refs root must not be granted: {grants:?}"
    );
    assert!(
        !grants.contains(&&logs_root),
        "logs root must not be granted: {grants:?}"
    );
    assert!(
        grants.contains(&&run_ref),
        "run ref directory must be present: {grants:?}"
    );
    assert!(
        grants.contains(&&run_reflog),
        "run reflog directory must be present: {grants:?}"
    );
    assert!(
        grants.iter().all(|path| {
            let path = Path::new(path);
            path.ends_with("objects")
                || path.ends_with(Path::new("refs/heads/agent/demo"))
                || path.ends_with(Path::new("logs/refs/heads/agent/demo"))
                || path.ends_with(Path::new("worktrees").join("demo"))
        }),
        "`.git` must stay an allow-list of specific metadata paths: {grants:?}"
    );
}

#[test]
fn wrap_refuses_a_worktree_branch_without_a_slash() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let mut worktree = worktree_under(&root);
    worktree.branch = "noslash".to_owned();
    let spec = SandboxSpec {
        checkout: &worktree.path,
        worktree: Some(&worktree),
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let error = subject
        .wrap_agent(vec!["codex".to_owned()], &spec)
        .expect_err("refuse branch without slash");
    match error {
        Error::Sandbox { detail } => {
            assert!(
                detail.contains("missing ref namespace"),
                "error must name the missing ref namespace: {detail}"
            );
        }
        other => panic!("expected Error::Sandbox, got {other:?}"),
    }
}

#[test]
fn wrap_agent_without_a_worktree_adds_no_git_grants() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("shared-checkout");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };

    let wrapped = subject
        .wrap_agent(vec!["codex".to_owned()], &spec)
        .expect("wrap");
    assert!(!wrapped.iter().any(|arg| arg == "--allow"), "{wrapped:?}");
    assert!(!wrapped.iter().any(|arg| arg == "--read"), "{wrapped:?}");
    assert!(
        wrapped
            .iter()
            .any(|arg| arg == &checkout.display().to_string())
    );
}

#[test]
fn wrap_review_uses_the_read_only_profile_and_no_git_grants() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let worktree = worktree_under(&root);
    let brief_dir = root.path().join("brief");
    let spec = SandboxSpec {
        checkout: &worktree.path,
        worktree: Some(&worktree),
        read_paths: vec![brief_dir.as_path()],
        read_files: Vec::new(),
        loopback_ports: vec![9],
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };

    let wrapped = subject
        .wrap_review(vec!["codex".to_owned(), "review".to_owned()], &spec)
        .expect("wrap review");

    let profile_flag = wrapped
        .iter()
        .position(|arg| arg == "--profile")
        .expect("a profile flag");
    assert_eq!(
        wrapped[profile_flag + 1],
        subject.review_profile().display().to_string(),
        "review must run under the review profile: {wrapped:?}"
    );
    assert_eq!(
        wrapped.last().map(String::as_str),
        Some("review"),
        "{wrapped:?}"
    );
    // nono refuses a non-interactive child with a read-only workdir unless CWD read access is
    // explicit; a review must carry this or it dies before the agent starts.
    assert!(
        wrapped.iter().any(|arg| arg == "--allow-cwd"),
        "a read-only review must grant CWD access: {wrapped:?}"
    );
    assert!(
        !wrapped.iter().any(|arg| arg == "--allow"),
        "a review must not receive a write grant: {wrapped:?}"
    );
    assert!(
        !wrapped
            .iter()
            .any(|arg| arg == &worktree.git_common_dir.display().to_string()),
        "a review must not receive the git common directory: {wrapped:?}"
    );
    assert!(
        wrapped
            .iter()
            .any(|arg| arg == &brief_dir.display().to_string()),
        "a review still gets the caller's read grants: {wrapped:?}"
    );
}

#[test]
fn wrap_refuses_an_empty_argv() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };

    let error = subject
        .wrap_agent(Vec::new(), &spec)
        .expect_err("empty argv");
    assert!(error.to_string().contains("empty argv"), "{error}");

    let error = subject
        .wrap_review(Vec::new(), &spec)
        .expect_err("empty argv");
    assert!(error.to_string().contains("empty argv"), "{error}");
}

#[test]
fn the_wrapped_argv_executes_the_child_after_the_separator() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("the checkout directory");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };

    let wrapped = subject
        .wrap_agent(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf 'wrapped-ok %s' \"$KEEP_ME\"".to_owned(),
            ],
            &spec,
        )
        .expect("wrap");

    let mut env = BTreeMap::new();
    env.insert(
        "PATH".to_owned(),
        "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
    );
    env.insert("KEEP_ME".to_owned(), "1".to_owned());
    env.insert("NONO_THEME".to_owned(), "latte".to_owned());
    env.insert("NONO_ALLOW".to_owned(), "/".to_owned());
    Sandbox::scrub_env(&mut env);

    let output = Command::new(&wrapped[0])
        .args(&wrapped[1..])
        .env_clear()
        .envs(&env)
        .output()
        .expect("run the wrapped argv");
    assert!(
        output.status.success(),
        "wrapped launch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "wrapped-ok 1");

    let recorded = fake.recorded_argv();
    assert_eq!(
        recorded.first().map(String::as_str),
        Some("-s"),
        "the wrapper must start with silent mode: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|arg| arg == "--no-diagnostics"),
        "{recorded:?}"
    );
    assert_eq!(
        recorded.iter().filter(|arg| arg.as_str() == "--").count(),
        1,
        "exactly one separator: {recorded:?}"
    );
    assert_eq!(
        recorded.last().map(String::as_str),
        Some("printf 'wrapped-ok %s' \"$KEEP_ME\""),
        "{recorded:?}"
    );

    let child_env = fake.recorded_env();
    assert_eq!(child_env.get("KEEP_ME").map(String::as_str), Some("1"));
    assert!(
        !child_env.keys().any(|key| key.starts_with("NONO_")),
        "no NONO_* variable may reach the child: {child_env:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Environment scrub and refusal classification
// ---------------------------------------------------------------------------------------------

#[test]
fn scrubbing_removes_loader_injection_before_the_supervisor_starts() {
    let mut env = BTreeMap::from([
        (
            "DYLD_INSERT_LIBRARIES".to_owned(),
            "/tmp/inject.dylib".to_owned(),
        ),
        ("DYLD_FRAMEWORK_PATH".to_owned(), "/tmp".to_owned()),
        ("LD_PRELOAD".to_owned(), "/tmp/inject.so".to_owned()),
        ("HOME".to_owned(), "/Users/example".to_owned()),
        (
            "XDG_STATE_HOME".to_owned(),
            "/Users/example/.state".to_owned(),
        ),
        (
            "dyld_insert_libraries".to_owned(),
            "lowercase-is-not-the-prefix".to_owned(),
        ),
    ]);
    Sandbox::scrub_env(&mut env);

    // A constructor in either variable runs inside the unsandboxed `nono` before any profile is
    // applied, so both must go; HOME and XDG are kept because preflight proves the state root.
    assert!(!env.contains_key("DYLD_INSERT_LIBRARIES"), "{env:?}");
    assert!(!env.contains_key("DYLD_FRAMEWORK_PATH"), "{env:?}");
    assert!(!env.contains_key("LD_PRELOAD"), "{env:?}");
    assert_eq!(env.get("HOME").map(String::as_str), Some("/Users/example"));
    assert_eq!(
        env.get("XDG_STATE_HOME").map(String::as_str),
        Some("/Users/example/.state")
    );
    assert_eq!(
        env.get("dyld_insert_libraries").map(String::as_str),
        Some("lowercase-is-not-the-prefix"),
        "the prefixes are case-sensitive, like environment variables themselves"
    );
}

#[test]
fn scrubbing_removes_every_nono_prefixed_variable_case_sensitively() {
    let mut env = BTreeMap::from([
        ("NONO_THEME".to_owned(), "latte".to_owned()),
        ("NONO_ALLOW".to_owned(), "/".to_owned()),
        ("NONO".to_owned(), "bare-prefix".to_owned()),
        (
            "nono_theme".to_owned(),
            "lowercase-is-not-the-prefix".to_owned(),
        ),
        ("PATH".to_owned(), "/bin".to_owned()),
    ]);
    Sandbox::scrub_env(&mut env);

    assert_eq!(env.len(), 3, "{env:?}");
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
    assert_eq!(
        env.get("nono_theme").map(String::as_str),
        Some("lowercase-is-not-the-prefix")
    );
    assert_eq!(
        env.get("NONO").map(String::as_str),
        Some("bare-prefix"),
        "only the `NONO_` prefix is scrubbed, not a variable named NONO"
    );
    assert!(!env.keys().any(|key| key.starts_with("NONO_")));
}

#[test]
fn a_refusal_is_quiet_stdout_and_a_nono_prefixed_first_stderr_line() {
    assert!(Sandbox::classify_refusal(
        "",
        "nono: sandbox initialization failed: Operation not permitted\n"
    ));
    assert!(Sandbox::classify_refusal(
        "   \n",
        "\n\nnono: profile rejected\n"
    ));

    // The agent's own stdout or stderr is not the sandbox's refusal shape.
    assert!(!Sandbox::classify_refusal(
        "some agent output\n",
        "nono: nope"
    ));
    assert!(!Sandbox::classify_refusal("", "permission denied\n"));
    assert!(!Sandbox::classify_refusal(
        "",
        "nono v0.80.0\nnono: later line\n"
    ));
    assert!(!Sandbox::classify_refusal("", "  nono: indented\n"));
    assert!(!Sandbox::classify_refusal("", "agent: nono: embedded\n"));
    assert!(!Sandbox::classify_refusal("", ""));
}

// ---------------------------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn doctor_reports_a_usable_sandbox() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);
    subject.preflight().await.expect("preflight");

    let lines = subject.doctor_lines().await;
    assert!(!lines.is_empty(), "doctor must report something");
    assert!(
        lines.iter().all(|line| line.level == DoctorLevel::Ok),
        "a usable sandbox must be all Ok: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.text.contains("0.80.0")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn doctor_warns_about_a_binary_newer_than_the_tested_version() {
    let fake = FakeNono::new("0.99.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);
    subject.preflight().await.expect("preflight");

    let lines = subject.doctor_lines().await;
    let warning = lines
        .iter()
        .find(|line| line.level == DoctorLevel::Warn)
        .unwrap_or_else(|| panic!("expected a warning line: {lines:?}"));
    assert!(warning.text.contains("0.99.0"), "{}", warning.text);
    assert!(warning.text.contains("newer"), "{}", warning.text);
}

#[tokio::test]
async fn doctor_fails_when_the_binary_or_a_published_profile_is_gone() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);
    subject.preflight().await.expect("preflight");

    std::fs::remove_file(subject.binary()).expect("remove the fake binary");
    let lines = subject.doctor_lines().await;
    assert!(
        lines.iter().any(|line| line.level == DoctorLevel::Fail
            && line.text.contains("missing or not executable")),
        "{lines:?}"
    );

    let review = subject.review_profile().to_path_buf();
    std::fs::remove_file(&review).expect("remove a published profile");
    let lines = subject.doctor_lines().await;
    assert!(
        lines
            .iter()
            .any(|line| line.level == DoctorLevel::Fail && line.text.contains("cannot be read")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn doctor_fails_when_a_published_profile_was_edited() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("the state base directory");
    set_state_root(&state);
    subject.preflight().await.expect("preflight");

    std::fs::write(subject.agent_profile(), "{\"edited\":true}\n").expect("tamper the profile");
    let lines = subject.doctor_lines().await;
    assert!(
        lines
            .iter()
            .any(|line| line.level == DoctorLevel::Fail && line.text.contains("does not match")),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The shared-checkout guard
// ---------------------------------------------------------------------------------------------

/// Run Git in `directory`, failing the test on a non-zero exit.
fn git_in(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fresh repository with one commit.
fn init_repository(directory: &Path) {
    git_in(directory, &["init", "--quiet", "."]);
    git_in(
        directory,
        &["config", "user.email", "workcell@example.invalid"],
    );
    git_in(directory, &["config", "user.name", "workcell"]);
    git_in(
        directory,
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    );
}

#[tokio::test]
async fn the_guard_passes_an_untouched_repository() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let git = Git::new(root.path());

    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");
    Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect("an untouched repository passes the guard");
}

#[tokio::test]
async fn the_guard_refuses_a_changed_git_configuration() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");

    let config_path = root.path().join(".git").join("config");
    let mut config = std::fs::read_to_string(&config_path).expect("read .git/config");
    config.push_str("\n[workcell-test]\n\tplanted = true\n");
    std::fs::write(&config_path, config).expect("plant a config key");

    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("a changed configuration must fail the guard");
    let text = error.to_string();
    assert!(text.contains("git configuration"), "{text}");
    assert!(!text.contains("repository hooks"), "{text}");
}

#[tokio::test]
async fn the_guard_refuses_a_planted_hook() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");

    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("the hooks directory");
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").expect("plant a hook");
    make_executable(&hook);

    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("a planted hook must fail the guard");
    let text = error.to_string();
    assert!(text.contains("repository hooks"), "{text}");
    assert!(!text.contains("git configuration"), "{text}");
}

#[tokio::test]
async fn the_guard_refuses_an_existing_hook_rewritten_with_the_same_name() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("the hooks directory");
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").expect("write the honest hook");
    make_executable(&hook);

    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");
    Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect("an unchanged existing hook passes the guard");

    // Same name, same length, different bytes: only the contents changed, which is the
    // CVE-2026-26268 shape a name-only digest would miss.
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("rewrite the hook");
    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("a rewritten existing hook must fail the guard");
    let text = error.to_string();
    assert!(text.contains("repository hooks"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn the_guard_hashes_the_target_of_a_symlinked_hook() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("the hooks directory");
    let target = root.path().join("hook-target.sh");
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").expect("write the hook target");
    make_executable(&target);
    let hook = hooks.join("pre-commit");
    std::os::unix::fs::symlink(&target, &hook).expect("symlink the hook into the hooks directory");

    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");
    Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect("an unchanged symlinked hook passes the guard");

    // Rewrite the target with the same length: the linked content Git would execute changed.
    std::fs::write(&target, "#!/bin/sh\nexit 1\n").expect("rewrite the hook target");
    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("a changed symlink target must fail the guard");
    assert!(error.to_string().contains("repository hooks"), "{error}");
}

#[tokio::test]
async fn the_guard_reports_an_unreadable_configuration_as_a_sandbox_failure() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");

    // A malformed config makes `git config --list --includes` fail; the guard cannot observe, so
    // it must fail closed in the non-retryable sandbox class rather than a bare Git error.
    std::fs::write(root.path().join(".git").join("config"), "[not a section\n")
        .expect("corrupt the repository configuration");
    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("an unreadable configuration must fail the guard");
    assert!(
        matches!(error, Error::Sandbox { .. }),
        "expected Error::Sandbox, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("cannot read the repository configuration"),
        "{error}"
    );
}

#[tokio::test]
async fn the_guard_snapshot_is_stable_across_reads() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let git = Git::new(root.path());

    let first = Sandbox::guard_snapshot(&git).await.expect("snapshot");
    let second = Sandbox::guard_snapshot(&git).await.expect("snapshot");
    assert_eq!(
        first, second,
        "an unchanged repository must hash identically"
    );
    assert!(first.starts_with("v4:"), "{first}");
    assert_eq!(first.split(':').count(), 5, "four surface digests: {first}");
}

/// The Unix permission bits of `path`.
#[cfg(unix)]
#[tokio::test]
#[ignore = "needs a real nono >= 0.80.0 and a host that is not itself sandboxed; a nested Seatbelt cannot initialize"]
async fn real_nono_resolves_preflights_and_launches_a_child() {
    let root = TempDir::new().expect("a temporary directory");
    let profile_dir = root.path().join("profiles");
    let subject = Sandbox::resolve(&profile_dir).expect(
        "the real nono binary must resolve on PATH; install nono >= 0.80.0 to run this drill",
    );
    subject
        .preflight()
        .await
        .expect("a plain host must be able to enforce the sandbox");

    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("the checkout directory");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let wrapped = subject
        .wrap_agent(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf 'real-nono-ok'".to_owned(),
            ],
            &spec,
        )
        .expect("wrap");

    let output = Command::new(&wrapped[0])
        .args(&wrapped[1..])
        .output()
        .expect("run the real sandbox");
    assert!(
        output.status.success(),
        "real nono launch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "real-nono-ok");
    assert!(
        !Sandbox::classify_refusal(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr)
        ),
        "a successful launch must not look like a refusal"
    );
}

// ---------------------------------------------------------------------------------------------
// Substituted targets and forged layouts
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn preflight_refuses_a_published_profile_that_is_not_a_regular_file() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);

    // A FIFO at the content-addressed path blocks a plain read forever. The profile must be
    // refused before any content is consumed, not after a launch hangs.
    let profile = subject.agent_profile().to_path_buf();
    std::fs::create_dir_all(profile.parent().expect("parent")).expect("profile directory");
    let made = Command::new("mkfifo")
        .arg(&profile)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");

    let error = subject
        .preflight()
        .await
        .expect_err("a FIFO is not a published profile");
    let text = error.to_string();
    assert!(text.contains("not a regular file"), "{text}");
}

#[test]
fn resolve_makes_a_relative_profile_directory_absolute() {
    let fake = FakeNono::new("0.80.0");
    fake.activate();

    // A relative directory would resolve against the caller's cwd here and against the
    // checkout's cwd in the child, so preflight and launch could read different files.
    let subject = Sandbox::resolve("relative-profile-dir").expect("resolve");
    assert!(
        subject.profile_dir().is_absolute(),
        "the profile directory must not depend on the caller's cwd: {:?}",
        subject.profile_dir()
    );
    assert!(subject.profile_dir().ends_with("relative-profile-dir"));
    assert!(subject.agent_profile().is_absolute());
}

#[tokio::test]
async fn the_guard_refuses_an_oversized_dot_git_entry() {
    let root = TempDir::new().expect("a temporary directory");
    let base = root.path().join("base");
    std::fs::create_dir_all(&base).expect("base");
    init_repository(&base);

    // A linked worktree's `.git` is a regular file inside the writable checkout, so an agent can
    // grow it after the valid `gitdir:` line. Refusing is the guarantee: the guard may not read an
    // attacker-sized file, and it fails closed whether Git rejects the pointer first (it does,
    // with a `gitdir:` path that no longer resolves) or the guard's own byte bound is reached.
    // The bound in `read_dot_git_entry` is defence in depth for a pointer Git would still accept.
    let checkout = root.path().join("checkout");
    git_in(
        &base,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "escape",
            checkout.to_str().expect("utf-8 path"),
        ],
    );
    let pointer = checkout.join(".git");
    let mut grown = std::fs::read(&pointer).expect("read the pointer");
    grown.push(b'\n');
    grown.extend(std::iter::repeat_n(b'A', 96 * 1024));
    std::fs::write(&pointer, &grown).expect("grow the pointer");

    let git = Git::new(&checkout);
    let outcome = Sandbox::guard_snapshot(&git).await;
    // A panic message must never carry the grown pointer: the observed Git error embeds it.
    let refused = match &outcome {
        Ok(_) => false,
        Err(Error::Sandbox { detail }) => {
            detail.contains("refusing to continue") || detail.contains("past the")
        }
        Err(_) => false,
    };
    assert!(
        refused,
        "an oversized `.git` entry must be refused, not snapshotted"
    );
}

#[test]
fn wrap_refuses_a_worktree_grant_that_is_not_the_named_entry() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);

    // `Worktree`'s fields are public, so a caller-built value must not be able to widen the
    // write grant to the whole common Git directory. Two forgeries, two refusals.
    for (git_dir, expected) in [
        (
            worktree_under(&root)
                .git_common_dir
                .join("worktrees")
                .join(".."),
            "parent-directory component",
        ),
        (
            worktree_under(&root).git_common_dir.join("hooks"),
            "refusing to widen",
        ),
    ] {
        let mut worktree = worktree_under(&root);
        worktree.git_dir = git_dir;
        let spec = SandboxSpec {
            checkout: &worktree.path,
            worktree: Some(&worktree),
            read_paths: Vec::new(),
            read_files: Vec::new(),
            loopback_ports: Vec::new(),
            listen_ports: Vec::new(),
            allow_domains: Vec::new(),
            bypass_protection: Vec::new(),
            writable_paths: Vec::new(),
        };
        let error = subject
            .wrap_agent(vec!["codex".to_owned()], &spec)
            .expect_err("a forged worktree layout is refused");
        let text = error.to_string();
        assert!(text.contains(expected), "expected `{expected}` in: {text}");
    }
}

#[tokio::test]
async fn the_guard_refuses_an_unreadable_hook() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks directory");
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, b"#!/bin/sh\n").expect("write the hook");
    let mut permissions = std::fs::metadata(&hook).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o000);
    std::fs::set_permissions(&hook, permissions).expect("make the hook unreadable");

    let git = Git::new(root.path());
    let outcome = Sandbox::guard_snapshot(&git).await;
    let refused = match &outcome {
        Ok(_) => false,
        Err(Error::Sandbox { detail }) => detail.contains("cannot read"),
        Err(_) => false,
    };
    assert!(
        refused,
        "an unreadable hook must fail closed, not hash by name"
    );
}

#[tokio::test]
async fn the_guard_refuses_a_surface_past_its_entry_cap() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let info = root.path().join(".git").join("info");
    std::fs::create_dir_all(&info).expect("info directory");
    // One past the documented cap. The cap must fire before the entries are read, so this stays
    // cheap: a hostile repository cannot make the guard allocate by growing the directory.
    for index in 0..257 {
        std::fs::write(info.join(format!("entry-{index:03}")), b"x").expect("write an info entry");
    }

    let git = Git::new(root.path());
    let outcome = Sandbox::guard_snapshot(&git).await;
    let refused = match &outcome {
        Ok(_) => false,
        Err(Error::Sandbox { detail }) => detail.contains("entry cap"),
        Err(_) => false,
    };
    assert!(
        refused,
        "a surface past its entry cap must be refused before reading"
    );
}

#[tokio::test]
async fn probes_do_not_inherit_sandbox_controls_or_loader_injection() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    // SAFETY: nextest runs each test in its own process, and these are read only by the probe
    // children this test spawns.
    unsafe {
        std::env::set_var("NONO_ESCAPE_PROBE", "1");
        std::env::set_var("LD_PRELOAD", "/nonexistent/workcell-probe.dylib");
    }

    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");

    let env = fake.recorded_env();
    for key in ["NONO_ESCAPE_PROBE", "LD_PRELOAD"] {
        assert!(
            !env.contains_key(key),
            "{key} reached a sandbox probe: {:?}",
            env.get(key)
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Guard surfaces: the hooks path Git uses, the content cap, and mode bits
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_guard_refuses_an_entry_past_its_content_cap() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks directory");
    // One byte past the hook content cap. The digest keeps only the prefix, so a larger entry
    // could be rewritten past the cap without moving it; the guard must refuse instead.
    std::fs::write(hooks.join("pre-commit"), vec![b'A'; 64 * 1024 + 1]).expect("write the hook");

    let git = Git::new(root.path());
    let outcome = Sandbox::guard_snapshot(&git).await;
    let refused = match &outcome {
        Ok(_) => false,
        Err(Error::Sandbox { detail }) => detail.contains("larger than"),
        Err(_) => false,
    };
    assert!(refused, "an entry past the content cap must be refused");
}

#[tokio::test]
async fn the_guard_hashes_the_configured_hooks_path() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    git_in(root.path(), &["config", "core.hooksPath", ".githooks"]);
    let hooks = root.path().join(".githooks");
    std::fs::create_dir_all(&hooks).expect("hooks directory");
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, b"#!/bin/sh\necho first\n").expect("write the hook");

    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");

    std::fs::write(&hook, b"#!/bin/sh\necho second\n").expect("rewrite the hook");
    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("a rewrite under core.hooksPath must be detected");
    assert!(error.to_string().contains("hooks"), "{error}");
}

#[tokio::test]
async fn the_guard_detects_a_mode_only_hook_change() {
    let root = TempDir::new().expect("a temporary directory");
    init_repository(root.path());
    let hooks = root.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks directory");
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, b"#!/bin/sh\ntrue\n").expect("write the hook");

    let git = Git::new(root.path());
    let snapshot = Sandbox::guard_snapshot(&git).await.expect("snapshot");

    let mut permissions = std::fs::metadata(&hook).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).expect("make the hook executable");

    let error = Sandbox::guard_verify(&git, &snapshot)
        .await
        .expect_err("an executable bit is a change");
    assert!(error.to_string().contains("hooks"), "{error}");
}

#[tokio::test]
async fn verify_profiles_refuses_a_profile_swapped_after_preflight() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");

    // A writer with access to the profile directory swaps in a weaker-but-valid document after
    // preflight; the launch path must re-read and refuse it.
    std::fs::write(subject.agent_profile(), b"{\"extends\":\"default\"}\n")
        .expect("swap the profile");
    let error = subject
        .verify_profiles()
        .await
        .expect_err("a swapped profile must be refused before launch");
    assert!(error.to_string().contains("no longer matches"), "{error}");
}

#[test]
fn wrap_agent_grants_listen_without_connect() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: vec![0],
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let argv = subject
        .wrap_agent(vec!["agy".to_owned()], &spec)
        .expect("wrap");
    assert!(
        argv.windows(2).any(|pair| pair == ["--listen-port", "0"]),
        "a listen-only grant must reach nono: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "--open-port"),
        "listen-only must not grant the connect half: {argv:?}"
    );
}

#[test]
fn wrap_refuses_a_relative_grant() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    let relative = std::path::Path::new("relative/brief");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: vec![relative],
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let error = subject
        .wrap_agent(vec!["agy".to_owned()], &spec)
        .expect_err("a relative grant must be refused");
    assert!(error.to_string().contains("not absolute"), "{error}");
}

#[test]
fn wrap_refuses_a_relative_checkout() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let relative = std::path::Path::new("relative/checkout");
    let spec = SandboxSpec {
        checkout: relative,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let error = subject
        .wrap_review(vec!["agy".to_owned()], &spec)
        .expect_err("a relative checkout must be refused");
    assert!(error.to_string().contains("not absolute"), "{error}");
}

#[test]
fn wrap_grants_a_caller_supplied_protection_bypass() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    let keychain = std::path::Path::new("/Users/example/Library/Keychains/login.keychain-db");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: vec![keychain],
        writable_paths: Vec::new(),
    };
    let argv = subject
        .wrap_agent(vec!["claude".to_owned()], &spec)
        .expect("wrap");
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--bypass-protection", keychain.to_str().unwrap()]),
        "a caller-supplied bypass must reach nono: {argv:?}"
    );
}

#[test]
fn wrap_verify_uses_the_verification_profile() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let argv = subject
        .wrap_verify(vec!["make".to_owned(), "verify".to_owned()], &spec)
        .expect("wrap");
    let profile = argv
        .iter()
        .position(|argument| argument == "--profile")
        .map(|index| argv[index + 1].as_str())
        .expect("a --profile argument");
    assert!(
        profile.ends_with("-verify.json"),
        "the verification launch must use the verification profile: {profile}"
    );
    assert!(
        !argv.iter().any(|argument| argument == "--allow-cwd"),
        "a writable workdir needs no allow-cwd: {argv:?}"
    );
}

#[test]
fn the_verify_profile_is_writable_and_offline() {
    let profile = embedded_profile(VERIFY_PROFILE);
    assert_eq!(profile.extends, "default");
    // A build must write its target directory; the sandbox's purpose here is the network and
    // credential boundary, not a read-only checkout.
    assert_eq!(profile.workdir.access, "readwrite");
    assert!(profile.network.block, "verification runs offline");
    assert!(
        profile.network.allow_domain.is_empty(),
        "the verify profile allows no host by default: {:?}",
        profile.network.allow_domain
    );
    for forbidden in [
        "$HOME/.claude",
        "$HOME/.claude.json",
        "$HOME/.codex",
        "$HOME/.gemini",
        "$HOME/.dsh",
        "$HOME/Library/Keychains/login.keychain-db",
    ] {
        assert!(
            !profile.filesystem.read.iter().any(|path| path == forbidden),
            "the verify profile must not read `{forbidden}`"
        );
    }
    for required in REQUIRED_DENIES {
        assert_contains(&profile.filesystem.deny, required);
    }
}

#[tokio::test]
async fn doctor_reports_the_read_only_package_caches() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let state = root.path().join("xdg-state");
    std::fs::create_dir_all(&state).expect("state");
    set_state_root(&state);
    let subject = resolve(&fake, &root);
    subject.preflight().await.expect("preflight");

    let lines = subject.doctor_lines().await;
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("package caches: read-only")),
        "doctor must name the cold-cache trade instead of leaving it to be discovered mid-run: {lines:?}"
    );
}

#[test]
fn wrap_refuses_a_writable_grant_above_the_checkout() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("repo");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let ancestor = root.path().to_path_buf();
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: vec![&ancestor],
    };
    let error = subject
        .wrap_agent(vec!["agy".to_owned()], &spec)
        .expect_err("a grant above the checkout must be refused");
    assert!(
        error.to_string().contains("ancestor of the checkout"),
        "{error}"
    );

    // A sibling grant stays legal: the caller's per-run state lives outside the worktree.
    let sibling = root.path().join("state");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: Vec::new(),
        bypass_protection: Vec::new(),
        writable_paths: vec![&sibling],
    };
    subject
        .wrap_agent(vec!["agy".to_owned()], &spec)
        .expect("a sibling grant is not an ancestor");
}

/// A supervisor stand-in that answers the audit queries with canned sessions.
fn audit_stub(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin directory");
    let nono = bin.join("nono");
    std::fs::write(
        &nono,
        r#"#!/bin/sh
case "$1" in
  --version) echo 'nono 0.80.0'; exit 0 ;;
  audit)
    case "$2" in
      list)
        cat <<'JSON'
[
 {"session_id":"old","started":"2026-10-28T10:00:00.000000+02:00","ended":"2026-10-28T10:00:01.000000+02:00","command":["claude","-p","x"]},
 {"session_id":"new","started":"2026-10-28T10:00:02.000000+02:00","ended":"2026-10-28T10:00:03.000000+02:00","command":["claude","-p","x"]},
 {"session_id":"other","started":"2026-10-28T10:00:04.000000+02:00","ended":"2026-10-28T10:00:05.000000+02:00","command":["codex","exec"]}
]
JSON
        ;;
      show)
        cat <<'JSON'
{"session_id":"new","network_events":[
 {"decision":"deny","denial_category":"host_denied","target":"example.com","port":443,"reason":"host example.com:443 is not in the allowlist"},
 {"decision":"allow","target":"api.anthropic.com","port":443,"reason":"allowlisted"}
],"command_policy_events":[]}
JSON
        ;;
    esac ;;
esac
"#,
    )
    .expect("write the audit stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nono).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nono, permissions).expect("make it executable");
    }
    let old = std::env::var_os("PATH").unwrap_or_default();
    // SAFETY: nextest gives each test its own process.
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin.display(), old.to_string_lossy()),
        );
    }
    nono
}

#[tokio::test]
async fn audit_picks_the_newest_session_with_the_same_command_and_parses_decisions() {
    let root = TempDir::new().expect("a temporary directory");
    let _nono = audit_stub(root.path());
    let sandbox = Sandbox::resolve(root.path().join("profiles")).expect("resolve");

    let audit = sandbox
        .audit_session(&["claude".to_owned(), "-p".to_owned(), "x".to_owned()])
        .await
        .expect("a matching session");
    assert_eq!(audit.session_id, "new", "the newest matching session wins");
    assert_eq!(audit.network.len(), 2, "{audit:?}");
    assert_eq!(audit.network[0].decision, "deny");
    assert_eq!(audit.network[0].category.as_deref(), Some("host_denied"));
    assert_eq!(audit.network[0].target, "example.com");
    assert_eq!(audit.network[0].port, Some(443));
    assert!(audit.network[0].reason.contains("not in the allowlist"));
    assert_eq!(audit.network[1].decision, "allow");
    assert_eq!(audit.command_policy_events, 0);

    // A command the supervisor never saw yields no audit, not an error.
    assert!(
        sandbox
            .audit_session(&["never".to_owned(), "ran".to_owned()])
            .await
            .is_none()
    );
}

#[test]
fn wrap_grants_a_caller_supplied_egress_domain() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let spec = SandboxSpec {
        checkout: &checkout,
        worktree: None,
        read_paths: Vec::new(),
        read_files: Vec::new(),
        loopback_ports: Vec::new(),
        listen_ports: Vec::new(),
        allow_domains: vec!["api.deepseek.com"],
        bypass_protection: Vec::new(),
        writable_paths: Vec::new(),
    };
    let argv = subject
        .wrap_review(vec!["dsh".to_owned()], &spec)
        .expect("wrap");
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--allow-domain", "api.deepseek.com"]),
        "a caller-supplied egress grant must reach nono: {argv:?}"
    );
}

#[test]
fn wrap_refuses_an_egress_grant_that_is_not_a_bare_hostname() {
    let fake = FakeNono::new("0.80.0");
    let root = TempDir::new().expect("a temporary directory");
    let subject = resolve(&fake, &root);
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    for bad in ["https://example.com/x", "example.com:443", "has space"] {
        let spec = SandboxSpec {
            checkout: &checkout,
            worktree: None,
            read_paths: Vec::new(),
            read_files: Vec::new(),
            loopback_ports: Vec::new(),
            listen_ports: Vec::new(),
            allow_domains: vec![bad],
            bypass_protection: Vec::new(),
            writable_paths: Vec::new(),
        };
        let error = subject
            .wrap_review(vec!["dsh".to_owned()], &spec)
            .expect_err("a URL or port is not a hostname");
        assert!(
            error.to_string().contains("not a bare hostname"),
            "{bad}: {error}"
        );
    }
}

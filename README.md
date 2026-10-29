# workcell

A sandboxed, git-backed workspace for running untrusted coding agents.

`workcell` is a library, not a service. It owns the parts of a coding-agent runtime that do not
depend on a task tracker, a model provider, or a review workflow: an in-process git2 Git runner,
one Git worktree per unit of work, conservative garbage collection, bounded child-process
execution, a bounded and root-confined instruction-file read, bounded and atomic file helpers, and
an **always-on OS sandbox** around the agent process (macOS Seatbelt in this release). A caller
supplies its own orchestration.

The crate is **async-first**. Only `sandbox::Sandbox::resolve` (a `PATH` lookup with no side
effects) and the pure helpers are synchronous; publishing the profiles, probing the binary,
reading instruction files, writing files, collecting worktrees, supervising a child, and hashing
the repository guard surfaces are asynchronous and run on the caller's Tokio runtime.

## Why

Permission flags are a policy, not a boundary. A coding agent is given a non-interactive
full-permission flag so it can run builds and tests without a TTY, and from that moment every child
process it starts has the invoking user's authority. Two independently documented failures make the
gap concrete:

- **CVE-2026-26268 / GHSA-8pcm-8jpx-hv8r (Cursor, Feb 2026).** A prompt-injected agent wrote
  malicious Git hooks; Git then executed them **outside** the sandbox, without user interaction.
  The write was inside the workspace the sandbox had made writable, and the execution happened in
  the tool that ran after the sandbox.
- **NVIDIA AI Red Team's sandboxing guidance (Jan 2026)** lists three controls as *mandatory* for
  agentic workloads: block network egress to unknown destinations, block file writes outside the
  active workspace, and block **all** writes to agent configuration files — because hooks, skills,
  and local MCP servers often run outside the sandbox. It also notes that application-level
  interception ends when control passes to a subprocess; only an OS-level boundary covers every
  process.

`workcell` therefore does not ship a "sandboxed mode". The sandbox is a prerequisite and the only
launch path in its API: `sandbox::Sandbox::resolve` fails when the sandbox binary is missing, and
`sandbox::Sandbox::preflight` fails when the host cannot enforce an OS sandbox. There is no
configuration key, environment variable, or constructor that runs an agent unconfined.

## What it does

| Module | Holds |
| --- | --- |
| `git` | An in-process git2/libgit2 runner and the primitives a coding workspace needs. libgit2 executes no repository hooks and no external `diff.<driver>.textconv`/`command` or `GIT_EXTERNAL_DIFF` programs, so a repository cannot execute code in the caller's process tree through Git. Credential helpers are global-only (a repository-local one is ignored) and run with a scrubbed environment; the shared-checkout guard hashes a v4 config digest plus bounded hooks and `.git/info` surfaces with their modes, detects a mismatch, and refuses. |
| `worktree` | `RepoSpec` and `Worktree`: fetch the base, cut one isolated checkout and branch per unit of work, remove it, and prune. |
| `gc` | Async, conservative collection of stale worktrees and branches, with a dry run and explicit protection for active work. |
| `process` | One child process: async spawn, timeout, optional stall watchdog, bounded in-memory capture, full log file, and a two-stage process-group kill (`SIGTERM`, 5 s grace, `SIGKILL`). |
| `sandbox` | The OS boundary: the external `nono` binary as a hard prerequisite, embedded agent and review profiles, a synchronous `resolve`, async `preflight` (publish the profiles, probe `nono --version`, validate both, prove the host), argv wrapping (`SandboxSpec::read_paths`, `writable_paths`, `read_files`, loopback connect and listen ports, and the four-path Git metadata allow-list in worktree mode: `objects`, the run's ref directory, the run's reflog directory, and the worktree git directory), environment scrubbing, refusal classification, and the shared-checkout Git-identity/configuration/hooks guard. `SandboxOptions::state_dir` names the caller's in-checkout run-control directory that both profiles deny. The policy is macOS Seatbelt-only in this release. |
| `instructions` | Async, bounded, root-confined reads of a checkout's instruction files. A path that resolves outside the checkout (including through a symlink) is refused. |
| `fs` | Async bounded reads, atomic writes, and a no-clobber publish. |
| `error` | One error type for every module; operator-facing detail, never a credential. |

### Execution model

`Sandbox::resolve` is a cheap synchronous lookup: it stats `PATH`, renders the embedded profiles,
and computes their content-addressed paths; it spawns no process and writes no file. Everything that
touches the outside world belongs to the async API — `Sandbox::preflight` publishes the rendered
profiles, probes `nono --version` (30-second deadline, empty environment, killed on drop), runs the
host-support probe, validates both published profiles, and proves the `nono` state root.
`Sandbox::version()` is `Option<&str>` and stays `None` until `preflight` (or `doctor`) has probed
it. The crate never calls a blocking `std::fs` or `std::process` operation from an async task.

## Install

```sh
cargo add workcell
```

The crate is publishable but not published yet; use a path or git dependency in the meantime.

```toml
[dependencies]
workcell = { path = "../workcell" }
```

Requirements:

- Rust 1.95 (edition 2024).
- macOS, because the seatbelt profile needs the deny-inside-grant rule. `preflight` refuses every
  other host rather than running an agent unconfined.
- `nono` **0.80.0 or newer** on `PATH` ([nono.sh](https://nono.sh)). This is a hard prerequisite.
- A Tokio runtime. The library starts none, registers no global state, and runs every I/O path on
  the caller's runtime.
- A C toolchain and OpenSSL headers are not needed: `git2` is built with `vendored-libgit2` and
  `vendored-openssl`. The library does not require a `git` binary; the tests use the system `git`
  to create fixture repositories.

## Quickstart

The full runnable form is [`examples/coding_agent.rs`](examples/coding_agent.rs), which CI builds
and which adds the CLI polish (argument parsing, a review profile, worktree discovery, and the guard
check). The abridged path is:

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use workcell::error::Result;
use workcell::process::{self, ProcessSpec};
use workcell::sandbox::{Sandbox, SandboxSpec};
use workcell::worktree::RepoSpec;

async fn run_one_agent() -> Result<()> {
    // 1. One isolated checkout and branch per unit of work.
    let repo = RepoSpec {
        root: PathBuf::from("/src/acme"),
        remote: "origin".into(),
        base_branch: "main".into(),
        worktree_root: ".worktrees".into(),
        branch_prefix: "agent".into(),
        require_clean_root: true,
        env: BTreeMap::new(),
    };
    let tree = repo.prepare("demo").await?;

    // 2. The sandbox is resolved and proven before any agent starts.
    //    (`resolve_with(.., SandboxOptions { state_dir })` names a different
    //    run-control directory; `.workcell` is the default.)
    let sandbox = Sandbox::resolve("/Users/me/.local/state/workcell/profiles")?;
    sandbox.preflight().await?;

    // 3. Wrap the agent invocation, then run it as one child process group.
    let argv = sandbox.wrap_agent(
        vec!["codex".into(), "exec".into(), "do the task".into()],
        &SandboxSpec {
            checkout: &tree.path,
            worktree: Some(&tree),
            read_paths: Vec::new(),
            writable_paths: Vec::new(),
            read_files: Vec::new(),
            loopback_ports: Vec::new(),
            listen_ports: Vec::new(),
        },
    )?;
    let outcome = process::run(
        ProcessSpec {
            argv,
            cwd: tree.path.clone(),
            env: BTreeMap::new(),
            log_path: PathBuf::from("demo-agent.log"),
        },
        Duration::from_secs(3600),
    )
    .await?;

    // 4. A sandbox refusal is not an agent failure; it is a configuration fault.
    if Sandbox::classify_refusal(&outcome.stdout, &outcome.stderr) {
        eprintln!("the sandbox refused to start the agent: {}", outcome.stderr.trim());
    }
    Ok(())
}
```

The exact `nono` invocation, profile bytes, and wrapper arguments are `workcell`'s; a caller never
constructs them. `wrap_agent` is deterministic for a given checkout and worktree, so the command
can be logged and reproduced.

The example harness wraps one command:

```sh
cargo run --example coding_agent -- /path/to/checkout -- /bin/echo hello
cargo run --example coding_agent -- /path/to/checkout --review /bin/echo review
cargo run --example coding_agent -- /path/to/worktree --worktree /bin/echo git-grants
cargo run --example coding_agent -- /path/to/checkout --guard /bin/echo guarded
cargo run --example coding_agent -- /path/to/checkout --codex-home codex exec --skip-git-repo-check "do the task"
```

It has no path that runs the command unconfined; a refusal exits non-zero with `nono`'s message.

## Security model

The sandbox is a pair of reviewed profiles compiled into the crate and published content-addressed
under the caller's profile directory, so concurrent runs share one reviewed copy and an edited copy
is refused rather than silently trusted. In brief:

- **Contained:** writes outside the checkout except a coding agent's own harness state, logs, crash
  reports, and read-only package caches; the profiles grant **no** harness configuration or
  credential path, because a hook, MCP server, or plugin planted there would run in the human's
  next session outside this sandbox — the caller supplies the harness-specific reads, read-files,
  and protection bypass for the one harness a run uses, so a child cannot read another harness's
  configuration or credential; the login keychain stays denied unless a caller names it for the
  harness that authenticates with it; in-checkout instruction and configuration files are denied read
  and write; egress is limited to a curated provider allowlist; a child is killed as a process
  group.
- **Exposed and not contained:** the shared kernel; verification runs in its own profile (network
  and credentials denied) but still writes the checkout it was handed; a harness `auth_check` runs
  in the read-only review profile with only that harness's grants; the macOS pasteboard, and the login-keychain
  item a claude run is granted by the caller (an item secret is retrievable inside that run);
  shared-checkout refs (in worktree mode refs are contained to the run's ref directory); the shared object
  store; a shared checkout's `.git`, which is guarded rather than contained; and the caller's own
  process, which `workcell` does not sandbox.

The full policy — every grant and denial, the residuals, and why each unusual grant exists — is
[`docs/security-model.md`](docs/security-model.md). The threat model behind it, the escape-testing
method, the per-hypothesis verdicts, and the upstream research are in [`docs/`](docs/README.md).

## Platform support

| Platform | Mechanism | Status |
| --- | --- | --- |
| macOS | Seatbelt via `nono` | **Supported**; the only supported host in this release |
| Linux | Landlock via `nono` | **Not supported.** Landlock cannot express a deny inside a grant — `nono` refuses the agent profile at startup — so `preflight` and `doctor` refuse the launch with that reason rather than running an agent unconfined. A Linux-compatible policy is a follow-up. |
| Windows | — | Not supported; no native path is tested or claimed |

## Honest limits

- **The sandbox is only as trustworthy as `nono`.** `workcell` wraps a binary; it does not
  reimplement Seatbelt. The minimum version is a contract on the wrapper flags, not a proof of the
  sandbox's internals, and `nono`'s own CLI is pre-1.0.
- **A deny is a path, not an inode.** Hard links and alternate names are not universally covered;
  refer to the live probe in [`docs/live-drill.md`](docs/live-drill.md) for the one-host observation.
- **Shared-checkout `.git` is detected, not contained.** Guard detection covers `.git/config`,
  `.git/config.worktree`, `.git/info`, and hooks; refs and objects remain writable by design, so
  worktree mode or an outer boundary is the stronger choice.
- **In worktree mode, refs are contained to the run's ref directory.** Writes outside
  `refs/heads/<prefix>/<label>/` and its reflogs are denied; shared-checkout refs and shared object
  stores remain named residuals.
- **Egress is a hostname allowlist, not a content policy.** `nono`'s proxy opens plain-host
  `CONNECT` tunnels for the listed provider hosts and refuses the rest. MCP and plugin domains were
  not exercised and are refused until added.
- **The package caches are read-only, but the caller's unsandboxed verification executes what the
  run wrote.** A caller that compiles or runs the checkout's code outside the sandbox inherits
  whatever the agent changed.
- **The live evidence is one host, one `nono` version, three harnesses.** grok and dsh were not
  exercised, and codex resume is broken upstream. The full list is in
  [`docs/security-model.md`](docs/security-model.md) and [`docs/live-drill.md`](docs/live-drill.md).
- **A `setsid` grandchild escapes the process-group kill.** The kill reaches the child's process
  group; a grandchild that creates a new session outlives the run (measured 2026-10-28). It stays
  inside the sandbox but can mutate the checkout after the guard read or keep a listener alive.
- **No nested sandbox on macOS.** A caller already inside another Seatbelt profile cannot start
  this one; run the caller unsandboxed and sandbox the child.
- **The caller's own work is not sandboxed.** Verification commands, `git` calls made by the
  caller, and any other process the caller starts are the caller's responsibility. `workcell` wraps
  exactly the child it is asked to wrap.

## Documentation

| Document | What it establishes |
| --- | --- |
| [`docs/security-model.md`](docs/security-model.md) | The full sandbox policy: contained, exposed, and why each grant exists. |
| [`docs/escape-testing.md`](docs/escape-testing.md) | The escape drill: what it probes, the safety rules, how to run it, the static assertions. |
| [`docs/live-drill.md`](docs/live-drill.md) | Dated live results and policy-variant experiments, with their limits. |
| [`docs/performance.md`](docs/performance.md) | Wrapper cost on one host, where it comes from, and how to re-measure. |
| [`docs/sandbox-hypotheses.md`](docs/sandbox-hypotheses.md) | One row per hypothesis with its verdict: refuted, confirmed, or needing a live probe. |
| [`docs/research/`](docs/research/README.md) | The research corpus: macOS Seatbelt, `nono` 0.80.0, the crate's own audit, and the harness surfaces the profiles encode (claude, codex, agy). |

## Examples

- `cargo run --example coding_agent -- <checkout> -- <command>` — wrap one command in the sandbox,
  with `--review`, `--worktree`, `--guard`, and `--codex-home` modes.
- `cargo run --example worktree_lifecycle -- <checkout>` — cut a worktree from a temporary
  repository, run Git in it, and collect it with `gc` (no sandbox needed).
- `cargo run --example process_limits` — spawn a child, exercise the timeout and bounded capture,
  and show the two-stage process-group kill (no sandbox needed).

## License

MIT. `nono` is Apache-2.0 and is invoked as a separate binary, never linked. The Git runner links
`git2` (MIT/Apache-2.0) against libgit2, which is GPL-2.0 with a linking exception; the vendored
build compiles those C libraries, and a distribution must carry the dependency notices. The crate
itself stays MIT.

## Contributing

- Run `make verify` before opening a pull request; it runs the same gates as CI (format, check,
  clippy, the offline test suite, doc-tests, docs, and the package check).
- Keep the crate domain-agnostic: no tracker, plan, brief, dispatch, or product concept in the
  public API, tests, examples, docs, or error messages. A caller maps its concepts onto these
  neutral types.
- The sandbox is unconditional. Do not add a configuration key, environment variable, flag, or
  constructor that runs an agent unconfined, and do not weaken a profile to make a test or a local
  run pass.
- Tests stay offline: a fake `nono` records argv and executes the command after `--`. Anything
  that needs a real Seatbelt profile belongs in `scripts/escape-drill.sh` as evidence.
- The public API is a compatibility surface; a breaking change needs a changelog entry.

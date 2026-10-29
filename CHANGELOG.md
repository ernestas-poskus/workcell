# Changelog

All notable changes to `workcell` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate follows semantic
versioning once it has a released API.

## Unreleased

### Security

- **Per-run worktree refs and narrowed Git grants.** In worktree mode, `wrap_agent` now narrows Git metadata write
  grants to a four-path allow-list: `objects`, the run's ref directory (`refs/heads/<prefix>/<label>/`), the run's
  reflog directory (`logs/refs/heads/<prefix>/<label>/`), and the worktree git directory. Neither the root `refs/` nor
  `logs/` directory is granted, closing the cross-run ref tampering path. Attempts to move refs outside the run's ref
  directory (e.g. `refs/heads/main`) are denied by the sandbox. In-run `git fetch` and tag creation are denied because
  their targets sit outside the run's ref directory; base updates are performed exclusively during integration outside
  the sandbox.
- **Nested worktree branch format and ref validation.** `RepoSpec::names` now generates nested branches of the form
  `<prefix>/<label>/head` (or `<label>/head` when prefix is empty) while preserving the checkout directory at
  `<worktree_root>/<label>`, isolating each run's ref namespace. `RepoSpec::prepare` and `Worktree::discover` validate
  and create the run's ref and reflog directories and refuse ancestor collisions and base branches placed inside the
  run's ref directory.

### Added

- **Published profiles are addressed by scheme and `nono` floor, not raw bytes.** `profile_digest` hashes a scheme marker plus the minimum `nono` version with the rendered policy, so a scheme change or a higher floor yields a different published name and a stale copy can never be reused under an address that now means something else.
- **The supervisor's network audit is readable per run.** `Sandbox::audit_session` correlates a
  completed child with the supervisor's audit trail (the newest session whose child argv matches)
  and parses its `network_events` into [`NetworkDecision`] values: allow/deny, denial category,
  target, port, and the supervisor's reason. Correlation is observability, never authorization:
  two concurrent identical commands can be confused, and a failure returns `None` rather than
  failing the run.
- **Grant validation refuses the dangerous shapes.** `wrap_agent`/`wrap_review` already refused a
  relative checkout or grant; a writable grant that is an ancestor of the checkout is now refused
  too, because it would hand the child the caller's tree (the checkout itself is the workdir grant).
- `SandboxOptions` and `Sandbox::resolve_with`: the caller names the in-checkout run-control
  directory both embedded profiles deny (`state_dir`, default `.workcell`). The profiles are
  rendered before the content address is computed, published, and verified, so a custom state
  directory is covered by the digest and the published bytes are the ones the binary receives.
  `Sandbox::resolve` keeps the default, and `state_dir` is validated as a plain relative path.
- `Makefile` and `.github/workflows/ci.yml` for the standalone crate: format, check, clippy, the
  offline test suite, doc-tests, docs, and the package check.
- Two runnable examples that need no sandbox: `worktree_lifecycle` and `process_limits`.
- `docs/security-model.md`, `docs/live-drill.md`, and `docs/performance.md`: the sandbox policy,
  the dated live evidence, and the wrapper-cost measurements, moved out of the README.
- `tests/sandbox.rs` covers the state-directory rendering: the default profile carries `.workcell`
  and not the token, a custom state directory changes the rendered denials and the content address,
  and an unusable value is refused.

### Changed

- **The crate is async-first.** `Sandbox::resolve` is now only a synchronous `PATH` lookup — it
  spawns no process and writes no file. `Sandbox::preflight` is async and owns every side effect:
  publishing the embedded profiles, probing `nono --version` (30-second deadline, empty
  environment, killed on drop), the host-support probe, validating both published profiles, and
  proving the `nono` state root. `Sandbox::doctor_lines` is async as well, and
  `Sandbox::version()` returns `Option<&str>` until preflight or doctor has probed the binary.
- `fs::{read_bounded, publish_once, write_atomic}`, `instructions::load`, `gc`, the process
  supervisor's log-directory and log-file I/O, and the shared-checkout guard's hook and `.git`
  entry reads now run on Tokio. Callers must `.await` them; `workcell` never calls blocking
  `std::fs` or `std::process` from an async task.
- The agent and review profiles now cover the grok CLI and the Antigravity/agy CLI. Both read the
  harness home and keep configuration read-only; only named state (sessions, logs, crash reports,
  caches, and a few single-file state documents) is writable. Both profiles deny
  `$WORKDIR/**/.grok/**` alongside the other in-checkout harness directories.
- Both profiles include the `node_runtime` group (codex is a Node CLI under `~/.nvm`; without it
  codex exits with `MODULE_NOT_FOUND`), and grant codex's writable state explicitly: the app-server
  directories plus the versioned SQLite databases and their `-shm`/`-wal` companions. Codex
  configuration (`config.toml`, `CODEX.md`, `AGENTS.md`, plugins, skills, rules, prompts, packages,
  `code-review-plugin/`, `computer-use/`, `vendor_imports/`, `automations/`) stays read-only.
- The **review profile now writes harness state** (codex and agy) while the checkout stays
  read-only: a read-only `~/.codex` makes codex fail at thread start even for a review.
- `wrap_review` appends `--allow-cwd`. Before the fix, every review launch under the read-only
  profile failed before the agent started with `nono: CWD access requires --allow-cwd in
  non-interactive mode`.
- Both profiles no longer read `$HOME/Library/Preferences`. The P5 variant without it ran codex,
  claude, and agy successfully, and the committed profile re-ran the full matrix and review drill
  green; the read was a Codex CFPreferences assumption that the experiment disproved.
- **`workcell::git` is in-process git2/libgit2, not a Git CLI runner.** Every operation, including
  `fetch`, runs in-process through `git2` 0.21/libgit2; no Git child process remains. libgit2
  executes no repository hooks and no external `diff.<driver>.textconv`/`command` or
  `GIT_EXTERNAL_DIFF` programs. The dependency is
  `git2 = { version = "0.21", default-features = false, features = ["vendored-libgit2", "ssh", "https", "vendored-openssl"] }`
  plus `url = "2"`; the vendored C build runs on macOS CI, and the crate stays MIT (git2 is
  MIT/Apache-2.0; libgit2 is GPL-2.0 with a linking exception).
- **Credential and signature policy moved into the crate.** Credential helpers are read only from
  global/XDG/system config (a repository-local `credential.helper` is ignored) and run with
  `env_clear()` plus the caller's supplied env; HTTPS uses URL userinfo then the helper chain; SSH
  uses ssh-agent then an unencrypted `~/.ssh/id_*`, honors `known_hosts`, and does not read
  `~/.ssh/config`; signatures come from supplied `GIT_COMMITTER_*` (used for author and committer)
  or the repository config.
- **Breaking API changes before release.** `Git::text`, `Git::status_ok`, and
  `gc::parse_worktree_list` are removed; `Git::with_env` now only feeds signatures and credential
  helpers. Added `branch_name`, `git_directories`/`GitDirectories`, `local_config_digest`,
  `changed_files_between`, and `commit_count_between`. `Diff.stat`/patch text are libgit2's format
  (bounded patch/UTF-8 boundary/complete path list unchanged; patch lines carry `+`/`-` prefixes).
  `list_worktrees` includes the main working tree first; `worktree_add` creates the branch then the
  worktree; `merge` creates `Merge branch '<ref>'`; `update_ref` is a transaction ref-lock CAS;
  `fetch` prunes stale remote refs. A missing working-tree directory still maps to
  `Error::Process`, and a path that is not a Git repository still maps to `Error::Git`.
- **The shared-checkout guard is v3.** It hashes `local_config_digest` (local config + includes +
  linked-worktree `config.worktree`, capped at 1 MiB), framed hook entries, and git2 directories;
  the snapshot prefix is `v3:`.
- All git2 work runs on `spawn_blocking`. `GIT_TIMEOUT` remains a hard timeout for local operations
  but is best-effort for `fetch`: an in-process connection blocked before libgit2's first callback
  cannot be killed, so the blocking thread may outlive the error.
- Both profiles now restrict egress to a curated provider allowlist instead of leaving the network
  open: `network.block = false` plus `network.allow_domain` for the OpenAI, Anthropic/Claude,
  Google/Antigravity, and Codex endpoints, and `network.open_port: [0]` for loopback IPC. nono's
  proxy refuses a direct connection to any other host; it is a plain-host `CONNECT` allowlist, not
  an L7/content filter.

### Changed

- The shared-checkout guard is **v4**: it also tracks `.git/info` and file mode bits. A restore mode
  was prototyped and removed before merge: restoring from a backup the same user (or the sandboxed
  child) could have written is a host-side write path with its own authority, and a review found a
  path-traversal in the manifest-driven restore. Detection plus worktree mode is the contract.
- `Sandbox::resolve` makes the profile directory **absolute**. A relative directory resolved against
  the caller's working directory at preflight and against the checkout's at launch, so the published
  bytes and the launched path could name different files.

### Fixed

- `examples/coding_agent.rs` publishes its sandbox profiles under
  `$HOME/.workcell-example-sandbox-<pid>` instead of `<checkout>/.workcell/sandbox`. The escape
  drill found that a child could append to the profile when the state directory was inside the
  writable checkout; a caller embedding the example must keep the profile directory outside it for
  the same reason.
- A published profile target must be a **regular file** under a byte bound, and its read has a
  deadline. A pre-created FIFO or character device at the content-addressed path used to block or
  read unboundedly before the bytes could be compared.
- The shared-checkout guard bounds the `.git` pointer read at 64 KiB. `git` rejects a grown pointer
  before the guard reads it, so the observable behaviour was already fail-closed; the bound removes
  the reliance on that ordering.
- `worktree_git_dir_grant` accepts only `<common>/worktrees/<name>`. `Worktree`'s fields are public,
  so a caller-built `git_dir` of `worktrees/..` could widen the worktree-mode write grant to the
  whole common Git directory; a parent-directory component or a non-matching directory is refused.

### Added

- **`doctor` names the cold-cache trade.** Package caches are read-only, so a dependency that is
  not already cached fails inside the sandbox instead of being fetched; `doctor` now says so and
  points at the pre-fetch, rather than leaving the operator to discover it mid-run.
- **`workcell::process::capture`** runs one child with bounded stdout/stderr capture, the
  process-group kill, and no durable log (a throwaway file bounded by the log cap, removed before
  returning). The verifier and `auth_check` now use it, so a repository-defined command cannot
  exhaust the dispatcher's memory by printing, and a timeout signals its whole group.
- **`auth_check` runs inside the sandbox.** An agent's optional `auth_check` was the last command
  wirepuller executed with the deployment user's authority; it now launches through the read-only
  review profile with only its own harness's caller-supplied grants. A check that must write its
  cwd, or reach a host outside the reviewed allowlist, reports the agent unavailable rather than
  running unconfined.
- **Corrected: `doctor` no longer claims `workspace.state_root` moves the state directory.** That
  setting relocates the archive/control file; the state directory follows the checkout, and worktree
  mode is the actual fix for the shared-checkout rename residual.
- **A verification profile.** `verify.profile.json` runs repository-defined verification with a
  writable checkout, network denied, no harness configuration or keychain, and read-only package
  caches; `Sandbox::wrap_verify` wraps a command for it, and preflight publishes and validates a
  third profile. A deployment whose verification genuinely needs a network host adds it to this
  reviewed profile — there is no runtime switch.

### Changed

- **Verification runs inside the sandbox.** Wirepuller's `[verify]` commands and acceptance probes
  launch through `wrap_verify` and fail the check on a refusal, instead of executing with the
  deployment user's authority. `auth_check` stays outside (it probes the harness login, not
  repository code).
- **The profiles are harness-neutral.** They no longer grant `~/.claude`, `~/.claude.json`,
  `~/.codex`, `~/.gemini`, `~/.grok`, `~/.dsh`, `~/.antigravity`, or the login keychain. A caller
  names the harness-owned paths its agent needs per run through `SandboxSpec::read_paths`,
  `read_files`, and the new `bypass_protection`; the login keychain is granted only to the harness
  that authenticates with it. This closes the cross-harness configuration read (a codex child can
  no longer read Claude's configuration) and narrows the keychain to one harness. The change also
  conforms the profiles to the crate's caller-neutrality rule.

### Added

- **`SandboxSpec::bypass_protection`**, wrapping `nono --bypass-protection` for a caller-supplied
  protected file (the macOS keychain), validated to be absolute like every other grant.
- **`listen` flags in the example** (caller-supplied `--read-path`, `--read-file`,
  `--bypass-protection`) so the manual harness matrix can reproduce each kind's grants.
- **A bounded child log.** The pumps stop at `MAX_LOG_BYTES` (16 MiB, stdout and stderr together)
  and append a single truncation marker, so a child that prints forever can no longer fill the
  disk. Before: 52,428,800 bytes in produced a 52,429,394-byte log.
- **`SandboxSpec::listen_ports`**, wrapping `--listen-port`: a wildcard bind **without** the connect
  half. Measured: agy completes under a listen-only grant, so it no longer needs the connect-capable
  `--open-port 0` that claude does (listen-only fails for claude with `Unable to connect to API
  (EPERM)`).

### Fixed

- **Relative sandbox grants are refused.** `wrap_agent`/`wrap_review` now reject a relative
  checkout or grant, because `nono` resolves a relative path against the child's cwd — the run's
  checkout, not the caller's — so a relative grant silently widens in the wrong place.
- **The shared-checkout guard covered only the first 64 KiB and ignored `core.hooksPath`.** An entry
  past the content cap is now refused instead of prefix-hashed, and the guard resolves
  `core.hooksPath` and hashes the directory Git would execute.
- **`verify_profiles` is public, bounded, and called before every launch.** Preflight alone left a
  window in which a profile could be swapped after publication; dispatch, repair, and review now
  re-read the bytes via the same bounded path as publication.
- **The preflight probes had no deadline and inherited the environment.** `nono setup --check-only`
  and `nono profile validate` now share `output_with_deadline` with the version probe, and their
  environment has `NONO_*`, `DYLD_*` and `LD_*` removed, so a hung or steered probe cannot decide
  whether the sandbox is enforceable. Tests drive a hung command past a deadline and assert a
  `LD_PRELOAD`/`NONO_*` variable does not reach the fake binary.
- **`DYLD_*`/`LD_*` no longer reach the supervisor.** A constructor in `DYLD_INSERT_LIBRARIES` runs
  inside the unsandboxed `nono` before any profile is applied; verified live (the marker recorded
  `/opt/homebrew/bin/nono`). `Sandbox::scrub_env` now removes those prefixes with `NONO_*`, and the
  caller's child-env filter rejects them too.
- **A forged refusal no longer skips the shared-checkout guard in the repair path.** The repair
  path classified the refusal before verifying the guard, so a child could tamper with `.git` and
  then print `nono:` to skip the check. One `verify_guard_then_classify` runs the guard first; a
  test forges the refusal over a tampered `.git/config` and asserts the guard error wins.

### Changed

- **Loopback is no longer a profile grant.** `network.open_port: [0]` is gone from both profiles.
  A caller that needs an arbitrary-port loopback bind asks for it per run through
  `SandboxSpec::loopback_ports`, and the caller decides from the harness
  (`AgentKind::needs_wildcard_loopback`): measured, claude fails with `EPERM` without it and a
  `49152–65535` narrowing still failed, agy binds a local server, and codex uses its explicit
  TLS-relay port. A codex run therefore reaches no local service; an unmeasured harness keeps the
  grant rather than inheriting its absence silently.
- **Package caches are read-only.** `~/.cargo/registry` and `~/.cargo/git` moved from `allow` to
  `read`. Measured: a full offline rebuild from a warm cache wrote zero files under `~/.cargo`, so
  the write grant was not buying a working build — it was buying the ability to poison the caller's
  next unsandboxed build. A cold cache or a new dependency now fails inside the sandbox; pre-fetch
  it outside.

### Added

- `scripts/escape-drill.sh` — a host-only adversarial drill for filesystem grants, credentials,
  process/IPC, egress, the shared-checkout guard, plumbing, linked worktrees, and the review
  profile. It writes a JSON+TSV report outside the checkout, uses canaries only, and is
  deliberately not a gate. Its method and live results are in `docs/escape-testing.md`.
- A "Escape drill" README section and `docs/escape-testing.md` documenting the probe matrix,
  statuses, safety rules, results, and residuals.
- The shared-checkout guard now covers `.git/info` and hashes file **mode** bits, and it enforces
  the entry cap before reading an entry and refuses an entry it cannot observe. It **detects and
  refuses**; it does not repair the checkout.

- The escape drill's first live run (2026-10-28, Darwin 25.5.0, `nono` 0.80.0) is recorded in
  `docs/escape-testing.md`: 26 `PASS`/`DENIED`, 6 `FINDING`, 1 `SKIP`, no `FAIL`/`UNKNOWN` in the
  safe run; `--aggressive` adds one clipboard `FINDING` and a passing signal-delivery probe.
- `scripts/sandbox-why.sh` (`make sandbox-why`): 32 static assertions over the two profiles using
  `nono why` and `nono run --dry-run`. It needs no Seatbelt profile, so it runs inside another
  sandbox and on a host that cannot initialize one, and it asserts the *reason* for a denial, not
  only the verdict.
- `docs/`: a crate-local documentation index, the per-hypothesis verdicts (including the hypotheses
  that were refuted), and three research documents covering macOS Seatbelt, `nono` 0.80.0, and the
  crate's own attack surface with `file:line` evidence.
- `AGENTS.md`: self-contained crate instructions (no parent-repository references), covering the
  domain-agnostic boundary, the unconditional sandbox, offline tests, and the crate's gates.
- A "Why these grants" section in the README explaining `bypass_protection` for the keychain
  files, the explicit `~/.config/gh` and `~/.cargo/credentials.toml` denials, the codex/agy/grok
  state grants, the review profile's harness-state writes, and the grok auth-file decision.
- `examples/coding_agent.rs --review` runs the example under the read-only review profile; the
  README quickstart shows both modes.
- `docs/live-drill.md` records the real-harness results and the drill-driven fixes, dated and
  scoped to one host; the shipped repeatable adversarial evidence is `scripts/escape-drill.sh`.
- `examples/coding_agent.rs` gained `--worktree` (discovers the linked worktree and grants its Git
  metadata) and `--guard` (snapshot before, verify after, exit 3 on a trip).
- `SandboxSpec::writable_paths: Vec<&Path>` — caller-supplied write grants outside the checkout,
  emitted as `--allow <path>` after `--allow-cwd`. It is for a per-run harness home, not a general
  writable directory.
- `examples/coding_agent.rs --codex-home` creates a fresh writable `CODEX_HOME` under the checkout's
  `.workcell` directory, links the operator's `~/.codex/auth.json` into it, grants it through
  `writable_paths`, sets `CODEX_HOME`, and the drill uses it for codex matrix/review.
- Both profiles no longer grant any `$HOME/.codex` write entry (`allow` or `allow_file`): the shared
  home is read-only, and codex state is per attempt. Each codex child gets a fresh `CODEX_HOME` the
  caller creates and removes, with `auth.json` linked (not copied) from the operator's home and
  granted through `writable_paths`. Profile tests assert the absence of the static grant; a
  fake-`nono` test asserts the `CODEX_HOME` grant, and the review wrapper test asserts the only
  `--allow` is the per-run home.
- The `egress` drill mode: a non-allowlisted domain is refused, an allowlisted provider endpoint is
  reachable, and the review profile keeps the same reach. It runs as part of `all`; it skips its
  checks when `curl` is absent.
- The profile tests parse `network.allow_domain`/`network.open_port` and `assert_provider_egress`
  asserts `block: false`, the provider domains, and `open_port: [0]` for both profiles.
- `workcell/tests/git.rs` covers the in-process primitives, status, ancestry/merge-base, local
  fetch, rebase conflict/abort in a normal and linked worktree, reset, CAS update-ref, worktree
  add/list/remove/prune/branch delete plus linked-worktree directories, bounded diff, the
  local-config digest tracking includes, and the planted-hook/external-diff/textconv non-execution
  tests; `tests/gc.rs` uses the typed `list_worktrees`; the sandbox guard test asserts `v3:`.
- `docs/performance.md` records the wrapper overhead measured on one host, the source of the cost
  (the recursive checkout denies), and how to re-measure it. There is no committed benchmark
  script.

### Notes

- The profiles validate against real `nono` 0.80.0 (`nono profile validate --strict` on both). The
  agy paths were checked live with `nono why`; the grok paths were validated by profile validation
  and the profile test because `~/.grok` does not exist on the validation host.
- A live drill (macOS 26.5.1, `nono` 0.80.0) ran the agent profile against codex-cli 0.164.0,
  claude 2.1.310, and agy 1.4.7: all wrote the file and exited 0. claude and agy second turns
  passed; `codex exec resume` failed with `thread/resume failed: list_turns is not supported yet
  (code -32601)`, an upstream codex limitation rather than the sandbox. Grok and dsh were not
  installed and were not exercised. This is one-host evidence, not a CI gate; the ignored real-nono
  drill remains the opt-in gate. Re-run it after a codex upgrade to check the per-run home layout
  against the new codex (there are no static SQLite grants to re-check).
- The P5 policy experiments (2026-10-27, one host) confirmed which grants are load-bearing:
  removing the two keychain files plus `bypass_protection` makes claude fail with
  `Not logged in · Please run /login` (agy still passes), so they stay; `network.block = true` makes
  claude fail with `API Error: Connection refused (ECONNREFUSED)`, so a blanket block is not viable
  (superseded by the provider allowlist below); `~/Library/Preferences` was unnecessary and is
  removed. The committed profile
  re-ran matrix (codex 19 s, claude 10 s, agy 11 s) and review (codex 18 s, claude 7 s, agy 9 s)
  green.
- The P2 live run (macOS 26.5.1, nono 0.80.0, 2026-10-27) with the shrunk profiles passed the
  matrix (codex 10 s, claude 8 s, agy 10 s) and the review drill: read-only probes, per-run-home
  write, shared-home denial, and all three reviews (codex 8 s, claude 8 s, agy 10 s). Alternate
  surfaces also ran under the sandbox: `codex exec -C <dir> review --uncommitted` (`-C` before the
  `review` subcommand), claude `-p --output-format stream-json --verbose` (stream-json requires
  `--verbose`), and agy `--output-format stream-json` (NDJSON init/step events). grok remains
  unexercised (the Homebrew `grok` is jordansissel/grok, an unrelated log tool; `@xai/grok-cli` and
  `grok-cli` are 404 on npm), and dsh is not installed; building the DSH checkout is out of scope.
  One host, evidence not a gate.
- A per-run `CODEX_HOME` is state, not a credential copy: `auth.json` is a read-only symlink to the
  operator's real file, the home is removed with the task, and it must stay per-attempt because a
  reused home could carry a planted `config.toml`/hook into a later attempt. Codex resume remains
  upstream-broken (`list_turns is not supported yet`).
- The P3 benchmark (macOS 26.5.1, nono 0.80.0, 2026-10-27) measured the wrapper at 3 iterations:
  `nono --version` 25.7 ms, setup 222.2 ms, `nono run -- true` under the default profile 383.4 ms,
  under the agent profile 3260.8 ms (samples 2248.8–10085.2 ms), and 344.7 ms with the recursive
  `$WORKDIR/**` deny entries removed — a ~2.9 s median delta. An earlier ad-hoc 5-iteration run
  measured 1.26 s and was not saved, so the delta is a range that scales with checkout size. The
  recursive denies are correctness-critical; reduce the glob count or use a `nono` literal deny-list
  if latency matters. One host, evidence not a gate.
- A normal-exit reaping measurement: a TERM-respecting background child adds ~0.3 s to the run and
  is reaped; the KILL_GRACE path remains covered by the existing process test.
- The P2.9 `CODEX_HOME` experiment is now implemented in P2: the per-run home (with `auth.json`
  symlinked from the real home) ran codex 0.164 at 1,919 tokens versus 14,535 with the shared home,
  and replacing the ~30 static state grants with the run directory plus the auth read is the current
  policy. Caveats still apply: the auth symlink needs the read grant, the home must be per-attempt
  (a reused one could carry a planted config/hook into a later attempt), and codex resume is
  upstream-broken.
- The scripted P0 drill re-verified the shell, review, and matrix probes: config/credential denies
  held; the review profile read but could not write the checkout while writing harness state (codex's
  state became a per-run home in P2); codex
  9 s, claude 12 s, agy 11 s in the matrix and codex 19 s, claude 8 s, agy 9 s in review (all PASS).
  The two hardlink probes produced `LINK_DENIED` (the sandbox refused to create the link to the
  read-only-granted config), which is an observation on this host, not a cross-platform guarantee.
  Worktree-mode Git passed (commit succeeded; shared config and hooks denied), and the live guard
  tripped on `git config core.hooksPath` before the caller's next Git call. The refs probe is a
  recorded `FINDING`, not a failure or a pass: the agent can move `refs/heads/main`; today's
  mitigation is the integration branch-head check plus the one-run-per-checkout lock, with a
  follow-up to record the expected base SHA and refuse a base move the integration did not produce.

- The egress allowlist was validated live on one host (macOS 26.5.1, nono 0.80.0): `egress` 3/3 PASS,
  matrix codex 9 s / claude 7 s / agy 12 s, review codex 9 s / claude 6 s / agy 9 s. The experiment
  found that codex works under the proxy, agy needed `open_port: [0]` and
  `daily-cloudcode-pa.googleapis.com` (its eligibility check), claude needed `open_port: [0]`, a
  canary to `example.com` returns HTTP 000/403, and `api.anthropic.com` returns 401. A blanket
  `block: true` breaks claude (`ECONNREFUSED`), and an allowlist without localhost ports breaks
  claude (`EPERM`) and agy (bind `EPERM`).
- The allowlist is a curated hostname set, not a promise for MCP/plugin domains (not exercised);
  loopback stays open via `open_port: [0]`; the caller's unsandboxed verification commands keep
  network by design; and this is one-host evidence with provider/harness-version caveats. Extend
  both profiles and re-run `scripts/escape-drill.sh` when a provider changes.

## 0.1.0 — 2026-10-26

First release. The crate is publishable but has not been published.

### Added

- `git`: a typed Git runner hardened for untrusted repositories — no pager, no external diff or
  textconv driver, and `-c core.hooksPath=/dev/null` on every invocation — with the primitives a
  coding workspace needs (status, diff, refs, merge/rebase, worktree and branch management).
- `worktree`: `RepoSpec` and `Worktree`, one isolated checkout per unit of work cut from a fetched
  base, plus removal and pruning.
- `gc`: conservative collection of stale worktrees and branches, with a dry run and explicit
  protection for active work.
- `process`: one child process with a timeout, an optional stall watchdog, bounded capture, a
  full-stream log file, and a two-stage process-group kill (`SIGTERM`, grace period, `SIGKILL`).
- `sandbox`: an always-on OS sandbox around a coding agent — `nono` resolution and version floor,
  embedded agent and review profiles, preflight, deterministic argv wrapping, environment
  scrubbing, refusal classification, and the shared-checkout Git configuration guard.
- `instructions`: bounded, root-confined reads of a checkout's instruction files, with per-file and
  total caps and a report of what could not be read.
- `fs`: bounded reads, atomic writes, and a no-clobber publish.
- `Error` and `Result`: one error type for every module, carrying operator-facing detail and never
  a credential.

### Notes

- Runtime requirements are `git` on `PATH` and `nono` 0.80.0 or newer on `PATH` for the sandbox.
- The sandbox is not optional: there is no configuration switch, no fallback, and no API that runs
  an agent without it.

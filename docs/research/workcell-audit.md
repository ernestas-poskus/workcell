# `workcell` audit: the boundary the crate itself enforces

Scope: `workcell` at release. Method: read-only source review, `nono` 0.80.0 policy introspection,
and the crate's own tests. **No sandboxed child was executed for this audit** — nested Seatbelt
refuses to initialise inside the session ([`macos-seatbelt.md`](macos-seatbelt.md) §1). The LIVE
drill has since run from a plain host shell (2026-10-28, [`../escape-testing.md`](../escape-testing.md));
findings it measured are updated in [`../sandbox-hypotheses.md`](../sandbox-hypotheses.md), and the
ones it does not cover are still marked **needs live probe**. The findings below are the audit
snapshot; later fixes and narrowed grants are recorded in the hypothesis document, not rewritten
here.

Cross-links: the drill is [`../../scripts/escape-drill.sh`](../../scripts/escape-drill.sh); probe
rationale and the operator's safety rules are [`../escape-testing.md`](../escape-testing.md);
upstream behaviour is [`nono-0.80.0.md`](nono-0.80.0.md).

## Legend

| Class | Meaning |
|---|---|
| `escape` | code executes outside the profile **during** the run |
| `persistence` | data is planted so a **later unsandboxed** process executes it |
| `boundary-weakening` | the profile or launch is altered, or a grant is broader than its rationale |
| `data` | no boundary crossed; readable/observable data |
| `detection` | the guard or the failure classifier can be evaded |
| `dos` | availability only |
| `footgun` | the API lets a caller weaken the boundary |

Confidence: `CONFIRMED` = the code proves the mechanism; `PLAUSIBLE` = mechanism established, one
live semantic unverified; `REFUTED` = does not hold.

## Divergence from the earlier audit

An earlier audit pass read a pre-release revision of the crate. The guard has since gained
coverage of **file mode** and **`.git/info`**, and it refuses (it no longer restores) on a
mismatch (`src/sandbox.rs`). Two findings from that pass are therefore **REFUTED here** (F7's mode
and info gaps). Everything else below was re-checked against the current code.

## Findings

| ID | Severity | Class | Verdict | One line |
|---|---|---|---|---|
| F1 | high | persistence → RCE | CONFIRMED | writable Cargo caches feed the unsandboxed verifier |
| F2 | high | escape (confused deputy) | CONFIRMED (policy and live loopback reachability; external proxy still refused) | every loopback port is connect **and** listen |
| F3 | high | persistence → RCE | writable CONFIRMED, execution PLAUSIBLE | `~/.claude/shell-snapshots` is sourced by the unsandboxed Bash tool |
| F4 | high | escape (shared mode) | PLAUSIBLE, needs live probe | the caller's state denies are literal paths in a writable tree |
| F5 | med-high | boundary-weakening | CONFIRMED | profile path is re-read at launch, but only verified at preflight |
| F5b | med | boundary-weakening | PLAUSIBLE | a relative profile dir can resolve differently at launch |
| F6 | med | dos | CONFIRMED | unbounded `.git` read; entry cap enforced after reading |
| F7 | med | detection | CONFIRMED (tail, hooksPath) | 64 KiB tail and non-default hooks path are outside the digest |
| F8 | med | detection | CONFIRMED | the child can forge a sandbox refusal |
| F9 | med-low | footgun | CONFIRMED | two preflight probes inherit env and have no deadline |
| F10 | med | escape (pre-sandbox) | CONFIRMED (conditional) | `DYLD_*` in the caller's env map reaches the supervisor |
| F11 | med | data | CONFIRMED (scope) | every child reads every harness's credentials |
| F12 | med-low | boundary-weakening | CONFIRMED | a "read-only" review can overwrite the agy token/state |
| F13 | low-med | footgun | CONFIRMED | `Worktree`'s public fields let a caller widen the git grant |
| F14 | low | data | CONFIRMED | the deny is not a confidentiality boundary (`git show`) |
| F15 | low | docs | CONFIRMED | module docs claim shared-mode `.git` is unwritable |
| F16 | low | dos | CONFIRMED | the child log is unbounded |
| F17 | info | footgun | CONFIRMED | spec grants are not validated against the checkout |

### F1 — writable Cargo caches poison the unsandboxed verifier (high, persistence → RCE)

**Mechanism.** The agent profile grants read+write on the extracted registry and the git checkouts:
`src/sandbox/profiles/agent.profile.json:52-53`. Cargo verifies `.crate` tarball checksums when it
unpacks, but trusts the already-extracted trees (`registry/src/…`, `git/checkouts/…`). A sandboxed
agent can edit a dependency's `src/*.rs` or `build.rs` and exit with a clean diff. Verification then
runs **outside** the sandbox as the deployment user: a caller asserts exactly that in its own
tests, and its verification step relies on it. Those paths are outside this crate. The next
`cargo test` compiles and executes the poisoned source. The poison survives the run and affects the
operator's own builds and later runs.

**Why it holds.** This needs no Seatbelt bypass and leaves no repository diff — it is the
CVE-2026-26268 class the README invokes, with the cache as the carrier instead of a hook.

**Falsifiable test.** In an **isolated** `CARGO_HOME` copy, add a `build.rs` that writes a marker;
run the normal unsandboxed verify against that copy; observe the marker. Live: `esc_cargo_cache_write`
touches-and-restores an extracted file in the real registry (non-destructive) to confirm the grant.

### F2 — all loopback ports are connect **and** listen (high, escape / confused deputy)

**Mechanism.** Both profiles set `"open_port": [0]`
(`src/sandbox/profiles/agent.profile.json:21`, `review.profile.json:21`). `nono`'s
`--open-port` is "bidirectional localhost TCP: connect + listen", and port `0` is a macOS wildcard
that resolves to `ipc localhost:0` (static evaluation in [`nono-0.80.0.md`](nono-0.80.0.md)).
`SandboxSpec::loopback_ports` (`src/sandbox.rs:626-629`) is consequently redundant.

**Why it matters.** Any local forward proxy is a ready-made egress channel whose outbound connection
is outside `nono`'s proxy, so `allow_domain` can be bypassed without touching the kernel rule. Any
local admin/DB/MCP service is reachable. The child can also **bind** a port a later unsandboxed
process dials. The recorded rationale is "the compression proxy's port"; the grant is not
port-scoped.

**Falsifiable test.** Needs live probe: start a throwaway host proxy and
`curl -x http://127.0.0.1:$PORT https://example.com` in the child; `curl 127.0.0.1:3080` for the
supervising harness; bind a port in the child and connect from the host.

### F3 — `shell-snapshots` is executable state (high, persistence → RCE)

**Mechanism.** `$HOME/.claude/shell-snapshots` is read+write (`agent.profile.json:56`). The profile
and `docs/security.md` classify it as state, not configuration; the profile's own rationale says
configuration is read-only specifically so a planted hook cannot run "in the human's next session".
The installed Claude Code binary describes each Bash command running a file from that directory,
with a new snapshot taken at startup.

**Verdict.** Writable is CONFIRMED. Whether a later unsandboxed session **sources** a file the agent
wrote is PLAUSIBLE: the snapshot selection/rotation rule is not public.

**Falsifiable test.** Needs live probe: write a **new** snapshot file (never modify an existing
one), start an unsandboxed Claude in that `HOME`, run one Bash command, check the marker; then
repeat after a fresh start to see whether a new snapshot displaces it.

### F4 — the denied state paths are literals in a writable tree (high, shared mode only)

**Mechanism.** In shared-checkout mode `$WORKDIR` is read+write (`agent.profile.json:49-50`) and the
caller's control-plane denies are literals under it (`state.json`, `logs`, `runs/*.log`, `control`,
`sandbox`, `integration.lock`; `agent.profile.json:101-121`). The profile denies those paths, not the
parent directory and not by inode. A sandboxed agent can rename the parent, then read or edit the
files under the new name, and leave a symlink at the original path so the caller's path still
resolves. Those files hold claims/runs and pause/trip state, so tampering can release a spend breaker
or forge run state.

**Why it holds only in shared mode.** In worktree mode the state directory lives in the integration
checkout, outside `$WORKDIR`.

**Why the profile still names those paths.** A profile is static, so the default deny list is the
caller's conventional state location. It is a compatibility default: a deployment that moves its
state elsewhere must pass the new paths, and the crate has no typed way to accept them yet. That is
the design gap, and it is why the row stays open rather than being "documented".

**Falsifiable test.** Needs a live probe against a **synthetic** checkout whose state directory
carries a fake state document: rename the directory, read a byte through the new name, restore
afterwards. Never the live control plane.

### F5 / F5b — the published profile is verified at preflight, not at launch (med-high)

**Mechanism.** `publish_profiles` writes no-clobber and `verify_published_profiles` byte-compares
(`src/sandbox.rs:640-686`); `wrap` only formats the path (`:564-634`). Whoever can write the profile
directory can substitute a weaker profile **after** preflight and before `nono` opens it. `nono` is
given a path, not an fd, and the digest in the filename is not re-checked. Two concrete variants:

- `publish_profile` calls `tokio::fs::read(target)` with no regular-file check and no size cap
  (`:949`). A pre-created FIFO blocks preflight forever; a symlink to `/dev/zero` makes the read
  unbounded.
- The profile directory is created with `create_dir_all` and no owner/mode check (`:970`); files are
  `0600` but the directory is not tightened.

**F5b.** `profile_path` (`:925`) and `resolve` (`:186`) never absolutize the profile directory, while
the binary is absolutized (`:816`). `process.rs:180` spawns `nono` with `current_dir(spec.cwd)`, so a
**relative** profile dir resolves against the checkout at launch but against the engine cwd during
preflight — an agent-plantable profile for the next run. Worktree mode with an absolute state root
is not affected.

**Falsifiable test.** Offline with the fake `nono`: preflight a temp profile dir, overwrite the
agent profile with a valid JSON granting `/`, launch, assert the child's grants. FIFO variant:
`mkfifo <content-addressed-path>` before preflight, run under `timeout`, expect a bound not a hang.

### F6 — guard reads are not bounded (med, dos)

**Mechanism.** `read_dot_git_entry` uses `tokio::fs::read(path)` with no cap (`:1369-1397`). In
worktree mode `.git` is a regular file inside the writable checkout; an agent can append gigabytes
after the valid `gitdir:` line, and `guard_verify` runs **after** the child, so the unsandboxed
parent OOMs post-run. Separately, `read_surface_entries_sync` collects **every** hook/`info` entry
and only then compares against `MAX_HOOK_ENTRIES` (`:1454-1484`), so a hostile directory of 100k
64 KiB files allocates gigabytes before the "cap" fires.

**Falsifiable test.** Append 2 GiB to a worktree `.git` pointer; measure `guard_verify` RSS/time.
Create 100k files in `.git/hooks`; measure `guard_snapshot`.

### F7 — hook guard surface gaps (med, detection)

**Mechanism.** `HOOK_CONTENT_LIMIT` is 64 KiB (`:64`) and the digest frames `(length, first 64 KiB)`
(`read_bounded_file`, `:1569-1578`), so rewriting bytes past 64 KiB without changing length is
invisible. `guard_surfaces` hashes the default `common_dir/hooks` (`:1321-1323`) and does not resolve
`core.hooksPath`, so writes to a pre-existing non-default hooks path are outside the surface (the
config digest catches a *change* to the key, not writes to the directory it already names).

**Refuted in the current code.** File **mode** is hashed (`mode_of`) and **`.git/info`** is part
of the snapshot; both were real gaps in the earlier audited revision.

**Residual.** The guard is TOCTOU detection by construction: a write restored before verify is not
seen. `.git/info` is now covered, but `.git/objects/info/alternates` is not.

**Falsifiable test.** 64 KiB stable prefix + a tail that is rewritten; `guard_verify` must fail.
Pre-set `core.hooksPath=.githooks`, then rewrite `.githooks/pre-commit`; `guard_verify` must fail.

### F8 — `classify_refusal` is forgeable (med, detection)

**Mechanism.** `classify_refusal` returns true for empty stdout and a first non-empty stderr line
starting `nono:` (`:380-388`). The child controls both streams: `printf 'nono: refused\n' >&2;
exit 1` is classified as a sandbox refusal — non-retryable, escalated, not an agent failure. It
cannot hide a genuine refusal (the child never runs then) and cannot make a successful run fail. In
the repair path the classification branch is reported to return before the guard check, which would
let a forged refusal skip `guard_verify`; **that ordering needs confirmation against the current
code** (a caller's repair launch is the same shape; that code is outside this crate).

**Falsifiable test.** Offline: a fake child that prints the `nono:` shape must classify as
`Sandbox`. Live/repair: write a guard surface, then forge a refusal, and check whether guard-verify
events appear.

### F9 — preflight probes inherit env and have no deadline (med-low)

**Mechanism.** `probe_version` uses an empty environment and a deadline (`:692`, `probe_nono_version`),
but `probe_host_support` (`:699-725`) and `validate_profile` (`:740-768`) use neither `.env_clear()`
nor a timeout. A `NONO_*` / `HOME` / `XDG_*` value in the parent reaches the two commands that
decide "can this host enforce" and "is this profile valid", and a hung invocation blocks startup.

**Falsifiable test.** Run preflight with `NONO_LOG_FILE=/tmp/x` and see whether a file appears; make
a fake `nono` that sleeps and observe the missing deadline.

### F10 — the supervisor's environment is a pre-sandbox surface (med, conditional)

**Mechanism.** `scrub_env` removes only the `NONO_` prefix and is a helper the caller must invoke
(`:365-367`); `wrap` never touches the environment. `process.rs:177-182` does `env_clear()` then
`envs(&spec.env)`, so **only the caller's map** reaches `nono` — but that map is not filtered for
`DYLD_INSERT_LIBRARIES`, `DYLD_FRAMEWORK_PATH`, `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, or
`PATH`. A dylib constructor in the supervisor runs **before** Seatbelt initialises, and the
Homebrew `nono` is not hardened. `HOME`/`XDG_*` also change `$HOME` expansion and `nono`'s state
root, while `nono_state_location` reads `std::env` (`:1053`) — preflight can prove a different state
root than the launch uses.

**Scope.** The caller does call `Sandbox::scrub_env` at its launch sites, and its child-env filter
keeps `NONO_*` out; neither filters `DYLD_*`/`HOME`/`XDG_*`. A caller
that controls the launch env can also replace `PATH`; this finding is that the crate's stated scrub
does not defend the supervisor against non-`NONO_` vectors.

**Falsifiable test.** Put `DYLD_INSERT_LIBRARIES=/tmp/ctor.dylib` in the env map passed to the
supervised launch; the constructor writes a marker before any profile applies.

### F11 — cross-harness credential exposure (med, data)

**Mechanism.** `read` covers `~/.cargo`, `~/.rustup`, `~/.local/bin`, `~/.claude`, `~/.claude.json`,
`~/.codex`, `~/.gemini`, `~/.antigravity`, `~/.grok`, `~/.dsh`, and the two keychain DBs with
`bypass_protection` (`agent.profile.json:83-100`; review `:70-87`). Every child reads every
harness's state: a codex child can read Claude's credentials, a Claude child can read
`~/.codex/auth.json`, and all children can read `~/.dsh` (which holds a credentials file) and the
agy token (also writable, F12). Egress is limited to provider domains, and those are exactly the
endpoints the stolen provider tokens work against.

**Correction to the docs.** The keychain claim is narrower than stated. The two DBs are readable as
**files**, but `nono` does not grant the `security`/Mach services needed to extract items, and the
DBs are encrypted; "the whole login keychain is readable" is an overclaim until a live
`security find-generic-password` probe proves item access.

**Falsifiable test.** Needs live probe: `test -r` on each credential path (exit code only, never
content), plus `security find-generic-password -s <service>` with output discarded.

### F12 — a "read-only" review can overwrite agy state and token (med-low)

**Mechanism.** The review profile has `workdir.access: read` and no `$WORKDIR`/git grants, but it
still grants read+write on agy state directories and, via `allow_file`, read+write on
`~/.gemini/antigravity-cli/antigravity-oauth-token`, `state.json`, `history.jsonl`,
`conversation_summaries.db`, `jetski_state.pbtxt`, `last_check.timestamp`
(`review.profile.json:49-69`). `knowledge/`, `brain/`, and `implicit/` are likely
instruction/memory-bearing. A review cannot edit the checkout or harness configuration, but it can
persist into the operator's next agy session.

**Falsifiable test.** Needs live probe: write the token/state from a review child, then start an
unsandboxed agy session and observe the change (isolated `HOME` copy).

### F13 — `Worktree`'s public fields can widen the git grant (low-med, footgun)

**Mechanism.** `worktree_git_dir_grant` returns `git_common_dir/worktrees/<file_name(git_dir)>`
(`:1167`) rather than `git_dir` itself, and `Worktree` has public fields
(`src/worktree.rs:37-48`). A caller-built `Worktree { git_dir: common.join("worktrees/..") }` yields
`--allow <common>/worktrees/..` — the whole common `.git`, defeating the worktree-mode
"hooks/config not granted" claim. `Worktree::discover`/`adopt` are git-derived and canonical, so
this is an API footgun, not a live agent path.

**Falsifiable test.** Offline: construct a forged `Worktree`, assert the argv contains `--allow
<common>` and that a fake sandbox reports `.git/config` writable.

### F14 — the deny is not a confidentiality boundary (low, data)

**Mechanism.** `deny` names worktree paths (`agent.profile.json:101-112`), but worktree mode grants
`--read <common .git>` and `--allow <common>/objects` (`src/sandbox.rs:604-616`), and shared mode
makes `.git` writable. `git show HEAD:AGENTS.md` / `git cat-file blob` read the same content
regardless of the path deny. The deny's real purpose is stopping a harness from **auto-loading** a
project config, not keeping repository content secret — the caller already passes the configured
instruction files.

**Falsifiable test.** Needs live probe: `git show HEAD:AGENTS.md` in the child.

### F15 — module docs claim shared-mode `.git` is unwritable (low, docs)

`src/sandbox.rs:9-13` says the profile leaves `.git/hooks`, `.git/config`, `.git/info` "unwritable or
absent". In shared mode `.git` is inside the writable checkout and only the guard covers it (and the
guard is detection without repair). A caller reading the module docs could assume
containment where there is post-hoc detection.

### F16 — unbounded child log (low, dos)

`process.rs:169-175` opens the log append-only and `pump` (`:317`) writes every byte; only the
in-memory capture is capped (`CAPTURE_MAX_BYTES`, `:35`). An agent can fill the state volume; write
failures are swallowed. Availability only.

### F17 — spec grants are not validated (info, footgun)

`wrap` emits caller-supplied `writable_paths` / `read_paths` / `read_files` as `--allow` / `--read` /
`--read-file` with no check that they sit inside the checkout (`:597-629`; contract at `:146-162`).
`wrap_review` also applies `writable_paths` as `--allow`, so a review caller can grant write to a
read-only review. A caller derives these from its configuration and per-run harness home, not agent
input; this is a footgun for future callers.

## Refutations (evidence that the boundary held)

1. **Argument/flag injection — REFUTED.** Every spec path is emitted as its own flag's value
   (`:597-629`) and the child argv follows exactly one `--` (`:631`). `nono`'s clap rejects a
   flag-shaped value (`a value is required for '--allow <DIR>'`) and `trailing_var_arg` means
   nothing after the program name is parsed.
2. **Same-digest or pre-verify profile substitution — REFUTED.** `publish_profile` compares bytes
   before returning (`:949`) and `verify_published_profiles` compares full bytes (`:665-686`); a
   different profile at the path is refused. The digest's 64-hex-character truncation in the name
   does not help an attacker because the full bytes are compared. Only the post-verify swap (F5)
   works.
3. **`..` traversal / symlink escape in the crate's read helpers — REFUTED.** `fs::read_bounded`
   canonicalizes and refuses outside-root (`src/fs.rs:38-59`); `instructions::load` canonicalizes
   and root-checks (`src/instructions.rs:68-94`, `:147-151`).
4. **An unsandboxed fallback — REFUTED.** `preflight` fails closed on a missing/old/invalid binary,
   an unenforceable host, and an unusable state root (`:241`); `wrap` only builds argv; there is no
   direct-spawn branch, and tests assert it.
5. **`deny` losing to `allow` — REFUTED.** `nono`'s schema says denies apply regardless, and the
   explicit `~/.cargo/credentials.toml` / `~/.config/gh` denials hold (`nono why` → `filesystem_deny`).
6. **A FIFO/device blocking the hook guard — REFUTED.** `surface_entry` records non-regular entries
   as `Special` instead of reading them (`:1546-1551`).
7. **Mode bits unhashed / `.git/info` uncovered — REFUTED in the current code**;
   true of the earlier audited revision.

## Needs a live probe (do not treat as settled)

F2 (loopback reachability and rule order), F3 (snapshot sourcing), F4 (rename bypass in shared
mode), F5b (relative-path divergence under a real launch), F8 (repair-path ordering), F10 (`DYLD_*`
into the supervisor), F11 (whether keychain **items** or only the DB files are readable), F12
(review → agy persistence), F14 (`git show` under the deny). The drill's suites and probe names are
in [`../escape-testing.md`](../escape-testing.md).

## What would change these verdicts

- A `nono` release that scopes `open_port` or adds a per-launch profile check.
- Moving `[verify]` and acceptance probes inside a sandbox (kills F1's consequence).
- Per-run credential homes (kills most of F11/F12).
- An fd-bound profile handle instead of a path (kills F5/F5b).
- Canonicalized, validated `SandboxSpec` grants (kills F13/F17).

## Sources

Local, this crate:

- `src/sandbox.rs` (lines cited inline), `src/sandbox/profiles/agent.profile.json`,
  `src/sandbox/profiles/review.profile.json`, `src/process.rs`, `src/worktree.rs`, `src/fs.rs`,
  `src/instructions.rs`
- `tests/sandbox.rs` (the offline fake-`nono` suite), and a caller's own assertion that its
  verification commands are not wrapped (that source is outside this crate)

Upstream, accessed 2026-10-28:

- `nono` 0.80.0 schema, `--help`, `profile schema`, `profile groups`, `nono why`, `--dry-run`
  (details and citations in [`nono-0.80.0.md`](nono-0.80.0.md))
- macOS Seatbelt behaviour (details and citations in [`macos-seatbelt.md`](macos-seatbelt.md))

# Sandbox hypotheses and verdicts

One row per hypothesis the escape testing set out to settle. A verdict is only as strong as its
evidence: **REFUTED** means a command or the source contradicts it; **CONFIRMED** means the code or
a resolved capability proves the mechanism; **NEEDS LIVE** means the mechanism is established but
the end-to-end effect needs a child process on a host that can initialize a Seatbelt profile.

Nothing here is a claim about a future `nono` or macOS. Scope: `nono` 0.80.0, macOS 26.5.1, arm64.
Detail and `file:line` for each finding is in [`research/workcell-audit.md`](research/workcell-audit.md).

## Refuted — the boundary held

| # | Hypothesis | Evidence that refutes it |
|---|---|---|
| H1 | Egress is proxy-advisory: a direct socket escapes the domain allowlist | `allow_domain` activates proxy mode. `nono run --dry-run` resolves `net proxy`, i.e. `(deny network*)` plus a localhost proxy port. `example.com:443`, `1.1.1.1:443` are `DENIED proxy_filtered`. Asserted by `make sandbox-why`. |
| H2 | A grant path that looks like a flag injects a `nono` flag | clap rejects it: `--allow --profile=x` → `a value is required for '--allow <DIR>'`. `run`/`wrap` are `trailing_var_arg`, so nothing after the program name is parsed. A `-`-shaped path fails closed. |
| H3 | A case or Unicode variant of a denied path walks past the deny | `nono why` resolves the vnode: `~/.cargo/CREDENTIALS.toml` and `Credentials.toml` both report `DENIED filesystem_deny` on the same rule. |
| H5 | `deny` loses to a broader `allow` | The schema states denies apply regardless of other allow rules, and `nono why` agrees: a deny inside an allowed tree still denies. |
| — | A weaker profile can be substituted at the same content-addressed name | `publish_profile` refuses different bytes and preflight compares the full bytes; a 64-bit name collision still fails the compare. Substitution *after* preflight is a different hypothesis and is listed below. |
| — | `..` traversal or a symlink escapes the crate's own bounded reads | `fs::read_bounded` and `instructions` canonicalize and root-check. |
| — | Any error path runs the child unsandboxed | `resolve`/`preflight` fail closed; `wrap` only builds argv; there is no direct-spawn branch. |
| — | A FIFO or device at a guard surface blocks the guard | Surface entries are classified before reading; a special entry becomes a marker, not a read. The *profile* path is a separate case, fixed in the current code. |
| — | The guard misses a hook's executable bit or a `.git/info` write | The current code hashes mode bits and covers `info`; both were gaps in the earlier audit. |

## Confirmed — mechanism proven, no live child needed

| # | Finding | Class |
|---|---|---|
| F1 | `~/.cargo/registry` and `~/.cargo/git` **were** read+write, so a run could edit an extracted dependency and the unsandboxed verify step would compile it. **Closed 2026-10-28**: both are read-only, measured against a full offline rebuild that wrote zero files. A cold cache now fails visibly rather than silently poisoning the caller. | persistence (fixed) |
| F2 | `open_port: [0]` **was** in both profiles: bidirectional loopback on every port. **Narrowed twice on 2026-10-28**: the profiles grant no loopback, and the caller asks per harness — `codex` none, **`agy` listen-only** (`--listen-port 0`: a bind, not a connect), `claude` the connect wildcard (`--open-port 0`, because listen-only failed with `Unable to connect to API (EPERM)`), and the unmeasured kinds keep the wider grant. The loopback **egress** path now survives only for claude and the unmeasured kinds. | escape (narrowed) |
| F3 | `~/.claude/shell-snapshots` is read+write, and its files are shell code the unsandboxed Bash tool runs. **Confirmed by the CLI's own text**: "Each Bash command first runs a file from there, without asking anyone" (`claude` 2.1.312), read by a `Read(~/.claude/shell-snapshots/**)` rule in the same policy list. This is a persistence channel of exactly the class the profile claims to block. The live-session attempt was blocked (isolated HOME cannot authenticate; the real account hit its weekly limit) but the mechanism is stated by the tool. | persistence |
| F4 | The profile's state-path denies are **literals in a writable tree**, so a rename of the parent defeats them. **Confirmed live 2026-10-28**: a sandboxed child renamed the state directory, read the state through the new name, wrote the control file, and left a symlink at the original path. A path deny cannot follow an inode, so the only sound fixes are worktree mode or state outside the writable checkout. A `state_dir_rename` probe now records it every run.. `state_dir_rename` records it every run, and `wirepuller doctor` now **warns** when the state directory sits inside a shared checkout. | boundary weakening (residual, diagnosed) |
| F5b | A relative profile directory resolved against the caller's cwd at preflight and the checkout's cwd at launch. **Fixed in the current code**: `resolve` absolutizes it, with a test. | boundary weakening (fixed) |
| F6 | The `.git` pointer read was unbounded. **Fixed** with a byte bound. The hook/info entry cap is enforced **before** each entry is read, an unreadable entry is refused, and an entry **past** the content cap is refused rather than prefix-hashed, so a rewrite past the cap can no longer hide. | detection gap (fixed) |
| F7 | The hook digest covered only the first 64 KiB and left a configured `core.hooksPath` outside the surface. **Fixed**: an oversized entry is refused, and the guard resolves `core.hooksPath` and hashes the directory Git would execute. Tests pin both, plus a mode-only change. | detection gap (fixed) |
| F8 | `classify_refusal` is forgeable by the child (`nono:` on stderr, empty stdout). In the repair path the classification returned before the guard verify, so a forged refusal skipped a tamper check. **Fixed in the current code**: one `verify_guard_then_classify` runs the guard first, and a test tampers `.git/config` while forging `nono:` and asserts the guard error wins. | detection gap (fixed) |
| F9 | Preflight's host probe and profile validation inherited the parent environment and had no deadline. **Fixed in the current code**: all probes share one `output_with_deadline`, and the host/validate probes remove `NONO_*`/`DYLD_*`/`LD_*`; a unit test drives `/bin/sleep` past a 50 ms deadline, and an integration test sets `NONO_ESCAPE_PROBE` and `LD_PRELOAD` and asserts neither reaches the fake `nono`. | hardening gap (fixed) |
| F10 | `DYLD_INSERT_LIBRARIES` reached the unsandboxed `nono` supervisor before any profile was applied — **proven live**: a constructor's marker recorded the executable as `/opt/homebrew/bin/nono`. **Fixed in the current code**: `Sandbox::scrub_env` and the caller's child-env filter both drop `DYLD_*` and `LD_*`, with tests on each. | escape (pre-sandbox code execution) (fixed) |
| F11 | Every child reads **every** harness's configuration and credentials (`~/.claude`, `~/.claude.json`, `~/.codex`, `~/.gemini`, `~/.dsh`, keychain databases). **Measured 2026-10-28**: a sandboxed child read `~/.codex/auth.json`, `~/.dsh/.credentials.yaml`, and `~/.claude.json`; `~/.claude/.credentials.json` does not exist on this host (Claude keeps its credential in the keychain). | data exposure |
| F12 | The "read-only" review profile can overwrite the operator's agy token/state and write `knowledge`/`brain`. **Confirmed live 2026-10-28**: a review-profile child wrote and read a file under `~/.gemini/antigravity-cli/knowledge` (`REVIEW_WROTE`). | persistence |
| F13 | `Worktree` is a public struct; a caller-built `git_dir` of `worktrees/..` widened the derived write grant to the whole common Git directory. **Fixed in the current code**: the grant must be `<common>/worktrees/<name>`, with tests for both forgeries. | footgun (fixed) |
| F14 | The instruction-file deny is a path deny, not a confidentiality boundary: `git show HEAD:AGENTS.md` reads the same bytes from the object store, which the worktree mode grants. The deny still prevents a harness auto-loading a project config, which is its real purpose. | documentation truth |
| F15 | Module docs claimed the profile makes `.git/hooks`/`config` "unwritable" in shared-checkout mode; they are writable there and only guard-detected. **Fixed in the current code** in the docs. | documentation truth (fixed) |
| F16 | The child's log was append-only and unbounded. **Fixed**: the pumps stop at `MAX_LOG_BYTES` (16 MiB) across stdout and stderr and append one truncation marker; a unit test writes 17 MiB and asserts the cap and the marker. Measured before the fix: 52,428,800 bytes in produced a 52,429,394-byte log. | denial of service (fixed) |
| — | A restore-on-mismatch mode would repair a tampered shared checkout | Removed before merge: a review found a manifest path traversal that could delete the common Git directory, an unsound "outside the checkout" backup invariant (the profiles grant writes outside the checkout), and that wholesale restore destroys concurrent state. Detection plus refusal remains; an in-memory backup is the sound variant. | design (rejected) |
| F17 | `SandboxSpec` grants were not validated. **Partly fixed**: `wrap_agent`/`wrap_review` refuse a relative checkout or grant, because `nono` resolves relative paths against the child's cwd (the run's checkout, not the caller's) — a silent widening in the wrong place. Whether a grant is *allowed* to sit outside the checkout is still the caller's contract. | footgun (partly fixed) |

## Live run — 2026-10-28, after the narrowing (one host)

Darwin 25.5.0, Apple Silicon, `nono` 0.80.0. Safe run: **26 `PASS`/`DENIED`, 4 `FINDING`, 4 `SKIP`,
0 `FAIL`, 0 `UNKNOWN`**; `--aggressive`: **27/5/3/0/0**. Per-probe detail is in
[`escape-testing.md`](escape-testing.md).

| Probe | Live outcome |
|---|---|
| `cargo_cache_write` | **`PASS`**: the child can no longer write `~/.cargo/registry`. F1's chain is closed for the warm-cache path. |
| `loopback_denied` / `loopback_granted` | **`PASS` / `PASS`**: the profile alone reaches no local service; the caller's `--loopback 0` reaches the same canary. F2 is narrowed and measured in both directions. |
| `hardlink_granted_config` | `PASS`: a hardlink did not grant the read-only-granted config. H4 is refuted live. |
| `direct_egress`, `direct_ip`, `proxy_bypass`, `udp_denied` | `PASS`: H1 is refuted by a child process, not only by reading the launch resolution. |
| `harness_snapshot_write` | **`FINDING`**: the child can still write a harness shell-snapshot. F3 is writabiity-confirmed; execution by a later unsandboxed session is still unproven. |
| `keychain_read`, `harness_credentials` | **`FINDING`**: the database files and granted harness auth files are readable. Keychain **item** extraction is still unproven (no `security` Mach grant). |
| `worktree_refs` | **`FINDING`**: refs remain writable by design; the caller's head/base checks are the mitigation. |
| `guard_tamper` | `PASS`: the child's `.git` writes were detected and the run refused (exit 3). Detection, not repair — the planted surface remains, so a shared checkout must be discarded after a refusal. |
| `profile_tamper` | **`SKIP`**, honestly: the child cannot discover the published profile (no supervisor argv, directory outside every grant), so the probe proved nothing and says so. Not being able to find the policy is a stronger property than failing to write it, but it is not this probe's assertion. |
| `clipboard_read` | **`FINDING`** with `--aggressive`; the profile has no grant primitive for the pasteboard under 0.80.0. |
| `appleevents`, `docker_socket` | `SKIP`: TCC consent, or `--aggressive` plus an installed socket. |

### Harness matrix (same host)

The narrowing's one behavioural change is codex, which no longer receives loopback:

| Harness and grants | Result |
|---|---|
| **codex**, no loopback, per-run `CODEX_HOME` | **exit 0** (28 s) |
| agy, `--loopback 0` | exit 0 (7 s) |
| claude, `--loopback 0` | exit 0 (5 s) |
| claude, **no** loopback (control) | **hung; killed at 180 s** (exit 143) |

The control did not reproduce P5's fast `EPERM`: without the grant claude stalled rather than
refusing. The grant is load-bearing either way, and the failure mode costs a deadline.

### Still unproven

`DYLD_INSERT_LIBRARIES` into the supervisor (F10), a forged refusal in the repair path (F8),
preflight's inherited environment and missing deadline (F9), a synthetic rename of a denied state
directory (F4), keychain **item** extraction, a planted snapshot being **sourced**, and the
cache-poisoning **chain** executing in an unsandboxed verify build. Each is a code-level finding with
a named test, not a measured result.

A refuted row is the useful outcome. When a live probe contradicts a verdict here, amend the row and
the audit document in the same change; do not quietly demote it to a pass.
| F18 | The two-stage kill reaches the child's process group, but a grandchild that calls `setsid` creates a new session and escapes it. **Confirmed live 2026-10-28**: the daemon wrote its marker 8 s after a run that had already exited. Mitigation: the daemon is still inside the sandbox, so its authority is the run's grants; it can, however, mutate the checkout after the guard read and keep a loopback listener alive. | process (residual) |

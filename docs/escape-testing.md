# Escape testing the workcell sandbox

Status: host-only evidence, not a gate. The drill is
[`scripts/escape-drill.sh`](../scripts/escape-drill.sh); the crate's own tests remain the
executable contract.

This document records what the drill probes, how to run it, what it observed on one host, and
which residuals are accepted. It deliberately uses only workcell concepts — a checkout, a linked worktree, a
profile, the shared-checkout guard, a child process — and names no caller, tracker, task
description, or integration repository.

## What this tests

The sandbox is a **capability** boundary: it limits which paths, network destinations, and process
and IPC operations a child and its descendants may use. It is not a virtual machine or a separate
kernel. The escape drill tests the boundary the crate claims to enforce:

- filesystem grants and denials (including symlink and hardlink aliasing, and shared temp/device paths);
- credentials — SSH material, package-manager credentials, the keychain, and harness auth files
  that the profiles explicitly grant;
- process and macOS IPC — signalling, inspection, launchd enumeration, pasteboard, AppleEvents,
  Mach/XPC;
- egress — direct connections outside the filtering proxy, DNS/UDP, proxy bypass, loopback
  services, Unix sockets;
- the shared-checkout guard — child tampering with `.git/config`, `.git/hooks`, and `.git/info`,
  detection and refusal, and whether a later host Git runs a planted hook;
- plumbing — environment scrubbing, profile/state tampering, surviving background children,
  review-profile parity.

Anything the policy explicitly grants, and anything outside the crate's contract (a caller that
executes child-written files after the child exits, the kernel, `nono` itself), is recorded as a
`FINDING`, not silently treated as a pass.

## Safety rules

- Temporary repositories and canary files only; the drill never reads or prints a credential value.
- No external-service requests: the direct-egress probe connects to `example.com` only to see
  whether the connection is refused.
- No destructive host operations, no persistence, no TCC-setting changes, no kernel or `nono`
  binary attacks.
- `--aggressive` adds clipboard read status, signal delivery to a parent-started helper, `ptrace`,
  and a container-socket handshake. They are still canary-only.
- A cleanup trap kills helpers/servers and removes canaries; reports go outside the checkout.

## Running it

```sh
cd workcell
cargo build --example coding_agent
scripts/escape-drill.sh --suite all --mode both
scripts/escape-drill.sh --suite all --mode both --aggressive
```

Prerequisites: macOS, `nono` 0.80.0 or newer (`nono setup --check-only`), `git`, `jq`,
`python3`, `nc`, and the built example. The host must not already be inside another Seatbelt
sandbox. Exit codes: `0` every probe classified without `FAIL`/`UNKNOWN`; `1` a `FAIL` or
`UNKNOWN`; `2` a prerequisite is missing.

Flags:

| Flag | Meaning |
|---|---|
| `--suite all\|filesystem\|credentials\|process\|network\|guard\|plumbing` | probe group |
| `--mode plain\|worktree\|review\|both` | checkout profile: writable checkout, linked worktree, read-only review, or plain + worktree |
| `--aggressive` | clipboard, signal, `ptrace`, container-socket probes |
| `--agent-cmd '<command>'` | optional caller-supplied command run inside the sandbox for manual red-team review |
| `--out DIR` | report directory (default `$TMPDIR/workcell-escape-drill-<ts>`) |
| `--bin PATH` | example binary |

The report is `results.tsv` and `report.json` with one row per probe:
`id,suite,mode,status,severity,expected,observed,detail,seconds`. Statuses:

- `PASS` — the expected control held.
- `DENIED` — the expected denial was observed.
- `FINDING` — an expected-but-unwanted residual (or a granted read) was observed; not a pass.
- `FAIL` — unexpected authority.
- `SKIP` — prerequisite absent or the probe is deliberately not run.
- `UNKNOWN` — the probe could not run or produced no marker; never counted as a pass.

## Probe matrix

| Suite | Probes |
|---|---|
| filesystem | SSH key read, outside-canary read/write, cargo/gh credentials, symlink out, hardlink to a read-only-granted file, keychain read, cargo-cache write, harness shell-snapshot write |
| credentials | SSH config, SSH agent socket, parent-process environment via `ps`, clipboard (`--aggressive`), granted harness auth, cross-harness credential read |
| process | signal helper, `sample`/`ptrace`, launchd enumeration, AppleEvents (`SKIP`: TCC), aggressive signal delivery, `setsid` daemon survival |
| network | direct egress, direct IP, allowlisted proxy reach, proxy bypass, loopback denied by the profile and granted by the caller, Docker socket (`--aggressive`), direct UDP/DNS |
| guard | child writes `.git/hooks/post-commit`, appends `.git/config`, writes `.git/info/attributes`; guard detection; host `git commit` must not run the hook |
| plumbing | child environment scrub, checkout readability, published-profile write |
| worktree | linked-worktree commit, ref move denial (PASS), shared Git config write |
| review | checkout read/write, keychain read, loopback reach, direct egress |

## Results

Two kinds of evidence, kept apart because they prove different things.

### Static: the profile's decisions, asserted (`scripts/sandbox-why.sh`)

`nono why` and `nono run --dry-run` evaluate a profile **without applying a Seatbelt profile**, so
this half runs on a host that cannot initialize a sandbox — including inside another sandbox. The
script renders the state-directory token and asserts 32 decisions and their *reason*, because "denied because nothing grants it" and
"denied because a deny rule covers it" are different guarantees: only the second survives a broader
grant.

| Assertion | Observed (macOS 26.5.1, `nono` 0.80.0) |
|---|---|
| `~/.cargo/credentials.toml` read, and its `CREDENTIALS.toml` case variant | `DENIED` `filesystem_deny` |
| `~/.config/gh` read; `~/Library/LaunchAgents/*.plist` write | `DENIED` `filesystem_deny` / `path_not_granted` |
| `$WORKDIR/**/AGENTS.md` read | `DENIED` `filesystem_deny` |
| `~/.gitconfig`, `~/.claude/settings.json` write | `DENIED` `insufficient_access` |
| `~/.cargo/registry/.../build.rs`, `~/.cargo/git/checkouts/.../build.rs` | read `ALLOWED`; write `DENIED` `insufficient_access` |
| `~/.claude/shell-snapshots/*.sh`, `~/.claude/projects/*` | `ALLOWED` read+write |
| `~/.dsh/.credentials.yaml` | `ALLOWED` read |
| `~/Library/Keychains/login.keychain-db` | `ALLOWED` read |
| `~/.gemini/antigravity-cli/antigravity-oauth-token` | `ALLOWED` read+write |
| resolved capability summary, agent and review | `net proxy`; `ipc localhost:0` |
| `example.com:443`, `1.1.1.1:443`, `127.0.0.1:3080` | `DENIED` `proxy_filtered` |
| `api.anthropic.com:443` | `ALLOWED` `proxy_allowed` |
| review checkout read / write | `ALLOWED` read / `DENIED` `insufficient_access` |

Reproduce with `make sandbox-why`; the report TSV lands under `target/evidence/sandbox-why`
(`scripts/sandbox-why.sh` alone defaults to `$TMPDIR/workcell-sandbox-why-*`).

What this establishes: the two profiles resolve to the intended shape; egress is proxy mode, not
"network allowed"; the profiles grant no loopback (a caller asks per run); the deliberate grants above are real.

What it does **not** establish: that Seatbelt enforces the compiled profile, the rule ordering
between a deny and a later allow, or any probe that needs a child process. Those are tested by the
live run below.

### Live: one host, 2026-10-28 (after the grant narrowing)

Darwin 25.5.0, Apple Silicon, `nono` 0.80.0. Run from a plain host shell, because the drill cannot
initialize a nested Seatbelt profile:

```sh
cd workcell && cargo build --example coding_agent
scripts/escape-drill.sh --suite all --mode both --out /tmp/escape-safe-20261028b
scripts/escape-drill.sh --suite all --mode both --aggressive --out /tmp/escape-aggr-20261028b
```

| Run | Classification |
|---|---|
| safe | **27 `PASS`/`DENIED`, 3 `FINDING`, 4 `SKIP`, 0 `FAIL`, 0 `UNKNOWN`**, exit 0 |
| `--aggressive` | **28 `PASS`/`DENIED`, 4 `FINDING`, 3 `SKIP`, 0 `FAIL`, 0 `UNKNOWN`**, exit 0 |

**What the narrowing changed, measured.** `cargo_cache_write` moved from `FINDING` to **`PASS`**: the
child can no longer write `~/.cargo/registry`. The old `loopback_service` row split into
`loopback_denied` **`PASS`** (the profile alone reaches no local service) and `loopback_granted`
**`PASS`** (the same service is reachable once the caller grants `--loopback 0`). `worktree_refs`
moved from `FINDING` to **`PASS`/`DENIED`**: the sandbox now grants only the run's ref directory,
so an attempt to move `refs/heads/main` is denied. Both halves are now
proven rather than implied.

A later run with the state-rename and keychain probes and `ESCAPE_KEYCHAIN_SERVICE` set classified
**27 `PASS`/`DENIED`, 5 `FINDING`, 4 `SKIP`, 0 `FAIL`, 0 `UNKNOWN`**. The newest run adds
`setsid_daemon` and `cross_harness_read` and classifies **27 / 6 / 5 / 0 / 0**; both new probes
report `FINDING`, which is the recorded residual: `state_dir_rename` and
`keychain_item` both reported `FINDING`, which is the recorded residual, not a regression. Without
the service variable `keychain_item` reports `SKIP`; the pre-probe numbers above are kept so the
earlier run stays comparable.

**Remaining findings, all documented residuals:** `harness_snapshot_write` (Claude sources shell
snapshots; the write grant remains), `keychain_read`, `harness_credentials`, plus
`clipboard_read` under `--aggressive` (the pasteboard is readable under 0.80.0 and the profile has no
grant primitive for it). `worktree_refs` is now an expected denial (`PASS`/`DENIED`).

**`profile_tamper` is `SKIP`, and that is the honest result.** The child cannot discover the published
profile: process enumeration does not yield the supervisor's argv, and the profile directory is
outside every grant, so `find` cannot see it. Not being able to find the policy is a stronger
property than failing to write it, but it is not the probe's assertion, so it reports `SKIP` rather
than a pass.

### Harness matrix (same host, same day)

The narrowing's one behavioural change is codex, which no longer receives loopback. Each run used the
example harness, a throwaway repository, and a one-line prompt:

| Harness and grants | Result |
|---|---|
| **codex**, no loopback, per-run `CODEX_HOME` | **exit 0** (28 s) |
| agy, `--loopback 0` | exit 0 (7 s) |
| claude, `--loopback 0` | exit 0 (5 s) |
| claude, **no** loopback (control) | **hung; killed at 180 s** (exit 143) |

Codex working without loopback is what the caller table predicts. The control is the interesting
row: without the grant claude did **not** fail fast with the `EPERM` the P5 experiment recorded — it
hung until killed. That still makes the grant load-bearing, but the failure mode is a stalled
dispatch rather than an immediate refusal, which is worse for an unattended run. Do not send claude
into a profile without the grant to "see what happens"; it costs the deadline.

### Additional proofs (manual, same host)

Four questions the drill does not answer, settled directly:

| Question | Method | Result |
|---|---|---|
| Does `DYLD_INSERT_LIBRARIES` reach the **supervisor**? | a constructor that records `_NSGetExecutablePath`, injected into `nono run` | marker contained `/opt/homebrew/bin/nono` — pre-sandbox code execution, before any profile |
| Is a forged refusal able to skip the guard? | a caller's test: tamper `.git/config`, then forge `nono:` and exit 1 | the guard error wins; the same forged output without tampering is classified as a refusal |
| Does the preflight probe honour a deadline and scrub the environment? | unit test: `/bin/sleep` past a 50 ms deadline; integration test: `NONO_ESCAPE_PROBE`/`LD_PRELOAD` | timeout refused; neither variable reached the probe |
| Does a rename defeat the state-path denies? | synthetic checkout: `mv .workcell renamed`, read/write through the new name | `MOVED`, `READ`, `WROTE`, `RELINKED` — confirmed |
| Is a keychain **item secret** retrievable? | `security find-generic-password -s "<service>" -w >/dev/null` inside the sandbox | exit 0 — retrievable; the value was discarded |
| Does **listen-only** loopback suffice? | repeated the matrix with `--listen-port 0` instead of `--open-port 0` | agy **completed** (listen is enough); claude **failed** after 186 s with `Unable to connect to API (EPERM)` |
| Does a `setsid` grandchild escape the group kill? | a `fork`+`setsid` child that sleeps 8 s, then writes a marker | **survived**: the marker appeared after the run had exited |
| Does one child read other harnesses' credentials? | `test -r` on `~/.codex/auth.json`, `~/.dsh/.credentials.yaml`, `~/.claude.json` | all three **readable**; `~/.claude/.credentials.json` does not exist here |
| Does a missing dependency fail loudly on the read-only cache? | `cargo check --offline` on a crate requiring a nonexistent version | clear resolution error in 3 s, no hang |
| Does the "read-only" review profile write harness state? | review-profile child writes under `~/.gemini/antigravity-cli/knowledge` | **`REVIEW_WROTE`**; the marker was removed |
| Is the child log bounded? | 52,428,800 bytes of stdout | log grew to **52,429,394 bytes** — unbounded |
| Does a planted shell snapshot actually run? | the CLI's own text for 2.1.312, plus a live attempt | the CLI states "**Each Bash command first runs a file from there, without asking anyone**"; the live session was blocked (isolated HOME cannot authenticate, the real account hit its weekly limit) |

The `setsid`, cross-harness, review-write and log rows are residuals; listen-only became a real
narrowing (agy), and the guard/re-verify fixes are covered in `sandbox-hypotheses.md`. The earlier
residual rows are recorded as residuals, not fixed: a path deny cannot follow an inode, and the
keychain grant is what makes claude authenticate (P5). The drill gained `state_dir_rename` and
`keychain_item` (set `ESCAPE_KEYCHAIN_SERVICE`) so both stay observable.

### Probe expectations after the harness-neutral profiles

The drill's `harness_credentials`, `cross_harness_read`, and `keychain_item` probes exercised profile
grants that no longer exist: the example grants no harness path, so all three now report `PASS`
(denied). To probe with a caller grant, pass the example's `--read-path`/`--read-file`/
`--bypass-protection` flags; the auth-subcommand experiments do exactly that and are recorded in
`sandbox-hypotheses.md` F11.

## Interpretation and residuals

- A `FINDING` is evidence of a documented boundary, not an escape. The README's "Exposed and not
  contained" section is the current list; the drill must agree with it.
- A `FAIL` is fixed in the crate, narrowed in the profile, or recorded with an owner and rationale
  here. A `FAIL` caused by `nono` is reported privately under its security policy; only the
  defensible residual is recorded.
- Re-run the drill after a macOS, `nono`, or profile change, and when a caller fixes a post-run
  trust handoff. The host and version belong in the results table; one host is evidence, not a
  guarantee.

## Why the guard detects and does not repair

The shared-checkout guard detects a change and refuses the run, but leaves the tampered checkout in
place. A host `git` later could still execute a planted hook or configuration, which is why a
restore-on-mismatch mode was built and then **removed before merge**.

An independent review found the restore unsound in three ways, and none of them is a tuning problem:

- **Path traversal in the restore.** The on-disk manifest's entry names were trusted and joined to
  the surface directory, so an entry named `..` made `remove_dir_all(".git/info/..")` delete the
  common Git directory — including, in worktree mode, the integration repository's `.git`, which the
  child cannot otherwise write.
- **The backup location invariant was wrong.** The design said "outside the sandbox-writable
  checkout", but the profiles grant writes well outside the checkout (`~/.cache`,
  `~/.cargo/{registry,git}`, `~/.claude/projects`). A child able to write the backup could choose the
  bytes restored into `.git/config`, `.git/hooks`, and `.git/info`, and the write landed before the
  post-restore re-check.
- **Restore is destructive in a concurrent shared checkout.** It deleted every entry not in the
  manifest and rewrote `.git/config` wholesale, with no way to attribute a change to *this* child;
  shared-checkout mode is concurrent by design.

A restore is only defensible from a source the child cannot write — an in-memory backup held by the
parent, or a snapshot the kernel protects. The cheaper and stronger fix is the one the crate already
offers: **worktree mode**, where `.git` is outside the writable checkout and there is nothing to
restore. The guard's job is now exactly detection plus refusal, and the drill's guard probe asserts
that a host commit after the refused run executes nothing.

## Sources

- <https://github.com/nolabs-ai/nono>
- <https://nono.sh/docs/cli/internals/security-model>
- <https://nono.sh/docs/cli/internals/seatbelt>
- <https://github.com/nolabs-ai/nono/blob/main/SECURITY.md>
- <https://github.com/nolabs-ai/nono/security/advisories>
- <https://labs.cloudsecurityalliance.org/research/csa-research-note-ai-coding-agent-sandbox-escapes-20260722-c/>
- <https://thenextweb.com/news/ai-coding-agents-sandbox-escapes-pillar>
- <https://powerofcommunity.net/assets/v0/poc2024/Mickey%20Jin,%20A%20New%20Era%20of%20macOS%20Sandbox%20Escapes%20-%20Diving%20into%20an%20Overlooked%20Attack%20Surface%20and%20Uncovering%2010+%20New%20Vulnerabilities.pdf>

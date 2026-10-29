# Live drill results (2026-10-27 / 2026-10-28)

Evidence, not a gate. The runs below executed real coding-agent CLIs and adversarial shell probes
through the rendered agent and review profiles on one host:

- macOS 26.5.1 (Darwin 25.5.0), Apple Silicon
- `nono` 0.80.0
- harnesses installed: codex 0.164, claude, agy. **grok and dsh were not installed** and are not
  exercised here.

The repeatable adversarial evidence is [`scripts/escape-drill.sh`](../scripts/escape-drill.sh); its
probe matrix, safety rules, and current results are in [`escape-testing.md`](escape-testing.md). The
harness matrix, egress, and policy-variant runs below need the commercial CLIs installed, so they
were run during development and are recorded here rather than claimed as a shipped script. Re-run
the relevant probe after any macOS, `nono`, or harness upgrade: a new version can invalidate a grant.

## Agent matrix (PASS)

With a per-run `CODEX_HOME`, codex completed in 10 s, claude in 8 s, and agy in 10 s: all exited 0
and wrote `hello.txt`. Earlier runs recorded codex ~11 s / ~12.3k tokens, claude 8–19 s, and agy
6–10 s. claude `-c` and agy `-c` second turns passed. codex `exec resume` failed with
`thread/resume failed: list_turns is not supported yet` (code `-32601`), an upstream codex 0.164
app-server limitation, **not** the sandbox.

## Shell probes (PASS)

Appending to `~/.claude/settings.json` was denied; appending to or reading `~/.ssh/config` was
denied; reading `~/.ssh/id_rsa` was denied; appending to the checkout's `AGENTS.md` was denied;
writing under `~/Desktop` was denied.

## Hardlink probes (observed, host-specific)

The drill links a read-only-granted config into the writable checkout and distinguishes three
outcomes: the link is created and a write through it is refused (`LINKED_BUT_DENIED`, PASS), the
sandbox refuses to create the link (`LINK_DENIED`, PASS), or the write through the link succeeds
(`WRITABLE`, FINDING). On macOS 26.5.1 + `nono` 0.80.0 the result was `LINK_DENIED` for both
`~/.claude/settings.json` and `~/.codex/config.toml`, so the alternate-inode-name path was closed in
this probe. That is an observation on this host, not a cross-platform guarantee; the denials still
name paths, not inodes.

## Worktree-mode Git (PASS)

A sandboxed commit in a linked `git worktree` succeeded. `git config --local` and a write to
`$(git rev-parse --git-common-dir)/hooks/pre-commit` were denied.

## Shared-checkout guard (PASS)

A child that ran `git config core.hooksPath` inside the shared checkout tripped the guard (the
example exits 3) before the caller's next Git call.

## Refs (PASS in worktree mode, 2026-10-28; historical FINDING superseded)

In worktree mode, `worktree_refs` transitioned to **PASS**: each worktree child is granted only its
own ref directory under `refs/heads/<prefix>/<label>/` and reflog directory under
`logs/refs/heads/<prefix>/<label>/`. An adversarial attempt to update `refs/heads/main` or any ref
outside the run's ref directory with `git update-ref` is denied by Seatbelt.

*Historical record (2026-10-27, superseded for worktree mode on 2026-10-28):*
Under the earlier whole-refs grant, the agent could run `git update-ref refs/heads/main <sha>`.
The drill recorded this as a `FINDING`. In shared-checkout mode, `shared_checkout_refs_writable` remains
a recorded finding; in worktree mode, the per-run ref grant now enforces containment.

## Review profile (PASS)

codex 8 s, claude 8 s, agy 10 s, all PASS. The review can read the checkout and cannot write it; it
can write its **per-run** codex home and is denied on the shared `~/.codex`; it cannot write
`AGENTS.md`. claude and codex reviews replied with the file content. Review launches originally
failed before the `--allow-cwd` fix with
`nono: CWD access requires --allow-cwd in non-interactive mode`.

## Egress allowlist (2026-10-27, PASS)

The egress probe passed 3/3: a non-allowlisted domain (`example.com`) was refused by `nono`'s proxy,
an allowlisted endpoint (`api.anthropic.com`) was reachable (HTTP 401 without a credential is a
reach, not a denial), and the review profile kept the same reach. The same run's matrix passed
(codex 9 s, claude 7 s, agy 12 s) and its review passed (read-only probes; codex 9 s, claude 6 s,
agy 9 s).

What the experiment established before the network policy was committed:

- codex works under the proxy;
- agy needs loopback plus `daily-cloudcode-pa.googleapis.com` (its eligibility check);
- claude needs an arbitrary-port loopback bind; `network.block = true` breaks claude
  (`ECONNREFUSED`), and an allowlist without localhost ports breaks claude (`EPERM`) and agy
  (bind `EPERM`);
- a later narrowing (2026-10-28) split the grant per harness: codex needs none, agy completes with
  a **listen-only** bind, and claude needs the connect wildcard — listen-only fails for claude with
  `Unable to connect to API (EPERM)`;
- the canary to `example.com` returns HTTP 000/403.

## Policy variants (2026-10-27)

Variant profiles were run to see which grants are load-bearing:

| Variant | Result |
| --- | --- |
| no `~/Library/Preferences` read | codex, claude, agy all PASS (exit 0) |
| no keychain files, no `bypass_protection` | claude FAIL (`Not logged in · Please run /login`); agy PASS |
| `network.block = true` | claude FAIL (`API Error: Connection refused (ECONNREFUSED)`) |

The committed profile (Preferences read removed) then re-ran the scripted drill green: matrix codex
19 s, claude 10 s, agy 11 s; review codex 18 s, claude 7 s, agy 9 s. The experiment behind the
per-run `CODEX_HOME` measured codex at 1,919 tokens versus 14,535 with the shared home; it is now
implemented policy rather than a follow-up.

## Per-run `CODEX_HOME` and alternate surfaces (2026-10-27)

With the static `~/.codex` grants removed, the matrix passed (codex 10 s, claude 8 s, agy 10 s) and
the review drill passed read-only probes, per-run-home write, shared-home denial, and all three
reviews (8/8/10 s). Alternate non-interactive surfaces also ran under the sandbox:

- `codex exec -C <dir> review --uncommitted` — the `-C` must come before the `review` subcommand;
- claude `-p --output-format stream-json` — requires `--verbose`;
- agy `--output-format stream-json` — NDJSON init/step events.

**grok and dsh remain unexercised**: the Homebrew `grok` formula is an unrelated log tool,
`@xai/grok-cli` and `grok-cli` are 404 on npm, and `dsh` was not installed.

## Limits of this evidence

One host, three harnesses, one `nono` version. grok and dsh are not exercised, codex resume is
upstream-broken, shared-checkout refs are writable (worktree refs contained), the keychain and egress residuals are documented, and the
hardlink result is host-specific. See the README's
[honest limits](../README.md#honest-limits) for the full list.

# Sandbox policy: what is contained, what is exposed

`workcell` launches a coding agent behind an operating-system sandbox and offers no way to run it
unconfined. This document is the crate-level policy: the two reviewed profiles under
`src/sandbox/profiles/` are the source of truth, and this page explains what they contain, what they
deliberately expose, and why each unusual grant exists. The README summarises it; this is the detail
an operator or auditor needs.

The profiles are embedded in the crate and rendered with `SandboxOptions::state_dir` before they are
published content-addressed and handed to `nono`; the rendered bytes are what the digest covers and
what is verified before a launch.

## Contained by the sandbox (macOS Seatbelt)

- Writes outside the run's checkout, except the harness **state**, logs, crash reports, and package
  caches a coding agent needs to run: `~/.cache`, `~/.claude/{projects,todos,shell-snapshots,statsig,debug}`,
  `~/.grok/{sessions,log,crash}`,
  `~/.gemini/{history,antigravity-cli/{cache,conversations,crashes,log,scratch,presence,knowledge,implicit,annotations,brain}}`,
  `~/.cache` and the named harness state. `~/.cargo/{registry,git}` are **read-only**: a full
  offline rebuild from a warm cache wrote no file there, so a build needs to read a package cache,
  never write one. Single-file grants cover the state a CLI keeps in one file:
  `~/.cargo/.package-cache`, `~/.gemini/state.json`, and the Antigravity/agy
  `antigravity-oauth-token`, `history.jsonl`, `conversation_summaries.db`, `jetski_state.pbtxt`, and
  `last_check.timestamp`. The profile includes the `node_runtime` group because codex is a Node CLI
  installed under `~/.nvm`; without the Node toolchain it dies at startup with `MODULE_NOT_FOUND`.
  **Codex is different: neither profile grants any `$HOME/.codex` write entry.** The shared
  `~/.codex` is read-only. Each codex child gets a fresh per-run home the caller creates, with the
  operator's `auth.json` linked into it (read through the `~/.codex` read grant, never copied),
  granted through `SandboxSpec::writable_paths` (`--allow`), and removed when the run ends. Harness
  **configuration** — `~/.claude`, `~/.claude.json`, `~/.codex`, `~/.grok`, `~/.gemini`,
  `~/.antigravity`, and `~/.dsh` — is read-only: a hook, notify command, MCP server, or plugin
  planted in `~/.claude/settings.json`, `~/.codex/config.toml`, `~/.grok/config.toml`, or
  `~/.gemini/antigravity-cli/settings.json` would execute in the human's next session, outside this
  sandbox, so it is never writable. The unusual grants are explained in
  [Why these grants](#why-these-grants).
- **The review profile is read-only for the checkout, not for a per-run codex home.** A codex review
  gets its own fresh `CODEX_HOME` (a per-attempt directory below the caller's state root) granted
  through `writable_paths` and removed after, while agy's state grants remain static. The checkout
  and harness configuration stay unwritable; a review cannot commit, edit the checkout, or plant a
  hook. Review launches also carry `--allow-cwd` so nono can start a non-interactive child against a
  read-only workdir.
- In worktree mode, the repository's execution-semantics paths are outside the writable checkout and
  are not granted. The agent receives four explicit Git metadata write grants: `.git/objects`, the run's
  own ref directory under `.git/refs/heads/<prefix>/<label>/`, the run's reflog directory under
  `.git/logs/refs/heads/<prefix>/<label>/`, and its own `.git/worktrees/<name>`. Neither the root `refs/`
  nor `logs/` directory is granted, and other branch refs cannot be modified. Base branch updates (e.g. `main`)
  are the caller's integration responsibility outside the agent sandbox. `.git/hooks`, `.git/config`,
  `.git/info`, and every other worktree are neither granted nor reachable. A shared checkout is different:
  `.git` is inside the writable checkout and only the guard protects it — see Honest limits.
- Agent instruction and configuration files inside the checkout are denied **read and write**:
  `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.cursorrules`, `.mcp.json`, and `.claude/`, `.codex/`,
  `.cursor/`, `.gemini/`, `.grok/`, `.agents/`. The instruction text the caller supplies is how those rules
  reach the agent.
- Host credential stores: SSH keys, cloud config, `gh` configuration, Cargo credentials files, and
  the other paths named by `nono`'s credential-deny groups. The six run-control paths under
  `SandboxOptions::state_dir` (default `.workcell`) are also denied.
- **Git never runs repository code in the caller's process tree.** Every `workcell::git` operation
  is in-process through libgit2: no Git child process, no repository hook, and no external diff,
  textconv, or `GIT_EXTERNAL_DIFF` program. Commit signatures come from the supplied
  `GIT_COMMITTER_*` environment (used for both author and committer) or the repository config.
  Credential helpers are read only from global/XDG/system config — a repository-local
  `credential.helper` is ignored — and run with `env_clear()` plus the caller's supplied
  environment; HTTPS uses URL userinfo then the helper chain, and SSH uses ssh-agent then an
  unencrypted default key. The shared-checkout guard hashes a v4 local-config digest plus hooks,
  `.git/info`, and repository identity, and refuses the run on a mismatch; see Honest
  limits for the timeout and SSH-config residuals.
- **Egress is restricted to a curated provider allowlist.** Both profiles run nono's filtering proxy
  for a fixed `network.allow_domain` set (the OpenAI, Anthropic/Claude, Google/Antigravity, and
  Codex endpoints listed in §Why these grants); a direct connection to any other host is refused.
  The proxy is hostname-level: it opens plain-host `CONNECT` tunnels and does no L7/SNI inspection,
  so it bounds *which hostnames* are reachable, not what is sent to them. Loopback is granted per run, by the **caller**, because it is harness-specific: codex needs none,
  agy needs a **listen-only** bind (`--listen-port 0`) and completes without the connect half, and
  claude needs the connect wildcard (`--open-port 0`) — listen-only fails with `Unable to connect
  to API (EPERM)`, and a `49152–65535` narrowing failed before that.
- A child is killed as a **process group**, and the process layer reaps group members left behind
  by a **normal** exit, so a `command &` cannot outlive the run and act after the caller's guard. A
  grandchild that calls `setsid` creates a new session and escapes the group kill (measured
  2026-10-28): it stays inside the sandbox, but it can outlive the run.

## Exposed and not contained

- **The allowlist is a curated set, not a complete policy.** A provider or MCP/plugin domain that
  is not in the list is refused until it is added; the committed set was validated with the three
  supported harnesses, not with every MCP server or plugin. Loopback is granted only when a caller
  asks for it (so a codex run reaches no local service), and the caller's
  unsandboxed verification commands still have network (they run outside the sandbox by design).
  See [Honest limits](../README.md#honest-limits).
- Reads are broader than writes: the toolchain, Cargo/npm caches, and harness configuration and
  state are readable. `~/Library/Preferences` used to be read for Codex CFPreferences; the P5
  experiment showed it is unnecessary (a variant without it ran codex, claude, and agy), so it is no
  longer granted.
- **The package caches are read-only, but an unsandboxed verifier still trusts what is in them.**
  `~/.cargo/registry` and `~/.cargo/git` moved from `allow` to `read`, so a run cannot edit an
  extracted dependency's source or `build.rs` (measured against a full offline rebuild, which wrote
  zero files). A cache poisoned before the run, or a checkout the run edited, is still compiled and
  executed by the caller's unsandboxed verification. Containing the agent and then running its code
  outside the sandbox is the larger half of the gap; the read-only cache closes the write channel,
  not the execution one. The escape drill's `cargo_cache_write` probe asserts the denial.
- **The macOS pasteboard is readable.** The escape drill's `--aggressive` clipboard probe read
  the pasteboard through the agent profile under nono 0.80.0; the profile has no clipboard grant or
  deny primitive. A password copied to the clipboard during a run is therefore visible to the
  agent. Treat it as a residual, not a containment claim.
- On macOS the **two login keychain database files are readable**, because the sandbox has no finer
  keychain granularity. The databases are encrypted and the `security` Mach services are not among
  the granted IPC, yet an item secret **is** retrievable: measured 2026-10-28, `security
  find-generic-password -s "…" -w` exited 0 inside the sandbox. Treat it as item-level exposure of
  the login keychain, not merely readable ciphertext. The two explicit keychain
  files are not optional: dropping them plus `bypass_protection` makes claude fail with
  `Not logged in · Please run /login` while agy still passes, so they stay for Claude auth.
- **Hard links and denied paths.** The profiles deny by path, not by inode. The 2026-10-27 drill
  found that this Seatbelt profile refused to create a hardlink to a read-only-granted config from
  the writable checkout (`LINK_DENIED`), so the alternate-name path was closed in that probe; that
  is an observation on macOS 26.5.1 + nono 0.80.0, not a cross-platform guarantee.
- **Shared-checkout `.git` is detected, not contained**, and **in shared-checkout mode refs remain writable**.
  In worktree mode, refs are contained to the run's ref directory and cross-run ref tampering is closed.
  In shared-checkout mode, guard detection covers `.git/config`, `.git/config.worktree`, `.git/info`, and hooks,
  but refs and objects remain inside the writable checkout and writable across the repository by design;
  worktree mode or an outer boundary is the stronger choice.
- A caller that puts other state inside a shared checkout is responsible for keeping the agent
  away from it: the profile denies only the six run-control paths under
  `SandboxOptions::state_dir`.
- The kernel is shared. macOS Seatbelt is an OS control, not a virtual machine; a kernel
  vulnerability is outside this boundary.
- The caller's own work is not sandboxed. Verification commands, `git` calls made by the caller,
  and any other process the caller starts are its responsibility. `workcell` wraps exactly the
  child it is asked to wrap.
- There is no nested sandbox on macOS: if the calling process already runs inside another OS
  sandbox, `nono` cannot initialize a second Seatbelt profile.

## Why these grants

The profile is a reviewed list, not a generated one. Each unusual entry answers an operator's
question about why it is there.

**Why the keychain files need `bypass_protection` (and cannot be dropped).** `nono`'s required
`deny_keychains_macos` group denies `~/Library/Keychains/**`, and a deny wins over a read grant.
Keychain-backed harness login therefore needs both a `read` grant for the two specific files
(`login.keychain-db` and `metadata.keychain-db`) and a matching `bypass_protection` entry; every
other path under the keychain directory stays denied. The P5 experiment tried removing them: claude
then failed with `Not logged in · Please run /login`, while agy still succeeded. The grants are
required for Claude auth and stay.

**Why `~/Library/Preferences` is no longer read.** It was granted for Codex's CFPreferences use. The
P5 variant without it ran codex, claude, and agy successfully (all exit 0), and the committed
profile re-ran the full matrix and review drill green, so the read was unnecessary and has been
removed from both profiles.

**Why codex state is per run, and why the static grants are gone.** Codex's app server writes
versioned SQLite databases, queue files, locks, and logs under `CODEX_HOME`. Granting each of those
inside the operator's `~/.codex` meant tracking names that change with a codex upgrade, and it left
the operator's own home writable to the agent. Both profiles now grant **no** `$HOME/.codex` write
entry: the shared home stays read-only. The caller gives each codex child a fresh `CODEX_HOME`
(one per attempt, for example
`<state>/codex/<run-id>`), links `auth.json` from the real home into it, grants
it through `SandboxSpec::writable_paths` (`--allow`), and removes it when the task ends. The P2.9
experiment is what justified it: the per-run home ran codex 0.164 inside the sandbox at 1,919
tokens versus 14,535 with the shared home, with no static state grant. The caveats are real: the
auth symlink still needs the `~/.codex` read grant; the home **must** be per-attempt, because a
reused one could carry a planted `config.toml` or hook into a later attempt; and codex resume is
already upstream-broken (`list_turns is not supported yet`).

**Why `SandboxSpec::writable_paths` exists.** The profiles are embedded policy and cannot know a
run's state directory. `writable_paths` is the caller's narrow write grant outside the checkout;
`wrap_agent`/`wrap_review` emit `--allow <path>` for each, after `--allow-cwd`, and a path must be a
non-empty absolute path (the wrapper refuses otherwise). It is for a per-run harness home, not for a
general writable directory.

**Why `~/.config/gh` and `~/.cargo/credentials.toml` are explicit denials.** `nono`'s
`deny_credentials` group does not list either path. `~/.cargo/credentials.toml` sits inside the
granted `read $HOME/.cargo`, so without an explicit deny it would be readable. `~/.config/gh` is not
reachable by a current grant; it is denied as defence in depth so a future `$HOME/.config` grant
cannot expose the GitHub CLI token.

**Why grok and agy get state grants but not configuration.** Both CLIs keep session history, logs,
crash reports, caches, and a few single-file state documents under their home directory, next to
their configuration. The profile grants only those state paths. `~/.grok/config.toml`, the grok
plugins/skills/agents directories, and `~/.gemini/antigravity-cli/{settings.json,mcp,builtin,bin,updater}`
stay read-only: code planted there would run in the human's next session, outside this sandbox. Both
profiles also deny `$WORKDIR/**/.grok/**` so a repository cannot ship a project-level grok
configuration that this run would read.

**Grok auth.** xAI's settings reference documents `GROK_HOME` (default `~/.grok`) as the home for
config, auth, sessions, skills, plugins, and logs, with sessions under `~/.grok/sessions`
([docs.x.ai](https://docs.x.ai/build/settings/reference), accessed 2026-10-27). It does not name the
auth file, so the profile leaves `~/.grok` read-only: a run must start from an established login,
and a live drill should confirm whether token refresh ever needs a writable auth file.

**Why the review profile writes a per-run codex home.** A review is a coding-agent child and needs
writable codex state to start. Rather than widen the review profile, the review gives codex the same
treatment as a fresh run: a fresh `CODEX_HOME` under the caller's state root, granted
through `writable_paths` and removed after. The checkout and harness configuration stay unwritable,
so a review still cannot commit, edit the checkout, or plant a hook; the write grant is the CLI's
own bookkeeping for one attempt. agy's state grants remain static in both profiles.

**Why `node_runtime` is in both profiles.** Codex is a Node CLI installed under `~/.nvm`. Without
the `node_runtime` group, the sandboxed codex process exits with `MODULE_NOT_FOUND` before it ever
talks to a provider.

**Why the network is a provider allowlist, not a block.** A blanket `network.block = true` breaks
the supported harnesses (P5: claude fails with `API Error: Connection refused (ECONNREFUSED)`), so
the profiles instead set `network.block = false` with `network.allow_domain` naming the endpoints
the three harnesses need: `api.anthropic.com`, `api.openai.com`, `auth.openai.com`, `chatgpt.com`,
`claude.ai`, `console.anthropic.com`, `statsig.anthropic.com`, `accounts.google.com`,
`clients6.google.com`, `oauth2.googleapis.com`, `www.googleapis.com`, `generativelanguage.googleapis.com`,
`cloudcode-pa.googleapis.com`, `daily-cloudcode-pa.googleapis.com`, `play.googleapis.com`,
`fonts.gstatic.com`, `cdn.openai.com`, and `www.gstatic.com`. nono's proxy opens a plain-host
`CONNECT` tunnel for those hosts and refuses the rest; it is not a content filter. **A caller-granted loopback grant is required for some harnesses**: claude (Bun-native) needs the
connect wildcard and fails with `EPERM` without loopback IPC, agy completes with a listen-only
bind, and codex needs none. To extend the policy, add the
provider host to both profiles, re-run `scripts/escape-drill.sh --mode both` and a live harness matrix, and
update this section. Do not describe the set as complete for MCP servers or plugins: those domains
were not exercised.

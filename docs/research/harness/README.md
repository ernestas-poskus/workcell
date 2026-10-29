# Harness surfaces behind the sandbox profile

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

**Date:** 2026-10-22. **Harnesses:** `claude` 2.1.303 (Claude Code), `codex-cli` 0.163.0,
`agy` 1.4.5 (Antigravity CLI).

This is the synthesis. Six per-harness notes sit beside it, produced as a stable-surface survey and
an experimental/practice dive per harness:

| Note | Harness | Depth |
|---|---|---|
| [`claude-web.md`](claude-web.md) | claude 2.1.303 | documented surface |
| [`claude-deep.md`](claude-deep.md) | claude 2.1.303 | experimental + practice |
| [`codex-web.md`](codex-web.md) | codex 0.163.0 | documented surface |
| [`codex-deep.md`](codex-deep.md) | codex 0.163.0 | experimental + practice |
| [`agy-web.md`](agy-web.md) | agy 1.4.5 | documented surface |
| [`agy-deep.md`](agy-deep.md) | agy 1.4.5 | experimental + practice |

## 1. Purpose and scope

`workcell` launches a coding-agent CLI inside an OS sandbox. The profile it launches behind is
static policy: it must let the CLI start, authenticate, and persist its session, while refusing
everything a hostile checkout could use to execute code later. That split is not a guess about
harnesses — it follows from what each CLI reads, writes, and executes on a host:

- **Configuration is read, never written by the child.** A CLI reads instruction files, settings,
  hooks, plugins, MCP server definitions, and credentials at startup. If a run could write any of
  them, a later session outside the sandbox would execute the planted hook, notify command, MCP
  server, or plugin.
- **State must be writable, or the CLI degrades.** Session transcripts, logs, crash reports, and
  caches are written continuously. A read-only home makes resume silently become "fresh" and turns
  startup diagnostics into noise.
- **The working tree is where the agent edits, and also where the same files are auto-discovered.**
  A checkout's `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.mcp.json`, `.claude/`, `.codex/`,
  `.gemini/`, and `.agents/` are inputs to a headless run, not inert text.

Scope is the **documented surface of the stated versions**, established from the installed CLIs'
`--help`, changelogs, and upstream source at the pinned tag, then cross-checked against vendor
documentation. This document contains no live dispatch: the harness research was documentation- and
source-level only. It records which facts the committed profiles encode and why.

## 2. Method and provenance

- **Local first.** The installed `--help`, `changelog`, `features list`, and source at the release
  tag win over web documentation; every note records version drift with dates. The three installed
  CLIs are newer than most public documentation, and some docs describe post-install behaviour.
  Treat "not in `--help`" as inconclusive: claude's own docs say `--help` does not list every flag,
  and codex accepts removed feature keys as no-ops.
- **Web second.** Primary upstream (official docs, changelogs, repository source at the installed
  tag, maintainer issues) first; UGC is labelled as hypothesis and never carries a material claim
  alone.
- **All sources are dated 2026-10-22**, and every note carries its own `## Sources` list plus a
  `## Gaps / could not verify` section. Confidence and provenance labels are per claim.
- **No live dispatch was run.** No harness was launched against a checkout for this research, and no
  plan was implemented. Two notes disclose small accidental billed turns while probing flag
  semantics: one short claude turn, and two minimal agy turns (~11.3k input tokens, 28 output
  tokens). Everything else was help output, source reading, and zero-token slash commands.

## 3. Per-harness surface

### 3.1 Claude Code 2.1.303

- **Home and configuration.** User state and config under `~/.claude/` (including `CLAUDE.md` and
  `settings.json`); project config in `./.claude/settings.json`, `.claude/settings.local.json`, and
  `.claude/rules/`; instructions in `./CLAUDE.md` (which may import `@AGENTS.md`), `./AGENTS.md`,
  and `CLAUDE.local.md`. `--settings` overlays a file or JSON, `--setting-sources user,project,local`
  selects scopes, `--safe-mode` disables CLAUDE.md, skills, plugins, hooks, and MCP discovery while
  keeping auth, models, tools, and permissions, and `--bare` skips the same discovery but will not
  read OAuth credentials or the keychain.
- **State.** Session transcripts (`.jsonl`; `--resume` also accepts an absolute transcript path),
  per-project data under `~/.claude/projects`, `todos`, `shell-snapshots`, `statsig`, and `debug`.
  `--no-session-persistence` is print-only and makes the session unresumable. `--bg` is rejected
  with `-p`.
- **Non-interactive permission.** Since 2.1.302 a `-p` run with **no permission mode set starts in
  Manual and denies** anything that would prompt. `--dangerously-skip-permissions` is therefore
  load-bearing, not convenience; it is equivalent to `--permission-mode bypassPermissions` and is
  refused as root/sudo. A project-level `permissions.defaultMode: bypassPermissions` is **inert**.
  `--permission-prompts none` removes person-answering tools; `--restricted` refuses bypass.
- **MCP, plugins, hooks.** `--mcp-config` only *adds* servers; `--strict-mcp-config` is the exclusive
  switch. A repository's `.mcp.json` is connected or attempted with **no trust prompt and no
  per-server approval** in `-p`, and plugin, skill, and hook discovery are on by default. The
  deterministic hook off-switch is `--settings '{"disableAllHooks": true}'`.
- **Exit-code traps.** 0 on success; **SIGTERM exits 143** with no result for the in-flight turn. A
  denied skill-frontmatter substitution has been reported as `subtype:"success"`, `is_error:false`,
  `num_turns:0` with empty `result`. `is_error` is undocumented; key on process exit code plus
  `subtype`.
- **Background and idle.** After the final turn a `-p` run waits up to 10 minutes for background
  work (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, default 600000, `0` = forever). Foreground Bash
  defaults to 120 s, after which a command may be moved to the background.
- **Version drift.** The installed 2.1.303 was five patches behind 2.1.308 at access time; docs
  already describe post-2.1.303 behaviour; `--help` is knowingly incomplete; `--effort`'s value set
  disagrees between help, runtime warning, and docs; `--disable-slash-commands` semantics moved from
  commands to skills; `--bare` is announced as a future `-p` default.

### 3.2 Codex 0.163.0

- **Home and configuration.** `CODEX_HOME` (default `~/.codex`) is the root for "config, auth, logs,
  sessions, skills, and standalone package metadata"; `$CODEX_HOME/config.toml` is the base config,
  `$CODEX_HOME/<name>.config.toml` is a profile, and a project `.codex/config.toml` loads only when
  the project is trusted (`projects.<path>.trust_level`, recorded in user config). `--ignore-user-config`
  skips the user config layer while auth still comes from `CODEX_HOME`; `--ignore-rules` skips user
  and project execpolicy `.rules`.
- **State.** Versioned SQLite state, queue files, locks, rollouts, and logs under `CODEX_HOME`;
  `history.persistence = save-all|none`; `-o/--output-last-message` writes the final message to a
  file and only on a completed turn. On a research host the state DB failed an integrity check with
  thread rows unqueryable — state is not something to assume healthy.
- **Non-interactive permission.** `codex exec` already defaults to `approval_policy = never` and
  rejects approval server-requests rather than waiting; `exec` has no `-a`. The sandbox is separate:
  `exec` defaults to `-s read-only`, and `--dangerously-bypass-approvals-and-sandbox` sets
  `danger-full-access` and removes the OS sandbox. `workspace-write` keeps network off unless
  `sandbox_workspace_write.network_access = true`, and keeps `.git`, `.codex`, `.agents`, and `.aws`
  under a writable root read-only, so `git commit` fails there. `--approve-for-me` is a reviewer,
  not a bypass.
- **MCP, plugins, hooks.** `-c mcp_servers={}` is a **no-op — tables merge, it does not replace**;
  dotted `-c mcp_servers.<id>.enabled=false` does work, and `--disable plugins` drops
  plugin-bundled MCP servers (`apps` is an independent gate). `mcp_servers.<id>.required = true`
  turns an init failure into a process exit. `--dangerously-bypass-hook-trust` exists for
  automation that has vetted hook sources.
- **Exit-code traps.** 0 after a completed turn; 1 on a non-retry `Error`, a Failed/Interrupted turn,
  or a rejected approval; 2 for clap-level invalid flags. An interrupted turn emits no terminal JSONL
  event. Exit 0 means only that the turn ended — a failed `-o` write can still exit 0.
- **Background and idle.** `exec` ends at turn completion; reconnect/retry loops surface as
  `error`/`turn.failed`. `codex exec review` has a known hang where a Git failure emits `error` and
  never ends the turn, so "exec always terminates" is not safe for every subcommand.
- **Version drift.** Public docs were rendered from a commit roughly eight months older than
  0.163.0; `--full-auto` is gone, `--yolo` is a hidden alias, `--search` and `-a` are absent from
  `exec` (config-only), feature stages are five including `deprecated` and `removed` (a removed flag
  parses and does nothing), the model catalog has drifted (the older `gpt-5.3-codex` pin is absent),
  and `CODEX_CA_CERTIFICATE` (singular) is documented while the plural spelling is not.

### 3.3 Antigravity CLI (`agy`) 1.4.5

- **Home and configuration.** `~/.gemini/antigravity-cli/settings.json` holds `permissions`,
  `toolPermission`, `artifactReviewPolicy`, `enableTerminalSandbox`, `allowNonWorkspaceAccess`,
  `enableTelemetry`, `modelProvider`, and `trustedWorkspaces`. Workspace rules are cumulative:
  `AGENTS.md` and `GEMINI.md` (always on, no frontmatter), `.agents/AGENTS.md|GEMINI.md`,
  `.agents/rules/*.md`, plus global `~/.gemini/AGENTS.md`, `~/.gemini/GEMINI.md`,
  `~/.gemini/config/**`, and `~/.gemini/antigravity-cli/rules/*.md`, with a 24 KB per-file cap and a
  ~20k-token aggregate budget. 1.4.4 fixed rules overriding earlier built-in instructions.
- **State.** Default log `~/.gemini/antigravity-cli/log/cli-<timestamp>.log` (overridable with
  `--log-file`), crash reports under `crashes/crash_<pid>_<uuid>.log`, `cache/last_conversations.json`,
  and the conversation/summary SQLite store. In a confined home all of these fail with
  `operation not permitted` — the CLI still prints an envelope and exits 0, but resume becomes
  unreliable.
- **Non-interactive permission.** Default headless policy is `request-review`: a tool needing
  approval is **soft-denied**, the run continues, and it **exits 0**. `--dangerously-skip-permissions`
  sets the effective posture to `always-proceed`. `--mode accept-edits|auto|plan` exists but is
  under-documented and **fail-open** — a bad value warns and the run continues with the default.
  `--sandbox` turns on the terminal sandbox and denies network by default, which breaks build
  fetches; its bypass requests cannot be approved headless.
- **MCP, plugins, hooks.** There is no per-run MCP flag: user-level `mcp_config.json` servers load
  into every run, and headless runs block on MCP startup. `agy mcp list` prints each server's command
  line including inline credentials, so never capture it. Plugins can ship skills, rules, subagents,
  MCP servers, hooks, and sidecars; a plugin load failure is non-fatal and silently changes the
  toolset.
- **Exit-code traps.** 0 = success **but also soft-denied tools and `--print-timeout` expiry**
  (partial output, warning on stderr); 1 = startup/validation failure; 2 = print-mode usage errors;
  3 = a turn that streamed and then died on a model/API failure (`AGY_ERROR:` on stderr). Error
  envelopes carry `conversation_id: ""`, so an empty id is "no session". `agy models` exits 0 while
  unauthenticated and is not an auth probe.
- **Background and idle.** Headless runs wait for background tasks and scheduled timers, bounded by
  `--print-timeout` with a 30-minute cap. `--print-timeout` default is `0` = unlimited (docs still
  say five minutes).
- **Version drift.** Docs are stale on the print-timeout default, the `--effort` values, the model
  catalog, and `--json-schema` validation; `--mode` is missing from the docs flag table; `--cwd`
  does not exist despite an example using it. Capability is partly server-rolled through
  `ListExperiments`, so features can drift within one binary version.

### 3.4 Cross-harness capability rows

| | claude 2.1.303 | codex 0.163.0 | agy 1.4.5 |
|---|---|---|---|
| Headless entry | `claude -p … --output-format json` | `codex exec --json …` | `agy -p … --output-format json` |
| Required permission flag | `--dangerously-skip-permissions` (since 2.1.302) | `--dangerously-bypass-approvals-and-sandbox` | `--dangerously-skip-permissions` |
| Session key | `session_id`; `--session-id` / `--resume` | `thread_id`; `exec resume` | `conversation_id`; `--conversation` |
| Per-run MCP isolation | `--strict-mcp-config` | unresolved; `--disable plugins` + per-server keys + per-run `CODEX_HOME` | none — user-level only |
| Config isolation | `--safe-mode` / `--bare` / `--setting-sources` | `--ignore-user-config` + `--ignore-rules` | none documented |
| Usage source | `usage.*`; `total_cost_usd` is a client estimate | `turn.completed.usage` (thread running total) | `usage.*` incl. `cache_read_tokens` |
| Exit-code trap | 143 on SIGTERM; exit 0 on a reported no-op | 0 = turn ended only; interrupted = no terminal event | 0 on soft-deny **and** on print-timeout expiry |
| Background/idle | 10 min post-turn default | ends at turn completion; review can hang | 30 min cap; print timeout unlimited |
| Model/effort | `--model`, `--effort low…max` | `-m`, `-c model_reasoning_effort=low…ultra` | `--model`, `--effort low…max` |
| Auth | API key, OAuth token, Bedrock/Vertex/Foundry | ChatGPT subscription or API key | keyring, `GEMINI_API_KEY` + `modelProvider`, `AGY_ADC_AUTH` |
| Version drift seen | docs ahead of binary; `--help` incomplete | docs ~8 months stale; `--full-auto` gone | docs stale on effort/timeout/catalog |

## 4. Mapping to `workcell`

**Why harness configuration is read-only in both profiles.** The notes establish that every one of
these CLIs auto-discovers and acts on files a checkout controls: claude reads `CLAUDE.md`/`AGENTS.md`,
hooks, plugins, and `.mcp.json`; codex reads `AGENTS.md`, project `.codex/config.toml`, rules, and
`hooks.json`; agy reads `AGENTS.md`/`GEMINI.md`, `.agents/rules`, hooks, and user MCP servers. The
profiles encode that by denying instruction and harness-config paths inside the worktree
(`$WORKDIR/**/AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.cursorrules`, `.mcp.json`, and
`.claude/`, `.codex/`, `.cursor/`, `.gemini/`, `.grok/`, `.agents/`) and by keeping the harness
configuration in those homes — `~/.claude/settings.json`, `~/.claude.json`, `~/.codex/config.toml`,
`~/.gemini/antigravity-cli/settings.json`, and the rest of the files in `~/.claude`, `~/.codex`,
`~/.gemini`, `~/.antigravity`, `~/.grok`, and `~/.dsh` — readable but not writable, with only the
named state subdirectories granted below. A run can start and authenticate; it cannot plant
a hook, notify command, MCP server, or plugin for a later session outside the sandbox. This is the
same reason the review profile leaves the checkout read-only: harness configuration is the
persistence channel, and the checkout is the execution channel.

**Why some harness state is deliberately writable.** The notes show the cost of not granting it:
claude needs session/transcript, todo, shell-snapshot, and debug state to keep a run resumable, and
agy's log, crash, cache, and conversation stores fail loudly (though non-fatally) in a read-only
home. The agent profile therefore grants harness state — `~/.claude/{projects,todos,shell-snapshots,statsig,debug}`,
`~/.gemini/antigravity-cli/{cache,conversations,crashes,log,scratch,presence,knowledge,implicit,annotations,brain}`,
`~/.gemini/history`, plus single state files such as `~/.gemini/state.json`, the Antigravity OAuth
token file, `history.jsonl`, `conversation_summaries.db`, `jetski_state.pbtxt`, and
`last_check.timestamp` — and never the corresponding `settings.json`/`config.toml`. Codex is the
exception by design: **no `$HOME/.codex` write entry exists in either profile**, because codex writes
versioned SQLite databases, queues, locks, and logs under `CODEX_HOME`; tracking those names inside
the user's home would both chase upstream changes and leave the human's own home writable.

**Why the caller names its state directory (`SandboxOptions::state_dir`).** Both profiles deny six
run-control entries — `state.json`, `logs`, `runs/*.log`, `control`, `sandbox`, and
`integration.lock` — under one path segment. Any caller-applied artifacts (logs, a per-run codex
home, a control file it must keep from the agent) live under that segment, so the name cannot be
hard-coded without making the crate depend on one consumer's layout. `SandboxOptions::state_dir`
supplies the segment (default `.workcell`), validates it as a plain relative path (no absolute
path, `..`, empty component, or unsafe characters), and renders it into the profile **before** the
profile is content-addressed, published, and verified. The token appears only in those six denial
entries and never in a grant, so an artifact path a caller grants inside the directory stays
readable.

**Why per-run homes are granted (`SandboxSpec::writable_paths`).** The static profiles cannot know a
run's state root, and a per-run directory is the narrowest write grant outside the checkout. The
caller creates the codex home for one attempt (for example below its state root), links the real
`auth.json` into it (read through the `~/.codex` read grant, never copied), grants that one absolute
directory through `writable_paths` — emitted as `--allow`, which is read+write; read-only needs
belong in `read_paths` — and removes it when the run ends. The home must be per attempt: a reused
home could carry a planted `config.toml` or hook into a later run.

**Residual risks that follow from the surface.**

- **Configuration is not writable, so a harness that insists on writing settings fails.** agy key
  mode needs `"modelProvider": "gemini"` written into `settings.json`, which the profile cannot
  allow; that auth path is incompatible with the profile, which assumes cached OAuth/ADC. Claude's
  project-level `bypassPermissions` is inert anyway, so its absence loses nothing.
- **Package caches are read-only.** `~/.cargo/registry` and `~/.cargo/git` are `read`, not `allow`,
  so a run cannot edit an extracted dependency or a `build.rs`; the one file cargo must write is a
  single-file grant (`~/.cargo/.package-cache`). A build from a warm cache works; a package manager
  that wants to mutate the cache fails, and a cache poisoned before the run is still compiled by the
  caller's unsandboxed verification.
- **The pasteboard and the login keychain are outside the profile's granularity.** The Seatbelt
  profile has no clipboard primitive, and the two keychain database files are readable because there
  is no finer keychain grant; the observed consequence is item-level exposure of the login keychain,
  measured in the crate's own escape testing. These are residuals, not containment claims.
- **Network egress is a curated allowlist.** Provider and MCP/plugin domains not on it are refused
  until added, and the committed set was validated with these three harnesses, not with every MCP
  server or plugin.

## 5. Honest limits

- **Documentation- and source-level only.** No harness was dispatched for this research; statements
  about how a CLI behaves inside a sandbox are derived from its documented contract, its `--help`,
  its changelog, and its source at the installed tag. The crate's own live drill (real harnesses,
  real profiles) is separate measured evidence in [`../../live-drill.md`](../../live-drill.md) and
  [`../../escape-testing.md`](../../escape-testing.md).
- **Version-scoped.** Every statement is about claude 2.1.303, codex 0.163.0, and agy 1.4.5. All
  three had published newer builds or newer docs at access time, and in every case the public
  documentation lagged or led the binary; a different version can change a flag, a default, or an
  exit code.
- **The boundary is not in the CLI.** The sandbox is macOS Seatbelt, applied by `nono`; the CLI is
  confined by the kernel, not by its own permission flags. For the platform's limits see
  [`../macos-seatbelt.md`](../macos-seatbelt.md); for the delegate's profile semantics and known
  weaknesses see [`../nono-0.80.0.md`](../nono-0.80.0.md); for what the crate itself enforces see
  [`../workcell-audit.md`](../workcell-audit.md).
- **Probes are narrow.** Several claims remain marked `[hypothesis]`, `[inference]`, or unverified
  in the notes (MCP isolation under a real exec, cache and resume behaviour on a subscription, the
  exact startup latency MCP waiting adds). Each note lists what it could not settle.

## 6. The six notes

- [`claude-web.md`](claude-web.md) — claude 2.1.303's documented surface: print-mode contract,
  permissions, session lifecycle, instruction and MCP discovery, and a flag inventory.
- [`claude-deep.md`](claude-deep.md) — claude's experimental/preview surface, `CLAUDE_CODE_*`
  environment knobs, isolation strength, failure modes, and the permission matrix.
- [`codex-web.md`](codex-web.md) — codex 0.163.0's `exec`/`resume`/`fork`/`review` surface, feature
  stages, config keys, the empirical MCP probe, and the JSONL event contract.
- [`codex-deep.md`](codex-deep.md) — codex's feature-registry precedence, approval and sandbox
  mechanics, JSONL accounting, resume semantics, and practitioner reports.
- [`agy-web.md`](agy-web.md) — agy 1.4.5's print mode and JSON envelope, resume, permissions and
  modes, rules discovery, exit codes, and MCP surface.
- [`agy-deep.md`](agy-deep.md) — agy's server-rolled experiments, mode/permission matrices, auth and
  quota, plugins/hooks/scheduling, and confined-home behaviour.

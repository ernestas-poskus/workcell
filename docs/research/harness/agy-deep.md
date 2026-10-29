# Antigravity CLI (agy) — experimental features and headless dispatch practice

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

## Scope and versions checked

- **Installed build:** `agy` 1.4.5 (macOS host). Local `--help`,
  `agy changelog`, subcommand help, and non-model probes are the ground truth for this checkout;
  public docs are compared against them and drift is listed at the end.
- **Upstream snapshot:** the `main` CHANGELOG's newest entry is also `1.4.5`
  ([C9](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md)), so the
  installed build is current at the time of writing — no release-note drift, only docs drift.
- **Auth mode on the machine used for probing:** Google Cloud Application Default Credentials
  (`~/.config/gcloud/application_default_credentials.json` present); `GEMINI_API_KEY`,
  `GOOGLE_API_KEY`, `AGY_ADC_AUTH`, `GOOGLE_GEMINI_BASE_URL` were **unset** (presence checked,
  values never read). This matters: it is the enterprise/ADC path, and it explains why every run
  logs "You are not logged into Antigravity" for the consumer-account endpoints while model calls
  still succeed.
- **Local repo inputs:** a repository checkout's `AGENTS.md` and `GEMINI.md`.
- **Honesty note on cost:** two minimal billed turns were executed early, before the probe
  technique was switched to validation-only: `agy -p "say hi" --output-format json`
  (11,261 in / 28 out / 19 thinking / 0 cached, `SUCCESS`) and `agy -p x --mode bogus`
  (a turn ran; the flag typo was only a warning). Everything after that is either a read-only
  print-mode slash command (`num_turns: 0`, 0 tokens) or a deliberately invalid `--model`, which
  fails validation before any generation. No further billed runs were made.
- **Not read (constraint):** `.env*`, deployment configuration, runtime state, `~/.gemini` token or auth files. One exception surfaced by the tool itself: `agy mcp list` printed an MCP server
  command containing an inline access token to stdout. It is **not** reproduced here; see §8.

## Findings (by theme; tag = [fact|inference|hypothesis] [confidence], source)

### 1. Two gates, and no user-facing experiment toggles in 1.4.5

- [fact high] The CLI polls a **`ListExperiments`** service endpoint in the background at startup.
  Unauthenticated (consumer-account path) it fails and is logged:
  `W ... cache.go:135] Cache(loadCodeAssistResponse): Singleflight refresh failed: error getting token source: You are not logged into Antigravity.` followed by
  `E ... errorreport.go:224] Failed to poll ListExperiments: error getting token source: You are not logged into Antigravity.` — observed locally, `agy agents` / `agy models` stderr, 1.4.5.
  The poller is non-fatal: the same process then completed a model turn normally.
- [fact high] `/config` — the interactive settings namespace — has **no experiment/preview
  toggles** in this build. `agy -p "/config" --output-format json` returns the full effective
  settings document (0 tokens, `num_turns: 0`); its keys are `agentMode, allowNonWorkspaceAccess,
  altScreenMode, artifactReviewPolicy, clearScrollbackOnResize, colorScheme, contextWindow,
  copyOnSelect, customModelsConfig, disableSlashCommands, editor, editorMode,
  enableTerminalSandbox, gcp, hideActiveItems, historySize, model, modelProvider, notifications,
  permissions, pickerGrouping, queuedMessages, runningLightSpeed, sandboxProxy, seasonalThemesOff,
  seasonalThemesOptIn, showFeedbackSurvey, showTips, statusLine, title, toolPermission,
  trustedWorkspaces, useG1Credits, useTerminalCursor, verbosity, vimInsertFirst`. None gate Auto
  mode, Remote Control, plugins, subagents or scheduling.
- [inference med] Feature availability is therefore **server-driven** (the experiments response)
  plus per-OS build gating, not a local flag: `--mode auto` is accepted by 1.4.5 on this macOS host,
  while the changelog scopes availability by OS and account type — "Auto mode is available on Linux
  and macOS" (1.3.5) then "Added Auto mode on Windows 11 24H2 … for personal Google Accounts"
  (1.4.4) ([C9](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md)).
- [hypothesis low] An `experimental` object in `settings.json` existed in 1.1.x: a third-party
  integration wrote `{"experimental": {"skills": true}}` at install time
  ([C11](https://github.com/harbor-framework/harbor/pull/2776)). Whether 1.4.5 still honours that
  namespace is unverified; it is not a `/config` key and `/config` reports only sparse persisted
  values, so absence in that output proves nothing.
- **Dispatch consequence:** preview behaviour cannot be pinned with an env var or flag for
  reproducibility. A dispatch box can change capability between days when the rollout changes; the
  only stability lever is pinning the CLI version and measuring what the build exposes
  (`/config`, `/permissions`, `/hooks`, `/usage`, `--help`).

### 2. `--mode` is real, under-documented, and fail-open on typos

- [fact high] `agy --help` (1.4.5) documents `--mode accept-edits, auto, plan`; the public flag
  reference does not list `--mode` at all ([C1](https://antigravity.google/docs/cli/headless) has no
  `--mode` row), while the changelog announces it (`1.3.5` Auto mode, `1.4.4` Windows Auto mode).
- [fact high] A wrong value does **not** fail the run:
  `warning: unrecognized --mode value "bogus" (valid: accept-edits, auto, plan)` on stderr, then
  execution continues with the default mode (verified locally; the same command with a bogus
  `--model` then exited 1 only because of the model).
- [fact high] `--mode plan` and `--mode accept-edits` are accepted without complaint by `/config`
  and by a validation-failing run; `--mode` was formerly *ignored* in print mode — "Fixed `--mode`
  being ignored in headless `-p` runs, where a valid value such as `accept-edits` or `plan` was
  never applied and an unrecognized value produced no warning at all" (1.1.12, [C9]). So the
  correct-spelling requirement is recent and load-bearing.
- [inference med] `accept-edits` ≈ approve edits and routine commands but keep destructive prompts;
  `auto` adds an LLM reviewer for commands that would leave the terminal sandbox (1.3.5, 1.3.6,
  1.4.3, 1.4.4). With `--dangerously-skip-permissions` both are moot for approval, which is why a bypass-flag argv works.

### 3. Permission model: `--dangerously-skip-permissions` is the only reliable headless grant

- [fact high] Default headless policy is `request-review`. A tool that needs approval it cannot get
  is **soft-denied**: the run continues, exits `0`, and prints a stderr notice naming the tool and
  the allow rule ([C1](https://antigravity.google/docs/cli/headless)).
- [fact high] Since 1.1.20, print mode treats benign tool errors and permission denials as
  non-fatal, "ensuring headless exit codes reflect only cascade-level failures and match interactive
  session behavior" ([C9]). Since 1.1.27 the refusal is also reported as `denied_actions` in the
  JSON envelope ([C9]). This is the documented failure mode — *"without it the agent runs, is denied its first edit/command, and exits 0 with nothing changed"*: the CLI is behaving as designed, not malfunctioning.
- [fact high] `--dangerously-skip-permissions` sets the run's effective posture to
  `permission_mode: "always-proceed"` in the `stream-json` `init` event ([C1]).
- [fact high] `--sandbox` "turns the sandbox on for the session, overriding `settings.json`"
  ([C4](https://antigravity.google/docs/sandbox)); the sandbox derives filesystem/network bounds
  from permissions and **runs sandboxed commands without network by default** (only `read_url`
  allowlist domains are reachable). A prompt/agent that needs `cargo fetch`/`npm install` will fail
  or request a bypass — and a bypass request needs approval, which headless cannot give. Do not add
  `--sandbox` to a dispatch argv.
- [fact high] `/config` is a **settings probe, not an effective-mode probe**: passing `--sandbox`,
  `--dangerously-skip-permissions`, `--mode accept-edits`, `--mode auto`, `--mode plan`, or
  `--model … --effort …` left its reported `toolPermission` (`request-review`),
  `enableTerminalSandbox` (`false`), `agentMode` (empty) unchanged (verified 1.4.5). The only
  observable of the *effective* posture is `init.permission_mode` on a `stream-json` run ([C1]).
- [fact high] Permission rules have three scopes — **project / shared / global** — and
  `agy -p "/permissions" --output-format json` returns them as structured data (0 tokens); this box
  has two global allow rules (`command(openspec)`, `command(git log)`) ([C8](https://antigravity.google/docs/cli/commands/permissions), verified locally).
- [inference high] Because global allow rules are inherited from the user's home, two identical argv invocations on two boxes can behave differently. `--dangerously-skip-permissions` flattens
  that difference; the rules only matter if a future design drops the flag.

### 4. Headless JSON/stream-json contract, exit codes, and the timeout trap

- [fact high] `--output-format json` emits one envelope on stdout: `conversation_id, status,
  response, error (failure only), duration_seconds, num_turns, structured_output/json_schema
  (with --json-schema), usage{input_tokens, output_tokens, thinking_tokens, cache_read_tokens,
  total_tokens}`. Diagnostics go to stderr, keeping stdout parseable ([C1]). Verified locally: the
  envelope above is exactly what 1.4.5 printed, and `status: "SUCCESS"`.
- [fact high] `status` enum: `SUCCESS | ERROR | CANCELED | INTERRUPTED | INVALID | WAITING |
  RUNNING` ([C1]).
- [fact high] Exit codes, assembled from docs + changelog: `0` success (and soft-denied tools);
  `1` startup/validation failures (`invalid model selection …`, unreadable/root-not-object
  `--json-schema`, unknown `--agent`); `2` print-mode usage errors (`--print` with
  `--prompt-interactive`; interactive-only slash commands, e.g. `/mcp`); `3` a turn that streamed
  and then died on a model/agent error, with an `AGY_ERROR: {...}` line on stderr (1.2.6/1.2.10,
  [C9]).
- [fact high] Startup failures still emit an envelope, with `conversation_id: ""` and zeroed usage
  (verified: bogus `--model` → `{"conversation_id":"","status":"ERROR",…}`, exit 1). A capture layer
  must treat an empty id as "no session".
- [fact high] `--print-timeout` default in 1.4.5 is **`0` = wait until the turn completes**
  (`agy --help`). Docs still say "By default, a run waits up to five minutes" ([C1]) — stale: 1.2.6
  changed the default from 5 minutes to unlimited ([C9]).
- [fact high] When `--print-timeout` does expire mid-turn, the CLI "returns the partial output it
  has and **exits successfully** with a warning on stderr" (1.1.28, [C9]). Exit 0 therefore does not imply the turn finished; the success definition must stay the caller's own diff/tests.
- [fact high] Headless runs wait for background tasks **and scheduled timers**, bounded by
  `--print-timeout` with a 30-minute cap (1.1.28 + 1.2.9, [C9]). With the unlimited default, a run
  that schedules anything can idle for a long time rather than exiting.
- [fact high] **Slash commands are consumed in print mode.** Read-only commands answer without an
  agent turn: `/config`, `/permissions`, `/hooks`, `/usage` (+1.1.11's `/quota`, `/credits`,
  `/model`, `/effort`, `/skills`, and `/help`, `/changelog`) emit tab-separated rows or a
  `command` payload under `--output-format json`, all with `num_turns: 0` (1.1.11/1.1.12, [C9];
  verified locally). Interactive-only commands fail loudly: `agy -p "/mcp"` → exit `2`,
  `"/mcp is not available in print mode (it opens the interactive MCP server view); pass
  --disable-slash-commands to send /mcp to the model as literal text"`. Any prompt beginning with
  `/` is therefore either a CLI command or an error — never plain text — unless
  `--disable-slash-commands` is passed (skill expansion in print mode since 1.1.9, [C9]).
- [fact high] `stream-json` begins with one `init` event (`cwd`, `tools`, `permission_mode`,
  optional `model`/`agent`/`json_schema`), then `step_update` events, then one `result` event with
  the same shape as the `json` envelope; tool steps carry `tool_info{name,parameters,output,error}`
  and subagent steps carry `subagent_info` with their own `conversation_id`
  ([C1]). `--input-format stream-json` keeps one process and one conversation across prompts, with
  cumulative `usage`/`num_turns` metadata ([C1]).

### 5. Session resume and prompt-cache reality

- [fact high] Headless runs are stateless by default; `--continue` (`-c`) resumes the most recent
  conversation, `--conversation <id>` resumes a specific one, and each starts a new process ([C1]).
  The capture keys (`conversation_id`, `conversationId`, `session_id`, `sessionId`) and the `--conversation` resume form match this build.
- [fact high] Gemini's caching that applies here is **implicit** caching: enabled by default on
  Gemini 2.5+, cost savings "not guaranteed", with a minimum input of **4,096 tokens** for Gemini 3.8
  Flash; explicit caching (TTL default 1 h, paid) is a separate API the CLI does not use for its
  turns ([C10](https://ai.google.dev/gemini-api/docs/generate-content/caching)).
- [inference med] There is no documented TTL for implicit caching, so a conservative warm-reuse window is the right default; the only measurable signal is the envelope's `cache_read_tokens` (per-step `usage` in `stream-json`). The policy that consumes it belongs to the companion caching research shipped with the `warmpath` crate.
- [fact high] A fresh short turn shows `cache_read_tokens: 0` and ~11.3 k input tokens (verified),
  consistent with a first-turn prefix that nothing cached yet; the 4,096-token minimum is
  comfortably exceeded by the system prompt + instructions + prompt, so a *repeat* request with an
  identical prefix is plausibly cacheable — unmeasured here by design.
- [fact high] In a **confined run** (one that cannot write `~/.gemini/antigravity-cli/`), conversation
  persistence fails: `summary store open failed (corrupt_on_open), recreating: … unable to open
  database file: no such file or directory`, `summary store recreate failed: …`,
  `failed to persist last conversation to cache: … open ~/.gemini/antigravity-cli/cache/last_conversations.json.<uuid>.tmp: operation not permitted`,
  `[Hub Summaries] failed to write summaries file in persist: … operation not permitted`. The run
  still returned a `conversation_id` and exited 0 (verified 1.4.5).
- [hypothesis med] Consequently **`--continue`/cross-process `--conversation` resume may not work in
  a confined box**, because the on-disk conversation/summary stores it reads are exactly what failed
  to open. A retry path that starts fresh on a failed resume degrades safely, but any warm-reuse plan should be treated as unproven until a two-step run on the run host shows a non-zero `cache_read_tokens` on the resumed turn.

### 6. Auth, quota, telemetry — what a CI/headless box needs

- [fact high] Three auth paths exist, and headless mode "uses your cached credentials"; with no
  terminal and no credentials it exits with an authentication error instead of hanging ([C1]).
  - **Cached interactive OAuth** — the docs' default: authenticate once with an interactive `agy`
    ([C1]). The token file lives under `~/.gemini/antigravity-cli/`; the CLI bypasses the OS keyring
    when no D-Bus session is present (headless hosts/containers) — 1.2.x/1.3.x fixes ([C9]).
  - **`GEMINI_API_KEY` key mode** — the CLI only reads `GEMINI_API_KEY` (a `GOOGLE_API_KEY` export
    must be forwarded under that name) and needs `"modelProvider": "gemini"` written into
    `settings.json` to skip interactive sign-in [C11]; the documented anchor is the install page's
    "Using a Gemini API key" section, cited by that integration. Key mode also needs
    `settings.json` writable — a real constraint in a confined container.
  - **ADC / enterprise** — `AGY_ADC_AUTH=true` authenticates from Application Default Credentials
    with no API key and no `modelProvider`; ADC supports Gemini 3 Flash and newer only [C11], and
    Business/ADC accounts never collect prompts, content or model responses (1.4.1 changelog: the
    telemetry row shows `off (disabled)` and explains why) ([C9]). **This is what the probe machine
    uses**, and it is the only path that needs no `settings.json` write.
- [fact high] Quota is observable without a model call: `agy -p "/usage" --output-format json`
  returns per-family remaining percentages and reset timestamps with `num_turns: 0` (verified:
  `Gemini Models / Weekly Limit Remaining / 88% / <iso>`, five-hour and weekly windows, plus
  per-model rows). `/quota` is an alias (1.1.11, [C9]).
- [fact high] Exhaustion is handled promptly rather than by retrying for minutes: `GEMINI_API_KEY`
  sessions stop immediately on an exhausted daily quota, project/billing spend cap, or depleted
  prepaid credits (1.2.12); a used-up plan with insufficient AI credits now says "Your AI credits
  balance is too low to continue." (1.2.15) ([C9]). The reset times are in `/usage`.
- [fact high] Telemetry: `enableTelemetry` defaults to `true` for consumer accounts and is forced
  off for Business/ADC accounts ([C2](https://antigravity.google/docs/cli/reference),
  [C9] 1.4.1). A consumer-sign-in dispatch box should set `enableTelemetry: false` (outside the
  checkout) or accept interaction collection.
- [fact high] `--remote-control` **does not support ADC auth** and "exits with a clear error"
  (1.3.3, [C9]) — on an ADC box (the likely CI shape) Remote Control is simply unavailable, and it
  should never be in a dispatch argv anyway (inbound tunnel + installed background service, 1.2.0).

### 7. Instruction files: cumulative discovery, and the 1.4.4 precedence fix

- [fact high] Rules are discovered per directory and **cumulative**, not replacement-based; more
  specific directory rules win on conflict. Loaded: `<dir>/AGENTS.md` or `GEMINI.md`,
  `<dir>/.agents/AGENTS.md|GEMINI.md`, `<dir>/.agents/rules/*.md` (legacy `.agent/rules`), plus
  global `~/.gemini/AGENTS.md`, `~/.gemini/GEMINI.md`, `~/.gemini/config/AGENTS.md|GEMINI.md`,
  `~/.gemini/config/rules/*.md`, `~/.gemini/antigravity-cli/rules/*.md`, and plugin
  `rules/` directories ([C5](https://antigravity.google/docs/rules)).
- [fact high] `AGENTS.md`/`GEMINI.md` need no frontmatter and are `always_on` for their scope; files
  under `rules/` need `trigger: always_on | model_decision | glob | manual` and are **silently
  discarded** without a valid trigger. Limits: 24,000 bytes per file (after inlining `@`-referenced label files) and a 20,000-token aggregate rules budget, past which the largest files are demoted to
  pointers ([C5]).
- [fact high] 1.4.4 fixed the precedence a thin `GEMINI.md` shim relies on: "the system prompt's user rules wording so `GEMINI.md` and `AGENTS.md` rules explicitly override earlier built-in instructions instead of only instructions that follow them" ([C9]). On ≤1.4.3 a shim that defers to `AGENTS.md` lost to earlier built-in text; on 1.4.5 it wins.
- [inference med] Workspace **trust** is state in `settings.json` (`trustedWorkspaces`, observable in
  the `/config` payload) and lives outside the checkout; workspace hooks require the folder to be
  trusted (1.1.1: hooks failed to load until "trusting a folder") ([C9]). A fresh CI checkout is
  therefore probably untrusted, which may also gate other workspace customizations. Not verified
  whether workspace `AGENTS.md`/`GEMINI.md` are trust-gated — the rules doc does not say so.
- [hypothesis med] An `AGENTS.md` that tells an agent to use a worktree and commit can conflict with a prompt that says leave changes uncommitted. Both are user-rule/prompt-level text; the docs give no precedence rule between an injected `AGENTS.md` and the user prompt, and 1.4.4 only fixes rules-vs-built-ins. An agy run may therefore commit; a caller that counts changes across the range tolerates it, but the reviewed diff shape changes. The cheapest mitigation is making the run's instructions explicitly supersede `AGENTS.md`'s worktree/commit steps.
- [fact high] `--add-dir` adds a directory to the workspace (repeatable) and therefore changes what
  is discovered/sandboxed; `--project` selects a project by ID or name; `--new-project` creates one
  ([C1]/local help). Neither flag was exercised here (they mutate account/project state).

### 8. What leaks into a dispatch: MCP servers, plugins, hooks, subagents, schedules

- [fact high] **User-level MCP servers load in headless runs.** `agy mcp list` on this box shows two enabled stdio servers, one of them a tracker MCP server. Headless/one-shot runs keep **blocking** on MCP startup "so their single scripted turn still sees the full toolset" (1.1.x fix, [C9]) — a slow or hanging server costs startup time. A run can therefore expose that server's write tools to the agent, and agy has no per-run MCP injection. Mitigation is user-level (`agy mcp disable <name>`), not per-run.
- [fact high] `agy mcp list` prints each server's full command line **including inline credentials**
  to stdout (observed; not reproduced). Never capture that output into logs, comments, or a research
  note; treat it like a secret-bearing command.
- [fact high] Plugin surface: `agy plugin list --json` → `[]` here. Plugins can ship skills, rules,
  subagents, MCP servers, hooks and sidecars, staged under `~/.gemini/antigravity-cli/plugins/`
  ([C6](https://antigravity.google/docs/cli/features)); a plugin load failure is not fatal to a run
  but silently changes the toolset (1.4.3/1.4.4/1.4.5 all fix plugin-load edge cases).
- [fact high] Subagents: headless `-p` runs **skip** subagent questions "so the subagent can
  continue" (1.3.6) — good: a dispatch cannot hang on a subagent approval. Subagents inherit parent
  plugins (1.4.4 fix).
- [fact high] Scheduling: the agent's `schedule` tool creates one-shot timers and recurring cron
  schedules (1.4.5 changed recurring cron to fire at a per-schedule offset rather than exactly on the
  boundary). Headless runs wait for scheduled timers up to the `--print-timeout`/30-minute cap
  (1.1.28, 1.2.9) ([C9]).
- [fact high] Hooks: `hooks.json` (plugin, `~/.gemini/config/hooks.json`, or workspace
  `<workspace>/.agents/hooks.json` — the last added 1.1.1) run shell commands before/after tool
  calls; `/hooks` returns `{"hooks":[]}` on this box (0 tokens). Hooks are a deterministic lever — and a supply-chain surface if a plugin ships them.
- [inference low] Sidecars appear only in changelog prose (`sidecar.json`, sidecar manifests,
  1.2.7/1.4.3/1.4.4) — no docs page was found; treat as internal/beta and assume a plugin-only
  feature with no documented headless semantics.
- [fact high] Remote Control (`--remote-control`, `remote-control start|status|stop`, 1.2.0/1.2.6;
  `mic-serve` streams a microphone to a CLI on another host) is TUI/remote-interaction surface: no
  place in a headless dispatch, and unsupported under ADC (1.3.3). The artifact viewer is TUI-only.
- [fact high] MCP `_meta` conversation ids: 1.4.4 added
  `_meta["antigravity.google/root_conversation_id"]` to `tools/call` requests so MCP servers see the
  top-level conversation id even from nested subagents; 1.3.3 added
  `_meta: {"antigravity/alwaysLoad": true}` for always-loaded tools. Useful only if an MCP-aware
  proxy wants to correlate calls with `conversation_id`.

## Experimental surface table

| Feature | How enabled | Status in 1.4.5 | Dispatch risk | Source |
|---|---|---|---|---|
| Auto mode | `--mode auto`, `/config` Agent Mode, `Shift+Tab`, Remote Control settings | Shipped, OS/account-gated (Linux/macOS 1.3.5; Windows 11 24H2 personal 1.4.4) | LLM-reviewed commands still prompt for risky ones; under `--dangerously-skip-permissions` largely moot | [C9], local help |
| Agent modes `accept-edits`/`plan` | `--mode` | Shipped but **undocumented**; typo warns and continues | Silently wrong mode; verify via `init.permission_mode` only | local help, [C9] 1.1.12 |
| `--sandbox` + `enableTerminalSandbox` | flag or `settings.json` | Shipped, `proceed-in-sandbox` preset preview | Blocks network by default; bypass needs approval → breaks builds | [C4], [C3] |
| Remote Control | `--remote-control`, `/remote-control`, `remote-control start/status/stop`; `mic-serve` | Shipped (daemon 1.2.0) | Inbound tunnel + installed OS service; **unsupported under ADC** (1.3.3) | [C9], local help |
| Plugins | `agy plugin install/enable/list --json/validate`, `/plugin`, marketplaces | Shipped; empty on this box | User-level skills/rules/hooks/MCP silently change toolset; load failures non-fatal | [C6], [C9] |
| Subagents | implicit; `--agent`, `/agents`, `@<subagent>` | Shipped | Headless skips their questions (won't hang); inherit parent plugins | [C6], [C9] 1.3.6/1.4.4 |
| Scheduling tool | agent `schedule` (one-shot timers, cron, `@every`) | Shipped (cron offset change 1.4.5) | Headless waits for timers up to print-timeout/30-min cap | [C9] 1.1.28/1.2.9/1.4.5 |
| Hooks | `hooks.json` (plugin/global/workspace `.agents/`) | Shipped, undocumented in CLI docs | Shell commands in-run; determinism lever or supply-chain risk | [C9] 1.1.1/1.2.3/1.2.4 |
| skills/rules discovery | `AGENTS.md`/`GEMINI.md`, `.agents/rules/`, `rules/*.md`, plugin rules | Shipped | 24 KB/file + 20k-token caps; silent discard on bad frontmatter | [C5] |
| `/config` & read-only slash cmds in print mode | `-p "/config"` etc. (+`--output-format json`) | Shipped (1.1.11/1.1.12) | None — 0-token introspection; exposes user paths (`trustedWorkspaces`) so don't log wholesale | [C9], verified |
| `--input-format stream-json` | flag pair with `--output-format stream-json` | Shipped (1.1.15) | Long-lived process; not needed for one-shot dispatch | [C1], [C9] |
| MCP `_meta` ids | server-side `tools/call` metadata | Shipped (1.3.3 `alwaysLoad`, 1.4.4 root conversation id) | Only relevant with an MCP proxy | [C9] |
| `ListExperiments` rollout | server-side poll at startup | Present, not user-toggleable | Capability drifts between days on the same binary | observed locally |
| `experimental` settings namespace | `settings.json` | Unknown in 1.4.5 (existed 1.1.x) | Unpinnable feature gates | [C11] |
| `--log-file`, `--json-schema`, `--print-timeout`, `--disable-slash-commands` | flags | Shipped, partially undocumented | See §1–§8 | local help, [C1] |

## Permission matrix for print mode

Effective posture for a headless `-p` run, by configuration (sources: [C1] headless docs,
[C3]/[C2] settings, [C4] sandbox, [C9] changelog, plus local probes).

| Configuration | Edits in workspace | Shell commands | Network/URL reads | Still denied/prompted | Exit code on denial | Notes |
|---|---|---|---|---|---|---|
| default (`request-review`, no flag) | auto-allowed (reads/writes inside active workspace) | **soft-denied** | soft-denied | everything needing approval | `0` + stderr notice + `denied_actions` | the "exit 0, nothing changed" mode |
| `permissions.allow` rules (`command(git)`, `write_file(src/)`, `regex:` forms) | per rule | per rule | per `read_url` rule | anything not matched | `0` | rules come from project/shared/global scopes, the user home included |
| `toolPermission: "strict"` | prompts → soft-denied | soft-denied | soft-denied | all non-read tools | `0` | strictly worse than default for dispatch |
| `toolPermission: "always-proceed"` | allowed | allowed | allowed | nothing (unless `deny` rules) | `0` | same posture as the flag below, via settings |
| `--dangerously-skip-permissions` | allowed | allowed | allowed | `deny` rules only | `0` | `init.permission_mode: "always-proceed"` |
| `--mode accept-edits` | allowed | allowed for routine/safe, prompts for destructive | allowed | destructive/irreversible actions | `0` (denied) | undocumented flag; typo warns and continues |
| `--mode auto` | allowed | allowed + separate reviewer for sandbox-escaping commands | allowed | risky commands prompt (soft-denied headless) | `0` | needs a model-driven reviewer step per risky command |
| `--mode plan` | none (plan first) | — | — | all execution | `0` | non-interactive runs auto-proceed through plan review (1.1.x fix) |
| `--sandbox` (+ any of the above) | workspace + temp + build caches | inside OS sandbox (`sandbox-exec`/`nsjail`) | **none unless `read_url` allowlisted** | anything needing net or outside paths | `0` (denied/failed) | bypass requests need approval → dead in headless |
| launch/validation error (bad `--model`, `--agent`, `--json-schema`, bad `--effort`) | — | — | — | — | `1` (JSON `status: ERROR`, `conversation_id: ""`) | no session captured |
| usage error (`--print` + `--prompt-interactive`, `/mcp` in print) | — | — | — | — | `2` | slash commands are intercepted |
| turn dies after streaming | — | — | — | — | `3` | `AGY_ERROR: {...}` on stderr; partial response in JSON |

## Prompt cache, resume and cost (surface facts)

- **Capture and resume.** A parser reads the first of `conversation_id|conversationId|session_id|
  sessionId` from the JSON result and resumes with `--conversation <id>`; both are correct for 1.4.5
  ([C1]; verified envelope contains `conversation_id`).
- **What the provider caches.** Implicit caching only, on by default, savings not guaranteed,
  minimum 4,096 input tokens on Gemini 3.8 Flash ([C10]). No TTL is published; the cache-policy
  analysis belongs to the companion caching research shipped with the `warmpath` crate.
- **Only measurable signal:** `usage.cache_read_tokens` in the result envelope and per-step `usage`
  in `stream-json` ([C1]).
- **Measured on 1.4.5 here:** fresh short turn → `input_tokens 11261, cache_read_tokens 0,
  output_tokens 28, thinking_tokens 19, duration 3.86 s`, i.e. ~11.3 k of fixed prefix for a trivial
  prompt. That exceeds the 4,096 minimum, so prefix reuse is *possible* — not demonstrated.
- **Cost caveat:** the CLI sends no cache-control fields; it relies on the provider noticing a
  repeated prefix. Whether a resumed `--conversation` run reuses it depends on how the CLI
  re-serialises history, which is not observable from outside.
- **Confined-run caveat:** if the box cannot write `~/.gemini/antigravity-cli/`, conversation
  persistence fails (see §5), so resume may silently become "fresh".

## Failure modes and practitioner reports (UGC labelled)

Confined-run and environment findings observed on 1.4.5 (facts about *this* sandbox, hypotheses
about a CI box):

- **Writes to `~/.gemini/antigravity-cli/` are denied → three distinct errors, all non-fatal.**
  `E … main.go:726] Failed to redirect output for CLI: creating log file: opening log file: open
  ~/.gemini/antigravity-cli/log/cli-<ts>.log: operation not permitted`;
  `E … server.go:1688] Failed to initialize crash reporter: … open
  ~/.gemini/antigravity-cli/crashes/crash_<pid>_<uuid>.log: operation not permitted`;
  and the summary/cache persistence failures quoted in §5. Consequence: **no CLI log file and no
  crash reports** on a locked-down runner (the default log path is not writable), plus a noisy
  stderr that a naive wrapper may mistake for failure. `--log-file` exists in `--help` (undocumented
  in the docs) to redirect the first one.
- **Unauthenticated-for-the-consumer-path startup is normal on ADC.** Six to ten
  `You are not logged into Antigravity` / `Failed to poll ListExperiments` / `Cache(loadCodeAssistResponse)`
  errors precede a successful run. Do not treat that stderr as fatal; check `status`/exit code.
- **`--mode` typo is a warning, not an error** (verified). A silent fallback to the default mode in
  a long-lived deployment is a real risk if the token is templated.
- **`--effort` vocabulary is `low|medium|high|xhigh|max`** (verified error text:
  `invalid --effort "s" (valid: low, medium, high, xhigh, max)`), plus a per-model gate:
  `--effort is not supported for model "<model>"`. The docs' "low, medium, or high" is stale
  ([C1] vs local help).
- **Unknown `--model` fails loudly in print mode** (`status: ERROR`, exit `1`, lists available
  models) — deliberate, and the opposite of interactive fallback ([C1], verified).
- **`--agent` with no matching agent exits 1 and lists agents** (1.3.4 fix, [C9]); locally
  `agy agents` printed no agents at all (0 agents available), so `--agent` is unusable on this box.
- **`--cwd` does not exist** despite the official best-practices page using it:
  `flags provided but not defined: -cwd` (exit 2). Child `cwd` is the correct mechanism.
- **`agy mcp list` emits inline MCP credentials** to stdout (observed). See §8.
- **[hypothesis med] Practitioner integration (harbor PR #2776, agy 1.1.x — dated, verify before
  relying):** another harness drives agy headless and reports: key mode requires
  `"modelProvider": "gemini"` in `settings.json`; `GOOGLE_API_KEY` must be renamed to
  `GEMINI_API_KEY`; `AGY_ADC_AUTH=true` selects ADC; ADC supports Gemini 3 Flash and newer only;
  they raised the print ceiling to 24 h because "headless agy gives up on a `--prompt` run after 5
  minutes by default"; `--add-dir` anchors the workspace to the invocation cwd; built-in models must
  **not** be registered under `customModelsConfig` because a registered built-in routed through the
  custom-model path returned HTTP 400 `INVALID_ARGUMENT` on agy 1.1.14
  ([C11](https://github.com/harbor-framework/harbor/pull/2776)). The 5-minute default contradicts
  1.4.5's unlimited default (changed in 1.2.6) — an example of 1.1.x advice going stale.
- **No practitioner UGC (Reddit/HN/forums) was needed to establish any claim above**; the two
  third-party sources consulted are a repo PR and a migration discussion snippet, both labelled.
  Reddit/HN claims about "agy ignores its flags" or "quota resets unreliably" were not verified and
  are deliberately omitted.

## Contradictions and version drift

| Claim | Where it says so | What 1.4.5 actually does | Verdict |
|---|---|---|---|
| `--print-timeout` default `5m` | [C1] flag table and prose ("waits up to five minutes") | `agy --help`: default `0s`, "0 waits until the turn completes"; 1.2.6 changelog changed 5 min → unlimited | **docs stale** (public docs predate 1.2.6) |
| `--effort` is `low, medium, high` | [C1] flag table | help: `low|medium|high|xhigh|max`; error text confirms all five; per-model support varies | **docs stale** |
| `--mode` | not in the docs flag reference at all | present in `--help`, announced in 1.3.5/1.4.4 changelog, validated with a warning | **docs incomplete** |
| `--cwd <dir>` | [C7] best-practices example | `flags provided but not defined: -cwd`, exit 2 | **docs wrong** for CLI (may be an App/IDE flag) |
| agy "gives up after 5 minutes" | [C11] (1.1.x integration) | unlimited by default since 1.2.6 | **third-party advice dated** |
| `experimental: {skills: true}` in settings | [C11] (1.1.x) | not a `/config` key in 1.4.5; not observable | **unverified / likely superseded** |
| Auto mode availability | 1.3.5 "Linux and macOS"; 1.4.4 "Windows 11 24H2+, personal Google Accounts" | `--mode auto` accepted on this macOS host with a Business/ADC account | **consistent with OS gating; account gating untested** |
| `agy models` catalog | [C1] example list | identical slugs locally (`gemini-4-argon-{low,medium,high}`, `gemini-3.8-flash-*`, `claude-*-5-5-*`, `gpt-oss-120b-medium`) | **no drift** |
| Remote Control + ADC | 1.3.3 says unsupported, exits with a clear error | not exercised here (refused by constraint) | **assumed current** |

## Gaps / could not verify

- **Effective permission mode under `--dangerously-skip-permissions`** is not observable without a
  turn: `/config` reports persisted settings only. `init.permission_mode` in a `stream-json` run is
  the documented probe ([C1]) but running it needs a billed turn — one calibration run on the run host would settle it.
- **`ListExperiments` payload** (which flags exist, how they map to Auto mode/plugins/Remote
  Control) — the endpoint only answers for an authenticated Antigravity account, which this box is
  not (ADC only). The local effect (a poller that fails and is logged) is all that could be
  established.
- **Whether the `experimental` settings namespace still gates anything** in 1.4.5.
- **Implicit-cache TTL / whether a resumed `--conversation` turn reuses the prefix** — no documented
  TTL, and confirming it needs two billed runs; not done.
- **Whether resume is actually broken in confined runs** (the persistence errors are facts; the
  behavioural consequence is an inference).
- **Workspace-trust gating of workspace `AGENTS.md`/`GEMINI.md`/hooks** — trust state
  (`trustedWorkspaces`) is in `settings.json` outside the checkout; the rules doc does not say
  whether discovery is gated by it.
- **Project-scope permission rules: storage location** — `/permissions` exposes a "project" scope
  ([C8]) but no doc states the file it lives in, so a checkout-local permission file (which would let a run drop `--dangerously-skip-permissions`) cannot be engineered yet.
- **Sidecars, hooks event names/matchers, `--add-dir`/`--project` discovery effects, plugin
  interactions** — changelog-level only; not exercised (would mutate account/project state or
  require a plugin install).
- **`agy agents` returned no agents** on this box (empty stdout, exit 0), so custom-agent/
  subagent surface is unexercised for dispatch.
- **No live dispatch was attempted** (prohibited): every claim about how agy behaves *inside a non-interactive run* is inference from the CLI contract, not a measured run.

## Sources (accessed 2026-10-22, with version/commit where known)

External:

1. [C1] Google Antigravity Docs — **Headless mode**, https://antigravity.google/docs/cli/headless
   (page does not state a version; describes `--output-format`, statuses, exit codes, resume,
   `stream-json`, permissions in headless mode). Retrieved 2026-10-22; two claims (5-minute default
   timeout, `low|medium|high` effort) are stale for 1.4.5.
2. [C2] Google Antigravity Docs — **CLI commands reference** (slash commands, keybindings,
   `settings.json` keys), https://antigravity.google/docs/cli/reference. Retrieved 2026-10-22.
3. [C3] Google Antigravity Docs — **Settings** (`~/.gemini/antigravity-cli/settings.json`,
   `toolPermission` values, `enableTerminalSandbox`, `allowNonWorkspaceAccess`,
   `artifactReviewPolicy`, telemetry), https://antigravity.google/docs/settings. Retrieved
   2026-10-22.
4. [C4] Google Antigravity Docs — **Terminal sandbox** (`--sandbox`, `sandbox-exec`/`nsjail`,
   network default-off, `unsandboxed(...)` rules), https://antigravity.google/docs/sandbox.
   Retrieved 2026-10-22.
5. [C5] Google Antigravity Docs — **Rules** (`AGENTS.md`/`GEMINI.md`/`.agents/rules`, precedence,
   frontmatter, 24 KB and 20k-token budgets, CLI global locations), https://antigravity.google/docs/rules.
   Retrieved 2026-10-22.
6. [C6] Google Antigravity Docs — **Antigravity CLI features** (plugins staging layout, terminal
   sandbox settings, subagents and `/agents`), https://antigravity.google/docs/cli/features.
   Retrieved 2026-10-22.
7. [C7] Google Antigravity Docs — **CLI best practices** (permission presets, `-p` example using
   `--cwd`), https://antigravity.google/docs/cli/best-practices. Retrieved 2026-10-22.
8. [C8] Google Antigravity Docs — **`/permissions` command** (project/shared/global scopes,
   allow/deny/ask lists), https://antigravity.google/docs/cli/commands/permissions. Retrieved
   2026-10-22.
9. [C9] **antigravity-cli CHANGELOG** (`main`; newest entry `1.4.5`, matching the installed build),
   https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md (raw:
   https://raw.githubusercontent.com/google-antigravity/antigravity-cli/main/CHANGELOG.md).
   Retrieved 2026-10-22. Versions cited inline: 1.1.1, 1.1.3, 1.1.5, 1.1.9–1.1.12, 1.1.15, 1.1.19,
   1.1.20, 1.1.27, 1.1.28, 1.2.0, 1.2.3, 1.2.4, 1.2.6, 1.2.7, 1.2.9, 1.2.10, 1.2.12, 1.2.14, 1.2.15,
   1.3.3, 1.3.4, 1.3.5, 1.3.6, 1.4.0–1.4.5.
10. [C10] Google AI for Developers — **Context caching** (implicit caching on Gemini 2.5+, no saving
    guarantee, 4,096-token minimum for Gemini 3.8 Flash; explicit caching TTL default 1 h),
    https://ai.google.dev/gemini-api/docs/generate-content/caching. Retrieved 2026-10-22.
11. [C11] harbor-framework/harbor **PR #2776** — "feat(antigravity): agy 1.1.x support — headless
    API-key/ADC auth, workspace-anchored runs, ATIF trajectories", diff read at
    https://patch-diff.githubusercontent.com/raw/harbor-framework/harbor/pull/2776.diff (PR page
    https://github.com/harbor-framework/harbor/pull/2776). Third-party integration; **agy 1.1.x**,
    so its CLI-behaviour claims are treated as dated hypotheses and cross-checked against [C1]/[C9].

Local (the version that matters for this checkout):

12. `agy` 1.4.5 — `--version`, `--help`, `agy changelog`, `agy mcp|plugin|agents|models|
    remote-control|install|mic-serve --help`, `agy models`, `agy mcp list`, `agy agents`,
    `agy plugin list --json`; read-only print-mode probes
    (`-p "/config"|"/permissions"|"/hooks"|"/usage"|"/mcp"` with `--output-format json`) and
    validation-failing probes (`--model does-not-exist`, `--mode bogus`, `--effort s|max|minimal`,
    `--cwd .`). Logs and stderr captured as quoted.
13. A repository checkout's `GEMINI.md` and root `AGENTS.md`.

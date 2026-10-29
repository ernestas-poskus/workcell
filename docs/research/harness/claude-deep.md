# Claude Code — experimental features and headless dispatch practice

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

## Scope and versions checked

- **Installed CLI (ground truth for this checkout):** `claude --version` → `2.1.303 (Claude Code)`; `claude doctor` adds native install, commit `07df2aedd2d0`, `darwin-arm64`, auto-updates *enabled*, last update success → 2.1.303 (2026-10-19). Access date 2026-10-22. [fact|high: local run]
- **Local `--help`** captured in full (311 lines) plus `claude mcp --help`, `claude plugin --help`, `claude agents --help`, `claude doctor --help`. Flags cross-checked against the docs CLI reference (see Contradictions).
- **Repository checkout read (2026-10-22):** `.claude/settings.json`, `.mcp.json` (**key names only**, no values), `CLAUDE.md`.
- **Upstream:** `code.claude.com/docs` Markdown endpoints (`*.md`) and `github.com/anthropics/claude-code` `CHANGELOG.md`, which at access time had released **2.1.308** — i.e. the checkout's 2.1.303 is five patch releases behind (drift section).
- **Not done:** no live model runs (no billed run), no install/update, no read of `~/.claude` internals, keychain state, environment files, or credentials.

---

## Findings (by theme)

### 1. `--betas` and `anthropic-beta`

- `--betas <betas...>` is documented by the CLI itself as "Beta headers to include in API requests (**API key users only**)" [fact|high: local `--help`; cli-ref]. Docs give one concrete value form: `claude --betas interleaved-thinking` [fact|high: cli-ref].
- The flag is a thin wrapper over the `anthropic-beta` request header; Claude Code already sends the betas it needs, and `--betas` exists to opt into an Anthropic API beta *before native support* [fact|high: env-vars `ANTHROPIC_BETAS` row].
- `ANTHROPIC_BETAS` (comma-separated) does the same but **works with every auth method, including claude.ai subscription**, unlike `--betas` [fact|high: env-vars]. `CLAUDE_CODE_EXTRA_BODY` merges a JSON object into the request body for provider fields with no flag [fact|high: env-vars].
- Beta *pairs* matter: body fields travel with a header (context management, beta tool fields such as `defer_loading`, structured outputs/`output_config`, effort-per-turn). Stripping one half produces `400`s that Claude Code sometimes retries without the pair [fact|high: llm-gateway-protocol §Beta header and body field pairs, §Automatic retry].
- `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1` is the documented squash switch: it removes context management, beta tool fields (incl. MCP tool search `defer_loading`), `output_config.format`/`task_budget`, but keeps extended-context, interleaved-thinking and effort betas, the OAuth beta subscription auth needs, and explicitly added values [fact|high: llm-gateway-protocol §Disable pre-release capabilities].
- Behind a custom `ANTHROPIC_BASE_URL` (a gateway topology) the practical requirement is **forward `anthropic-beta` verbatim**; don't allowlist values, "the set changes with Claude Code releases" [fact|high: llm-gateway-protocol §Request headers]. Changelog 2.1.307 fixed "the `prompt-caching-scope` beta header being sent behind custom `ANTHROPIC_BASE_URL` gateways … where it has no effect" — i.e. 2.1.303 *does* send that header to the gateway [fact|high: CHANGELOG 2.1.307].
- **For a dispatch:** set no betas. If a capability is needed, prefer `ANTHROPIC_BETAS` (auth-agnostic) and make sure the gateway forwards the header and `cache_control` unchanged; the CLI's own betas are not something a dispatch should curate [inference|high].

### 2. Experimental/preview surface in this build

- **Agent teams** — `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`, experimental and disabled by default; **in `-p` Claude never spawns teammates**, a named subagent runs as an ordinary subagent [fact|high: agent-teams, env-vars]. Enabling it changes ordinary delegation even interactively, so a dispatch gains nothing and loses determinism [inference|high].
- **Plugins** — installed plugins and the official marketplace auto-install are discovered at startup in a normal `-p` run; only `--bare`/`--safe-mode` skip them [fact|high: headless §bare mode, cli-ref]. Plugin *command* sources and marketplace installs run local commands / network I/O; `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` stops command-source reruns [fact|high: env-vars row]. A plugin can also keep a headless run alive past `--max-turns` — fixed in 2.1.304, i.e. **not** in the installed 2.1.303 [fact|high: CHANGELOG 2.1.304].
- **Skills / commands** — auto-discovered in `-p`; `--disable-slash-commands` disables all skills, `--bare` skips discovery [fact|high: local help; headless]. Skills can carry `allowed-tools` and frontmatter hooks that workspace trust never gates [fact|high: permissions §What runs before you trust a folder].
- **Hooks** — project/user hooks run in `-p`, including in a folder never trusted; `--bare`/`--safe-mode` skip settings/plugin hooks but built-in behavior is unaffected [fact|high: headless; local help]. Project settings can re-enable what user settings turn off, so the reliable off-switch is the flag: `--settings '{"disableAllHooks": true}'` [fact|high: permissions §What runs before you trust a folder].
- **Subagents** — available headless; `--forward-subagent-text` (stream-json only) and `--append-subagent-system-prompt[-file]` (docs ≥2.1.205/2.1.261) exist in docs but are absent from local `--help`; subagents get their own 5-minute cache bucket [fact|high: cli-ref; prompt-caching].
- **Background sessions (`--bg`)** — **rejected with `-p`** ("Claude Code rejects `--bg`") [fact|high: headless §Basic usage]. They need workspace trust and a supervisor; unrelated to a dispatch [fact|high: agent-view].
- **Remote Control** — interactive/conversation-owner feature; no `-p` mode [fact|high: cli-ref].
- **Auto permission mode** — `--permission-mode auto`: a classifier reviews calls instead of prompting; availability depends on feature-flag fetching, model support, provider, and org policy [fact|high: permission-modes; env-vars]. It is *not* a determinism guarantee, and it is unavailable in sessions that don't fetch feature flags (e.g. `DISABLE_TELEMETRY`, third-party providers) [fact|high: permission-modes §How auto mode falls back / headless]. `claude auto-mode defaults` prints the rules as JSON [fact|high: cli-ref].
- **Sandboxing** — OS-enforced Bash boundary, **off by default**; on only via settings `sandbox.enabled` (user or managed) [fact|high: sandboxing §What the sandbox restricts]. If host settings enable it, a non-bare `-p` run inherits it; blocked hosts are reported to Claude, which may retry with `dangerouslyDisableSandbox` — so the sandbox is a guardrail, not a hard boundary [fact|high: sandboxing §The unsandboxed retry escape hatch]. `autoAllowBashIfSandboxed` defaults true; `sandbox.allowNestedUserNamespaces` (added 2.1.305) even lets sandboxed commands create user namespaces and get around file protections [fact|high: sandboxing; CHANGELOG 2.1.305].
- **`--brief`** — present in local help: "Enable SendUserMessage tool for agent-to-user communication" [fact|high: local help]. It is **not documented** on the pages checked (cli-reference/headless), so its exact headless semantics are unverified [gap]. Adding a tool definition changes the request's tool set → invalidates a warm cache prefix and gives the model a mid-run user channel; it is the wrong default for a one-shot run [inference|med].
- **Chrome (`--chrome`)** — browser integration; needs a browser/extension and adds tools; irrelevant and risky in a headless dispatch [fact|high: local help; cli-ref].
- **Cloud / environments (`--cloud`, `--environment`)** — `--cloud` with a task description is rejected by `-p`; `--cloud` with a session id only queues a message; `--environment` creates cloud sessions on a self-hosted environment [fact|high: headless; cli-ref]. Not a local dispatch path.
- **`--bare`** — minimal mode: skips hooks, skills, custom commands, subagents, installed plugins, MCP auto-discovery, auto memory, and CLAUDE.md; sets `CLAUDE_CODE_SIMPLE=1`; Anthropic auth is **strictly `ANTHROPIC_API_KEY` or an `apiKeyHelper` via `--settings`** (OAuth/keychain never read); no system reminders; no background tasks; no automatic system-prompt recording unless `--system-prompt-snapshot on` [fact|high: local help; headless §Start faster with bare mode; cli-ref §System prompt flags]. Docs: "`--bare` is the recommended mode for scripted and SDK calls, and will become the default for `-p` in a future release" [fact|high: headless].
- **`--safe-mode`** — disables customizations (CLAUDE.md, skills, plugins, hooks, MCP, auto memory, LSP, …) but **auth, model selection, built-in tools and permissions work normally**; managed policy still applies; sets `CLAUDE_CODE_SAFE_MODE=1` [fact|high: local help; env-vars]. This is the subscription-compatible sibling of `--bare`.
- **`--restricted`** — removes command/code-running tools and WebFetch unless named in `--tools`, confines file tools to working dirs, loads only managed settings and `--settings`, and **refuses `bypassPermissions`** [fact|high: local help; cli-ref]. It is therefore *incompatible* with a full-permission non-interactive run by design.

### 3. `CLAUDE_CODE_*` and other environment knobs

Beyond `ANTHROPIC_BASE_URL` (the gateway route), the environment knobs relevant to a non-interactive run are:

| Variable / setting | Effect | Dispatch relevance |
|---|---|---|
| `ENABLE_TOOL_SEARCH` | Unset or `true` defers MCP tool definitions; `auto`/`auto:N` thresholds; `false` loads all tools upfront [fact\|high: env-vars] | A caller behind a gateway may set `true`. Tool-search on/off changes whether an MCP-server change invalidates the prefix, and a gateway that rejects `defer_loading` retries without tool search (2.1.305+); 2.1.303 has no such retry [fact\|high: llm-gateway §Retry without tool search; CHANGELOG] |
| `CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING` | Off by default behind a custom base URL/Foundry/gateway; `1` forces it on [fact\|high: env-vars] | Behind a gateway, large tool inputs (long file writes) arrive only at the end without it, which can look like a hang [fact\|high: env-vars] |
| `CLAUDE_CODE_SAFE_MODE` / `CLAUDE_CODE_SIMPLE` | Programmatic equivalents of `--safe-mode` / `--bare` [fact\|high: env-vars] | Use the flags; env can be inherited by children and is easier to leak |
| `CLAUDE_CODE_DISABLE_AUTO_MEMORY` / `CLAUDE_CODE_DISABLE_CLAUDE_MDS` | Turn auto memory off / stop loading any CLAUDE.md [fact\|high: env-vars] | Auto memory is host state written across runs and part of the system prompt; a dispatch should not accumulate it |
| `CLAUDE_CODE_SKIP_PROMPT_HISTORY` / `--no-session-persistence` | Skip transcripts on disk / session cannot be resumed (print only) [fact\|high: env-vars; cli-ref] | **Do not** combine with a retry/warm-resume path: no transcript = no resume |
| `CLAUDE_CODE_PROMPT_CACHE_TTL` (`5m`\|`1h`), `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL`, `promptCacheTtl` | Choose the main-conversation / non-main cache TTL (≥2.1.242) [fact\|high: env-vars; settings-ref] | Determines the TTL a warm-reuse policy must assume (see §5) |
| `DISABLE_PROMPT_CACHING[_SONNET/_OPUS/...]`, `FORCE_PROMPT_CACHING_5M`, `ENABLE_PROMPT_CACHING_1H` | Cache disable / TTL overrides [fact\|high: env-vars] | Never disable; `FORCE_PROMPT_CACHING_5M` is a debugging aid |
| `MAX_THINKING_TOKENS` | Fixed thinking budget; `0` disables thinking where the model allows; adaptive-reasoning models choose their own depth when unset [fact\|high: env-vars] | The dispatch pins no effort today; leaving it unset lets the model choose. Setting `0` changes cost/latency but not on Sonnet 5.5/Opus 5.5-class models |
| `BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS` | Foreground command default 120 s (max 600 s the model may request); over the default background limit in unattended sessions (≥2.1.285) [fact\|high: env-vars] | Build and test commands can exceed 120 s; a timeout moves the command background and interacts with the 10-min exit wait |
| `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` | Idle wait for background work after the final `-p` turn; default 600000; `0` waits forever [fact\|high: env-vars; headless §Background tasks at exit] | Default can hold a dispatch open ten minutes after the model is done; set a bounded value (and `--max-budget-usd` stops remaining background work) |
| `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS` | Disables `run_in_background`, auto-backgrounding, Ctrl+B [fact\|high: env-vars] | The most deterministic setting for a one-shot dispatch |
| `--autocompact auto\|<tokens>`, `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | Auto-compact window / trigger threshold [fact\|high: cli-ref; env-vars] | A compaction mid-dispatch replaces history and invalidates the cache; with a fixed prompt, a known window is a cost control |
| `DISABLE_AUTOUPDATER`, `DISABLE_UPDATES` | Stop background auto-updates / all updates [fact\|high: env-vars] | An upgrade changes the system prompt and rebuilds every cache from the top [fact\|high: prompt-caching] |
| `DISABLE_TELEMETRY`, `DO_NOT_TRACK`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` | Opt out of telemetry/nonessential traffic; also disables **feature-flag fetching** [fact\|high: env-vars] | Feature-flag fetch affects things like auto mode availability and Remote Control; some are unavailable once off [fact\|high: env-vars §features that need feature-flag fetching] |
| `MCP_TIMEOUT` (default 30000), `MCP_CONNECT_TIMEOUT_MS` | MCP startup/connect bounds [fact\|high: env-vars] | With `--mcp-config`, `-p` waits for still-pending servers before the first turn [fact\|high: headless] |
| `CLAUDE_CODE_EXTRA_BODY`, `ANTHROPIC_BETAS` | Extra request body / beta headers [fact\|high: env-vars] | Gateway escape hatches, not dispatch defaults |

Two semantics traps: `DISABLE_TELEMETRY` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` are **disabled by any value, including `0`/`false`** — unset the variable to re-enable [fact|high: env-vars]. And `--setting-sources`/`--settings`/env precedence means a repository's project settings can override a user-level opt-out [fact|high: permissions §What runs before you trust a folder].

### 4. Reproducibility: what leaks into a `-p` run

Order of isolation strength, from local ground truth + docs:

1. **`--bare`** — the strongest. No hooks, skills, custom commands, subagents, plugins, MCP auto-discovery, auto memory, CLAUDE.md; no OAuth/keychain reads; no system reminders; no background tasks. Cost: **Anthropic auth must be `ANTHROPIC_API_KEY` or `--settings` `apiKeyHelper`**; and system-prompt recording is off unless `--system-prompt-snapshot on` [fact|high: headless; cli-ref §System prompt flags].
2. **`--safe-mode`** — removes customizations but keeps normal auth, model, tools and permissions; managed policy still applies [fact|high: local help; env-vars]. The pragmatic choice for a subscription-authenticated dispatch.
3. **Targeted isolation** — `--setting-sources user` (drop project/local settings and `.mcp.json`), `--strict-mcp-config` (only `--mcp-config` servers), `--settings '{"disableAllHooks": true}'`, `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` [fact|high: cli-ref; permissions; headless]. Note that `--bare` already implies most of this; mixing them is belt-and-braces.

What *does* leak without any of the above, in a `-p` run in a repository:

- User + project + local settings files, hence hooks, `env` block, helper commands [fact|high: permissions].
- Installed plugins, marketplace state, skills/custom commands/subagents, auto memory, CLAUDE.md up the tree [fact|high: headless].
- `.mcp.json`: **"Connected without asking, approved or not"** in a `claude -p`/SDK run in a folder never trusted; `claude mcp list` still reports them pending [fact|high: permissions §What runs before you trust a folder].
- Workspace trust is never shown in `-p`; project `permissions.allow`/`additionalDirectories` are **not** applied and a `this workspace has not been trusted` warning goes to stderr [fact|high: permissions; cli-ref `-p` note].
- Keychain/OAuth reads (unless `--bare`), git status snapshot, cwd/platform/shell info in the system prompt, auto-updater state [fact|high: prompt-caching §Cache scope; headless].

**The repository hazard:** a gitignored `.mcp.json` can declare servers that a `-p` run connects without asking [fact|high: docs]. Passing only `--mcp-config` does **not** exclude them; by the documented rule they can still connect and add tools (and tool definitions change the cached prefix), unless `--strict-mcp-config` is also passed [inference|high from fact|high docs; not empirically verified in 2.1.303 — see Gaps].

### 5. Prompt cache, resume and cost

- Request order is system prompt → project context → conversation; a change anywhere earlier invalidates everything after it [fact|high: prompt-caching §How the cache is organized]. Prefix is machine- and directory-scoped: the system prompt embeds auto-memory paths and the conversation opens with cwd/platform/shell/OS; sequential sessions share a prefix only when the git-status snapshot taken at startup matches [fact|high: prompt-caching §Cache scope].
- **`--exclude-dynamic-system-prompt-sections`** moves per-machine sections (cwd, env info, memory paths, git status) from the system prompt into the first user message, improving cross-user/cross-machine prompt-cache reuse; ignored when `--system-prompt` is set [fact|high: local help; cli-ref].
- **`--system-prompt-snapshot on` (default)** records the prompt rendered on the first request and reuses it verbatim on every later request and on `--resume`, until compaction; `off` rebuilds every request. **In `--bare`, recording stays off unless you pass `--system-prompt-snapshot on`** [fact|high: cli-ref §System prompt flags / in resumed conversations].
- **TTL:** the per-request cache lifetime is set with `promptCacheTtl` or `CLAUDE_CODE_PROMPT_CACHE_TTL` (`5m`/`1h`, ≥2.1.242) [fact|high: env-vars], and the default differs by auth mode. The policy that consumes this belongs to the companion caching research shipped with the `warmpath` crate; this note records only that the knob exists.
- **Invalidators that matter for a retry:** model switch, effort change, MCP server set change (mitigated when tool search defers definitions), plugin enable/disable, bare-tool deny rule, compaction, CLI upgrade, and `--system-prompt-snapshot off`; permission-mode changes and skill/command invocations keep the cache [fact|high: prompt-caching §Actions that invalidate / keep].
- **Resume:** the whole conversation is re-sent; the recorded system prompt is kept by default, so a resume with different `--append-system-prompt` text still reads the old prefix until compaction [fact|high: prompt-caching §Resuming a session]. Subagents have their own cache; a fork inherits the parent's prefix exactly [fact|high: prompt-caching §Subagents and the cache].
- **JSON contract (the fields a parser can rely on):** result message carries `type:"result"`, `subtype`, `session_id`, `duration_ms`, `duration_api_ms`, `is_error`, `num_turns`, `result`, `stop_reason`, `total_cost_usd`, `usage` (incl. `cache_read_input_tokens`, `cache_creation_input_tokens`), `modelUsage`, `permission_denials`, optional `structured_output` [fact|high: agent-sdk typescript `SDKResultMessage`; headless §Get structured output]. A caller should read `session_id` and `usage.input_tokens`/`output_tokens`/`cache_read_input_tokens`, and treat provider cost figures as estimates.
- **Cost on resume:** the docs only state that `maxBudgetUsd` "counts only the call's own spend; totals restored from a resumed session don't count" [fact|high: agent-sdk typescript `maxBudgetUsd`]. Treat `total_cost_usd` on a resumed run as the resumed invocation's view rather than a fresh-vs-resume-comparable total [inference|med].
- **Background work at exit:** `-p` can stay open after the final turn waiting for background commands/subagents/workflows/watches, capped at 10 minutes idle by default; SIGTERM exits 143 and records no result for the in-flight turn [fact|high: headless §Background tasks at exit / §Stop a run with SIGTERM].

### 6. Permission semantics in `-p`

- **Starting mode:** a `-p`/SDK run with **no permission mode set starts in Manual (`default`)**, from 2.1.302 onward — i.e. the installed 2.1.303 is in that range — and calls needing approval are denied [fact|high: permission-modes §Tool calls denied in `claude -p` and Agent SDK runs]. An explicit `--dangerously-skip-permissions` is therefore load-bearing, not redundant.
- `--dangerously-skip-permissions` ≡ `--permission-mode bypassPermissions`: no prompts or safety checks, including writes to protected paths; no warning dialog in non-interactive mode [fact|high: permission-modes §Skip all checks]. Cited exceptions that still prompt/deny: recognized `rm`/`rmdir` critical-path removals, reading another org's public artifact, PowerShell `Remove-Item` denies, cross-session messaging safeguards [fact|high: permission-modes].
- **Root/sudo refusal:** "On Linux and macOS, Claude Code refuses to start in `bypassPermissions` mode when running as root or under `sudo` … The check is skipped automatically inside a recognized sandbox." [fact|high: permission-modes] → a root CI container needs a non-root user or the devcontainer config.
- **`--permission-prompts none`** (≥2.1.259): with a host, requests are denied instead of waiting; without a host they're denied anyway and the flag also tells Claude not to retry. It removes tools that need a person (`AskUserQuestion`), cancels unanswered MCP elicitation, and denials appear in `permission_denials` [fact|high: headless §Turn off permission prompts in unattended runs; cli-ref]. In a plain `-p` run with no host it is a determinism/annoyance improvement, not a permission change [inference|high].
- **`dontAsk`**: auto-denies everything that would prompt; still runs no-approval actions (reads in working dirs, read-only Bash), allow rules, and `PreToolUse` hook approvals; denies `AskUserQuestion`, org-`ask` connector tools, `requiresUserInteraction` MCP tools, network-path reads, critical-path removals [fact|high: permission-modes §dontAsk; headless].
- **`acceptEdits`**: writes files and common filesystem commands without prompting, but **other shell commands and network requests still need an allow rule** — insufficient for "run the plan's tests" [fact|high: headless; repo docs agree].
- **`auto`**: classifier reviews most actions; falls back to denial where auto mode isn't available; not a hard determinism boundary [fact|high: permission-modes §auto].
- **Hooks** can approve (`PreToolUse`) or deny; `PermissionRequest` hooks are consulted before a mode's denial [fact|high: permissions §Extend permissions with hooks; headless]. A project hook therefore *can* change dispatch behavior — the deterministic off-switch is `--settings '{"disableAllHooks": true}'` [fact|high: permissions].
- **Plan mode** keeps its blocks in non-interactive runs, so `--permission-mode plan` in `-p` still refuses edits even with bypass available [fact|high: permission-modes].
- **Bash tool:** read-only commands are auto-approved in Manual; `BASH_DEFAULT_TIMEOUT_MS`/`BASH_MAX_TIMEOUT_MS` bound foreground commands; under `--bare` a command that hits its timeout stops instead of moving to the background [fact|high: env-vars; headless §bare mode].

### 7. Failure modes and practitioner reports

Primary/maintainer-adjacent (tagged by source quality):

- **Auth in CI / headless:** `-p` never shows a trust dialog and, without `--bare`, still reads OAuth/keychain; `--bare` requires an API key or `apiKeyHelper` [fact|high: headless; cli-ref]. On *this* host `claude doctor` reports "macOS Keychain is not writable … Console login will fail to save your API key" [fact|high: local run] — a concrete reminder that credential state is host-specific and worth asserting in a preflight check.
- **Exit/result semantics:** exit 0 on success, non-zero on failure; an in-run failure such as missing auth is printed as the result on stdout; SIGTERM → 143 with no result for the in-flight turn; `--max-budget-usd` stops remaining background work [fact|high: headless].
- **Silent "success" with no work:** open issue #80223 — in `claude -p`, a denied shell substitution in a skill's frontmatter is reported as `subtype:"success"`, `is_error:false`, `num_turns:0`, empty `result`, empty `permission_denials`, with the only signal an untyped `<local-command-stderr>` string [fact|high: anthropics/claude-code#80223, open, created 2026-07-22]. A harness that trusts `is_error` alone can score a no-op as success; a diff/tree-change verifier mitigates this by construction [inference|high].
- **Allow rules not honored in print mode:** closed issue #35601 — `claude --print --permission-mode acceptEdits` ignored `.claude/settings.local.json` `permissions.allow`, denying matching Bash calls (reported on 2.1.76) [fact|high: anthropics/claude-code#35601, closed]. Don't build a predicate on print-mode allow rules; use the mode that matches the contract.
- **Version-specific headless fixes:** 2.1.304 fixed a plugin keeping a headless run going past `--max-turns`; 2.1.306 fixed `claude -p` inside cloud sessions reporting "Not logged in"; 2.1.307 fixed MCP servers reported as timed out/staying failed in headless runs after machine sleep, and the `prompt-caching-scope` header behind custom base URLs [fact|high: CHANGELOG 2.1.304/2.1.306/2.1.307]. None of those fixes are in 2.1.303.
- **Compaction:** auto-compaction replaces history (cache invalidation by design), costs a summarization request, and "Autocompact is thrashing" errors occur in some large-tool-result turns (fixes in 2.1.307) [fact|high: prompt-caching §Compacting; CHANGELOG 2.1.307]. A run that compacts may lose prompt detail late in a long run; a known `--autocompact` window and `--max-turns` bound the exposure [inference|med].
- **Sandbox denials:** blocked hosts/files are reported to the model, which may retry unsandboxed via `dangerouslyDisableSandbox`; strict sandbox mode removes that retry [fact|high: sandboxing §The unsandboxed retry escape hatch / §Turn off the retry].

Secondary / UGC (hypothesis only):

- [hypothesis|med] A practitioner guide frames unattended runs as concentrating three risks — permission, spend, and audit — and recommends `--bare`, explicit credentials, turn/time caps, and capturing structured output + diff as the only record [hidekazu-konishi.com, secondary analysis, accessed 2026-10-22].
- [hypothesis|low] A community "known issues" guide (last updated 2026-04-23) claimed prompt-cache-invalidating bugs were "still active as of v2.1.88" and cited anthropics/claude-code#40524 [UGC]. The issue itself is **closed** (closed 2026-04-15, 124 comments), so that claim is stale and must not be repeated as current [fact|high: anthropics/claude-code#40524 via GitHub API].

---

## Experimental surface table

| Feature | How enabled (2.1.303) | Status | Dispatch risk | Source |
|---|---|---|---|---|
| Agent teams | `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` | Experimental, off by default; no teammate spawning in `-p` | Low (inert) but pointless; alters delegation wording | agent-teams |
| Plugins / marketplaces | Auto-discovery; `--plugin-dir`, `--plugin-url`, `plugin` subcommands | Stable, on by default | **Med-High**: tool/prompt prefix changes, network installs, can outlive `--max-turns` (fixed 2.1.304) | headless, cli-ref, CHANGELOG |
| Skills / custom commands | Auto-discovery; `--disable-slash-commands` | Stable | Med: prompt/tool changes; `allowed-tools` bypass trust | headless, permissions |
| Hooks (settings, plugin, skill) | Settings files; `--include-hook-events` to observe | Stable | **High**: can block/approve; project settings override user opt-out | headless, permissions |
| Subagents | Built in; `--agents`, `--append-subagent-system-prompt` | Stable | Low-Med: own cache bucket, extra spend | prompt-caching, cli-ref |
| Background sessions | `--bg` / `claude agents` / supervisor | Stable, **rejected with `-p`** | None for dispatch; needs workspace trust | headless, agent-view |
| Remote Control | `--remote-control`, `claude remote-control` | Stable | None (`-p` incompatible) | cli-ref |
| Auto permission mode | `--permission-mode auto`, `auto-mode` subcommand | Stable, availability-gated | Med: classifier + feature flags = non-determinism | permission-modes |
| Sandboxed Bash | `sandbox.enabled` settings (+ `/sandbox`) | Stable, **off by default** | Med: host settings leak in; escape hatch; not a boundary | sandboxing |
| `--brief` (SendUserMessage) | Flag | Present in help, undocumented in checked docs | Med: adds a tool → prefix change; mid-run user channel | local help (gap) |
| Chrome integration | `--chrome` / `--no-chrome` | Stable | High: browser/extension, extra tools | cli-ref |
| Cloud sessions / environments | `--cloud`, `--environment` | Stable; `--cloud <desc>` rejected with `-p` | None for local dispatch | headless, cli-ref |
| `--bare` (SIMPLE) | Flag / `CLAUDE_CODE_SIMPLE=1` | Stable, recommended, future `-p` default | **Low-Med**: best determinism; breaks OAuth auth, disables system reminders/background tasks and prompt recording | headless, cli-ref |
| `--safe-mode` | Flag / `CLAUDE_CODE_SAFE_MODE=1` | Stable | Low: fewer customizations, normal auth | local help, env-vars |
| `--restricted` | Flag | Stable (≥2.1.248) | **Incompatible**: refuses `bypassPermissions` | cli-ref, permission-modes |
| `--system-prompt-snapshot` | Flag | Stable (≥2.1.257) | Low; must be `on` under `--bare` for resume caching | cli-ref |
| `--exclude-dynamic-system-prompt-sections` | Flag | Stable | Low: reduces prefix variance | cli-ref, local help |
| `--betas` / `ANTHROPIC_BETAS` | Flag (API key only) / env (all auth) | Stable | Med: betas can change request shape; gateway must forward header | cli-ref, env-vars, llm-gateway |
| MCP tool search | `ENABLE_TOOL_SEARCH` | Stable; deferral default on supported models | Med: changes tool-definition set and cache behavior | env-vars, prompt-caching |

## Permission matrix for `-p`

| Selection | Prompts/who answers | What runs | What is denied/fails | Notes |
|---|---|---|---|---|
| *(none set)* since 2.1.302 | Nobody; Manual mode | No-approval actions: working-dir reads, read-only Bash | Anything needing approval, unless allow rule / hook / `--permission-prompt-tool` | Installed 2.1.303 is in range. [permission-modes] |
| `--permission-mode acceptEdits` | Nobody | File edits + common filesystem commands | Other Bash, network, WebFetch without allow rules | Not enough to run tests. [headless] |
| `--permission-mode plan` | Nobody | Reads/analysis | Edits/blocks enforced even headless | [permission-modes] |
| `--permission-mode dontAsk` | Nobody | No-approval actions + allow rules + PreToolUse approvals | Every would-be prompt, `AskUserQuestion`, certain connector/MCP tools, network-path reads, critical-path removals | Locked-down CI mode. [permission-modes] |
| `--permission-mode auto` | Classifier | Most actions reviewed by model | Falls back to denial where unavailable; critical paths still gated | Feature-flag/model gated. [permission-modes] |
| `--dangerously-skip-permissions` (= `bypassPermissions`) | Nobody; checks skipped | Essentially everything, incl. protected-path writes | A few hard exceptions (critical-path `rm`, other-org artifact, PowerShell remove) | Refused as root/sudo; bypass acceptance key irrelevant in `-p`. [permission-modes] |
| bypass + `--permission-prompts none` | Nobody, and Claude is told not to retry | Same as bypass | Same hard exceptions; `AskUserQuestion` removed | Best determinism; flag ≥2.1.259. [headless] |
| `--restricted` (+ any bypass flag) | Person/configured handler for settings/git/tool-config writes | No command/code tools or WebFetch unless named in `--tools`; file tools confined | **Refuses `bypassPermissions`** | Incompatible with a full-permission run. [cli-ref] |
| bypass as root/sudo | — | — | Startup refusal: `--dangerously-skip-permissions cannot be used with root/sudo privileges` | Skip only inside a recognized sandbox; devcontainer runs non-root. [permission-modes] |

## Contradictions and version drift

- **Installed vs latest:** installed `2.1.303` (commit `07df2aedd2d0`); latest released at access is `2.1.308` [fact|high: local `--version`; CHANGELOG]. Docs already describe post-2.1.303 behavior (`--permission-prompts` "v2.1.259 or later", retries "before v2.1.304", a retry-watchdog variable "requires v2.1.305 or later") — so the docs are ahead of the checkout's binary and must be read with the version notes [fact|high: docs pages].
- **`--help` is not complete:** docs say outright "`claude --help` does not list every flag". In 2.1.303, `--max-turns`, `--advisor`, `--channels`, and `--append-subagent-system-prompt[-file]` are absent from help although docs document them with version requirements the build satisfies [fact|high: cli-ref; local help]. An attempt to settle presence with `claude <flag> --version` was inconclusive: `--version` short-circuits argument validation (a deliberately bogus flag also printed the version) [fact|high: local test].
- **`acceptEdits` still prompts for Bash:** consistent with current docs, and the newer default (Manual for `-p` since 2.1.302) makes `--dangerously-skip-permissions` even more necessary [fact|high: permission-modes].
- **UGC staleness:** a community guide asserted prompt-cache-invalidating bugs were active "as of v2.1.88"; the cited issue #40524 is closed (2026-04-15) and the installed build is 2.1.303 [fact|high: GitHub API]. Do not carry that claim forward.
## Gaps / could not verify

- **No live runs.** Nothing here is measured: not `total_cost_usd` on resume, not `cache_creation` TTL buckets, not whether `--betas`/`--max-turns`/`--advisor` are accepted by the 2.1.303 parser (billed or interactive runs were out of scope), not whether `.mcp.json` servers actually connect in a `-p` run of a repository checkout. The `--strict-mcp-config` recommendation rests on the documented rule, not an observed transcript.
- **No `~/.claude` inspection** (constraint): user settings, installed plugins, auto memory, and keychain state on the run host are unknown. `claude doctor` was the only permitted probe; it reports auto-updates enabled, Pro/Max account, remote managed settings not fetched, and a non-writable macOS keychain.
- **`--brief` semantics unverified:** present in 2.1.303 `--help`, not covered by the docs pages checked; the claim that it adds a tool definition and changes the prefix is an inference from its help text.
- **Gateway pass-through unverified:** whether the proxy forwards `anthropic-beta` and `cache_control` verbatim (the documented requirement) was not tested; the 2.1.307 `prompt-caching-scope` fix suggests the 2.1.303 header reaches the proxy.
- **No deployment configuration was read** (constraint), so the effective argv of a live deployment may differ from anything shown here in ways this note cannot see.
- **UGC depth is thin:** one secondary guide and one community guide; no first-hand operator post-mortem was found with a date/version specific enough to promote above hypothesis.

## Sources (accessed 2026-10-22)

Local ground truth
- `claude --version` → `2.1.303 (Claude Code)`; `claude --help` (311 lines); `claude mcp --help`; `claude plugin --help`; `claude agents --help`; `claude doctor --help`; `claude doctor` output. Binary commit `07df2aedd2d0`.
- A repository checkout (2026-10-22): `.claude/settings.json`, `.mcp.json` (server names/types only), `CLAUDE.md`.

Primary upstream (Claude Code docs, Markdown endpoints, accessed 2026-10-22; docs track a build newer than 2.1.303)
- CLI reference — https://code.claude.com/docs/en/cli-reference (flags, system prompt flags, exit behavior)
- Environment variables — https://code.claude.com/docs/en/env-vars
- Run Claude Code programmatically (headless) — https://code.claude.com/docs/en/headless
- How Claude Code uses prompt caching — https://code.claude.com/docs/en/prompt-caching
- Choose a permission mode — https://code.claude.com/docs/en/permission-modes
- Configure permissions (working dirs, workspace trust, hooks) — https://code.claude.com/docs/en/permissions
- Claude Code gateway compatibility guide (`anthropic-beta`, pass-through, retries) — https://code.claude.com/docs/en/llm-gateway-protocol
- Orchestrate teams of Claude Code sessions (agent teams) — https://code.claude.com/docs/en/agent-teams
- Configure the sandboxed Bash tool — https://code.claude.com/docs/en/sandboxing
- Manage multiple agents with agent view — https://code.claude.com/docs/en/agent-view
- Settings reference (`promptCacheTtl`, `sandbox.enabled`, `disableAllHooks`, `includeGitInstructions`) — https://code.claude.com/docs/en/settings-reference
- Agent SDK TypeScript reference (`SDKResultMessage`, `maxBudgetUsd`) — https://code.claude.com/docs/en/agent-sdk/typescript

Primary upstream (GitHub)
- Changelog (2.1.303→2.1.308 at access) — https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md
- Issue #35601 "settings.local.json permissions not honored in --print mode" (closed; reported on 2.1.76) — https://github.com/anthropics/claude-code/issues/35601
- Issue #80223 "Headless: denied skill-frontmatter shell substitution reported as success with num_turns:0" (open) — https://github.com/anthropics/claude-code/issues/80223
- Issue #40524 "[BUG] Conversation history invalidated on subsequent turns" (closed 2026-04-15) — https://github.com/anthropics/claude-code/issues/40524

Secondary / UGC (hypothesis only)
- Hidekazu Konishi, "Claude Code in CI/CD and Headless Automation — Running the Agent Unattended in Pipelines" (secondary analysis; undated page, accessed 2026-10-22) — https://hidekazu-konishi.com/entry/claude_code_cicd_and_headless_automation.html
- Florian Bruniaux, "Known issues & critical bugs" (UGC community guide; last updated 2026-04-23, stale relative to 2.1.303) — https://github.com/FlorianBruniaux/claude-code-ultimate-guide/blob/main/guide/core/known-issues.md

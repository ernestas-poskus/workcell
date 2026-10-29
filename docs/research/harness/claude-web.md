# Claude Code — stable surface for headless dispatch

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

## Scope and versions checked

- **CLI under test:** `claude --version` → `2.1.303 (Claude Code)` [fact, high]. Ground truth was collected 2026-10-22 from the installed binary, not from docs.
- **Mode:** non-interactive print mode (`-p`/`--print`) with no TTY, which is how an embedded sandbox profile runs a harness.
- **Local sources read:** `claude --help` (full), `claude -p --help`, `claude mcp --help`, `claude plugin --help`, `claude agents --help`, `claude doctor --help`, `claude auth --help`, `claude auto-mode --help`; a repository checkout's `CLAUDE.md`, `AGENTS.md`, `.claude/settings.json`, and `.mcp.json` (server names and env key names only).
- **Upstream sources:** `code.claude.com/docs` pages listed in §Sources, all accessed 2026-10-22. These are undated live pages; treat their version-gating notes as claims about "latest", not about 2.1.303 specifically.
- **Rule applied:** where local `--help` and docs disagree, **local `--help` wins for this checkout**; every mismatch is recorded in §Contradictions and version drift.
- Out of scope (owned by `claude-deep`): experimental betas in depth, prompt-cache tuning, war stories.

**Method note — how flags were validated without a model call.** `--version` short-circuits option parsing (`claude --definitely-not-a-flag --version` exits 0), so it cannot prove a flag exists. Flags were therefore probed by trailing an *invalid enum value* (`--output-format bogus`), which commander rejects at parse time before auth or network. One probe (`--permission-mode default`) did **not** error and started a real one-turn session — see §Gaps.

## Findings

### 1. Print-mode output and input contract

- `--output-format` accepts exactly `text|json|stream-json`; default `text`; only works with `--print` [fact, high] (local `--help`; [cli-ref]).
- `json` returns one result object whose documented members are `result` (the text), `session_id`, `usage`, `total_cost_usd`, plus a per-model cost breakdown in `modelUsage`; on `--continue`/`--resume` the totals cover the whole conversation, earlier runs included [fact, high] ([headless] › *Get structured output*, *Pipe data through Claude*; [cost]).
- The result object is an **SDK result message**, so it also carries `type: "result"` and a `subtype` that discriminates outcome: `success`, `error_during_execution`, `error_max_budget_usd` are the ones named in [cost] [fact, med]. Both success and error results carry `usage` and `total_cost_usd` [fact, high] ([cost] › *Track costs on failed conversations*).
- **`is_error` is not documented on any page I read.** It is plausibly still emitted (it is the historical boolean twin of `subtype`), but I could not confirm it without a billed run [hypothesis, low]. Do not key success logic on it; key on process exit code plus `subtype`.
- **`usage` undercounts when subagents run**; `total_cost_usd` and `modelUsage` include them [fact, high] ([cost] › *The three result-level fields differ…*). This matters to any caller that reads `usage.input_tokens`/`usage.output_tokens`/`usage.cache_read_input_tokens`: a run whose agent spawns subagents reports **too few tokens**, while `total_cost_usd` stays correct.
- `--json-schema <schema>` constrains output; the validated object arrives in a **`structured_output`** field, not `result` [fact, high] ([headless] › *Get structured output*). An invalid schema exits with `Error: --json-schema is not valid JSON: …` or `Error: --json-schema is not a valid JSON Schema …` [fact, high] — reproduced locally: `claude --json-schema 'not a schema' -p hi` → `Error: --json-schema is not valid JSON: JSON Parse error: Unexpected identifier "not"` (no model call). `format` is accepted as an annotation and not enforced [fact, high].
- `--input-format` is `text` (default) or `stream-json`; `--replay-user-messages` requires `--input-format stream-json` **and** `--output-format stream-json` [fact, high] (local `--help`; [cli-ref]).
- `--include-partial-messages` requires `--print` + `stream-json`. `--include-hook-events` requires `stream-json`; `SessionStart`/`Setup` hook events are always included whether or not the flag is passed, and some events (`Notification`, `SessionEnd`, `PreCompact`, `PostCompact`) never emit `hook_started` [fact, high] (local `--help`; [cli-ref]).
- Piped stdin is capped at **10 MB**; exceeding it exits non-zero with a clear error [fact, high] ([headless] › *Pipe data through Claude*). A dispatch passes the prompt in argv, so this is only a risk for `--input-format stream-json`.
- Exit codes: 0 on success, non-zero on failure; **SIGTERM → exit 143** with no result recorded for the in-flight turn, **SIGINT ends the turn** [fact, high] ([headless] › *Stop a run with SIGTERM*). A caller that kills the child on timeout therefore sees 143, not a partial JSON result.
- A `-p` run may stay open after the final turn to wait for background work; the idle cap is 10 min (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, default 600000, `0` = wait forever) [fact, high] ([headless] › *Background tasks at exit*; [env-vars]). With `json` output the "Waiting for background work" notice is suppressed so the JSON stays parseable [fact, high].
- In `-p`, user-invoked skills/custom commands work via `/skill-name` in the prompt; terminal-only built-ins (`/login`) do not [fact, high] ([headless] › *Create a commit*).

### 2. Permissions and approval

- `--permission-mode` local allowed values are exactly **`acceptEdits, auto, bypassPermissions, manual, dontAsk, plan`** — reproduced from the parser: `error: option '--permission-mode <mode>' argument 'bogus' is invalid. Allowed choices are acceptEdits, auto, bypassPermissions, manual, dontAsk, plan.` [fact, high]. `default` is **accepted too** even though it is not in the help list: `claude --permission-mode default -p hi` ran without a parse error [fact, high — see §Gaps for how I learned this]. Per docs, the config value is `default` (UI label "Manual") and `manual` is a CLI alias; `claude --help` lists `manual` *in place of* `default` and both work [fact, high] ([cli-ref]; [perm-modes] › *Available modes*).
- **A `-p` run with no permission mode set starts in Manual and denies anything that would prompt, since v2.1.302** [fact, high] ([perm-modes] › *Tool calls denied in `claude -p` and Agent SDK runs*). The decision-relevant fact for a non-interactive run: passing an explicit full-permission flag is **required**, not merely convenient.
- `--dangerously-skip-permissions` is documented as **equivalent to `--permission-mode bypassPermissions`** [fact, high] ([cli-ref]).
- `bypassPermissions` still does not auto-approve everything: explicit `ask` rules, connector tools set to `ask`, `requiresUserInteraction` MCP/`AskUserQuestion` tools, and **critical-path `rm`/`rmdir`** are never auto-approved [fact, high] ([perm-modes] › *Actions no mode auto-approves*). In a `-p` run a critical-path removal is **denied** (no terminal to show the 2-minute countdown) [fact, high] ([perm-modes] › *Time limits and denials…*). Also: **deny rules block in every mode including `bypassPermissions`, and allow rules have no effect in it** [fact, high] (same page).
- `--permission-prompts <host|none>` (default `host`, requires v2.1.259+) decides who answers a prompt in print mode. `none` denies anything that would prompt, tells Claude not to retry, and **removes person-answering tools such as `AskUserQuestion`** [fact, high] (local `--help`; [headless] › *Turn off permission prompts in unattended runs*). Note this is a *host* selector, not a mode: "the permission mode still decides everything else" (local `--help`).
- `--permission-prompt-tool <mcp__…>` routes prompts to an MCP tool in non-interactive mode [fact, high] ([cli-ref]); it is **absent from local `--help`** but parse-accepted [fact, high].
- `.claude/settings.json` `permissions.defaultMode` is *not* a reliable grant for a dispatch:
  - Starting mode comes from `--permission-mode`/`--dangerously-skip-permissions` first, then `permissions.defaultMode`, then the built-in default [fact, high] ([perm-modes] › *Which mode a session starts in*).
  - **`"auto"` and `"bypassPermissions"` set in `.claude/settings.json` or `.claude/settings.local.json` do not take effect**; with `bypassPermissions` the session starts in **Manual** [fact, high] (same page). `bypassPermissions` is honored only from user, `--settings`, or managed settings [fact, high] ([perm-modes] › *Switch permission modes*).
  - A project-level `permissions.defaultMode: bypassPermissions` is therefore **inert for a `-p` run**: it grants nothing, and the run still starts in Manual.
  - `permissions.blockReadsOutsideWorkingDirectories` makes even `bypassPermissions` prompt for out-of-directory reads (v2.1.257+) [fact, high] ([perm-modes] › *Actions no mode auto-approves*).
- `--allow-dangerously-skip-permissions` only puts `bypassPermissions` in the Shift+Tab cycle without starting in it — **useless in a non-interactive dispatch** [fact, high] ([cli-ref]).
- `bypassPermissions` is refused when running as **root/sudo** on Linux/macOS (`--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons`), skipped automatically inside a recognised sandbox [fact, high] ([perm-modes] › *Skip all checks…*). Relevant if a dispatch ever runs in a root container.
- `--restricted` **refuses `bypassPermissions`** and removes Bash/PowerShell/code-running tools and `WebFetch` unless named via `--tools`; it also ignores user/project/local settings, confines file tools to the working directories, and lets only a person (or the configured handler) approve writes to settings/git/tool-config files [fact, high] (local `--help`; [cli-ref]). It is therefore **incompatible** with a run that must edit and execute tests.
- `--allowedTools`/`--disallowedTools` are rule lists, not mode replacements. A bare tool name in `--disallowedTools` removes the tool from context; a scoped rule like `Bash(rm *)` leaves it available and denies matching calls [fact, high] ([cli-ref]). In `-p`, `--allowedTools "Bash,Read,Edit"` is the documented way to auto-approve tools [fact, high] ([headless] › *Auto-approve tools*).

### 3. Session lifecycle

- `--session-id <uuid>` pins the conversation id up front (must be a valid UUID) [fact, high] (local `--help`; [cli-ref]). This is the only way a fresh run can leave a resumable key before it prints a result.
- `--resume <id|name>` accepts a session ID **or a name**, and also an absolute path to a `.jsonl` transcript [fact, high] ([cli-ref]). An ID search covers the current project and its git worktrees, and since v2.1.223 every other project on the machine [fact, high]. **Caution:** because a bare id is searched machine-wide, resuming is not directory-scoped; correct scoping is the caller's job, not the CLI's.
- `--continue` skips sessions created by `claude -p` unless `-p` is also passed (`claude -p --continue` includes them) [fact, high] ([cli-ref]) — a trap for any template that tries to resume with `-c` under `-p`.
- `--fork-session` with `--resume`/`--continue` mints a new session id [fact, high] (local `--help`). A retry must not use it (it would defeat prefix reuse).
- `--no-session-persistence` is **print-mode only and makes the session unresumable** [fact, high] (local `--help`; [cli-ref]). It is the exact opposite of what a resumable run needs; never add it.
- `--bg`/`--background` **cannot be combined with `-p`** — Claude Code rejects the combination with an error naming the conflict [fact, high] ([headless] › *Basic usage*; [cli-ref]). `claude agents --json` exists for scripting background sessions, but it manages *background* sessions, not `-p` runs, so it is not a path for a one-shot headless run.
- SIGTERM leaves the turn unfinished and unrecorded (exit 143); resuming leaves the interrupted turn as-is unless `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` [fact, high] ([headless] › *Stop a run with SIGTERM*).
- Concurrency/resume race: `--resume` on a *still-running* background session attaches to it (since v2.1.285) rather than refusing [fact, high] ([cli-ref]). A serial retry cannot collide with itself, but a manually left-behind background session sharing the id could.

### 4. Model, effort, cost control

- `--model` takes an alias (`fable`, `opus`, `sonnet`, `haiku`) or a full id; it overrides the `model` setting **and** `ANTHROPIC_MODEL` [fact, high] (local `--help`; [cli-ref]).
- `--effort` local help lists `low, medium, high, xhigh, max` [fact, high]. The binary's own runtime warning for a bad value enumerates exactly those five: `Warning: Unknown --effort value 'bogus' — ignoring it and using the default effort. Valid values: low, medium, high, xhigh, max.` [fact, high — reproduced locally]. `CLAUDE_CODE_EFFORT_LEVEL` accepts the same five **plus `auto`**, and **overrides** `--effort`/`/effort` [fact, high] ([env-vars]). Docs additionally claim `--effort` accepts `ultracode` (v2.1.203+) [fact, med] ([cli-ref]) — **not corroborated by the 2.1.303 binary's own value list**; see §Contradictions.
- `--fallback-model` accepts a comma-separated chain tried in order when the primary is overloaded/unavailable, and re-tries the primary at the start of each user turn [fact, high] (local `--help`; [cli-ref]). Cost note: a fallback is a *different model*, and prompt caches are per-model, so a fallback re-bills the prefix [inference, med].
- `--max-budget-usd` stops the run when the **client-side estimate** reaches the amount (print mode only); spend can overshoot, subagent spend counts, and totals restored from an earlier resume do **not** count [fact, high] (local `--help`; [cli-ref]). `--max-budget-usd` is therefore a runaway guard, not a hard cap.
- `--max-turns` limits agentic turns and exits with an error on hit [fact, high] ([cli-ref]); it is **absent from local `--help`** yet parse-accepted [fact, high]. `CLAUDE_CODE_MAX_TURNS` is the env equivalent and `--max-turns` wins when both are set; a non-positive-integer value is **rejected at startup** [fact, high] ([env-vars]).
- `--autocompact <auto|tokens>` sets the auto-compact window (100k–1M) for the session; invalid values are rejected with `It must be 'auto', or between 100k and 1M …` [fact, high — value range reproduced from the local parser]. Requires v2.1.221+ [fact, high] ([cli-ref]).
- `--betas` adds beta headers and is **API-key users only** [fact, high] (local `--help`; [cli-ref]). `ANTHROPIC_BETAS` does the same for *all* auth methods including a claude.ai subscription [fact, high] ([env-vars]) — the only way to send betas on OAuth.
- `total_cost_usd` is explicitly a **client-side estimate**, not billing [fact, high] ([cost] › warning block). Any cost figure derived from it inherits that caveat.

### 5. Context and instruction control

- **Discovery in a checkout with both files:** if `./CLAUDE.md` exists and imports `@AGENTS.md`, a `-p` run loads the `CLAUDE.md` *and* the imported `AGENTS.md` at launch, on top of any `.claude/rules/`, `CLAUDE.local.md`, or `.claude/CLAUDE.md` present [fact, high].
- `@path` imports are expanded at launch, both relative (resolved against the importing file) and absolute, recursively to a depth of **4 hops**; imports do not reduce context cost, they add to it; import parsing skips code spans and fenced blocks; a quoted path is **not** imported [fact, high] ([memory] › *Import additional files*).
- Load order is broadest→most specific: managed policy → user `~/.claude/CLAUDE.md` → project `./CLAUDE.md` or `./.claude/CLAUDE.md` → `./CLAUDE.local.md`, plus every ancestor directory, then subdirectory files on demand [fact, high] ([memory] › *Choose where to put CLAUDE.md files*, *How CLAUDE.md files load*). Files are concatenated, not overridden; closer files read last.
- `CLAUDE.md` beats `AGENTS.md`: Claude reads `AGENTS.md` directly **only when no `CLAUDE.md`/`.claude/CLAUDE.md`/`CLAUDE.local.md` exists in the working directory or above it** (v2.1.277+; a "Project instructions" setting can change this) [fact, high] ([memory] › *When Claude Code reads AGENTS.md*). This corroborates the overlay rationale: keep the harness-specific file thin and import the canonical one.
- Files over **4 MiB are skipped**; the guidance ceiling is ~200 lines; block-level HTML comments are stripped before injection [fact, high] ([memory] › *Write effective instructions*, *Troubleshooting*).
- System-prompt flags: `--system-prompt`/`--system-prompt-file` **replace** the whole default prompt (dropping tool guidance, safety text, and the git-status snapshot); `--append-system-prompt`/`--append-system-prompt-file` **preserve** it and add text; the two families combine, and since v2.1.283 a flag can be combined with its own file form [fact, high] ([cli-ref] › *System prompt flags*). `--system-prompt-file` and `--append-system-prompt-file` are **absent from local `--help`** but parse-accepted [fact, high].
- **`--system-prompt-snapshot` defaults to `on`**: the prompt is rendered on the conversation's first request (including `--system-prompt`/`--append-system-prompt` text), recorded, and every later request **and every `--resume`** sends the recorded text verbatim, even if a later launch passes different text, until compaction [fact, high] (local `--help`; [cli-ref] › *System prompt flags in resumed conversations*). This is a free prefix-stability guarantee for any retry path: **no extra flag is needed**. Caveat: in `--bare` mode recording stays **off** unless `--system-prompt-snapshot on` is passed explicitly [fact, high].
- `--exclude-dynamic-system-prompt-sections` moves per-machine/per-user sections (local help: "cwd, env info, memory paths, git status"; docs: "per-user context, such as the auto memory location") out of the system prompt into the **first user message**, improving cross-user/cross-machine cache reuse; ignored when `--system-prompt` is set; default false [fact, high] (local `--help`; [cli-ref]). Wording drift noted in §Contradictions. Because the **git status snapshot** lives in that dynamic set, and a dispatch's tree state differs run to run, this flag is the cheapest way to keep claude's cached system-prompt prefix byte-identical across runs [inference, med].
- `--bare` is a **behavioural change, not just a speed-up**: it skips hooks, plugins, skills/commands, subagents, MCP servers, auto memory, and CLAUDE.md auto-discovery; sets `CLAUDE_CODE_SIMPLE=1`; and **never reads OAuth credentials or the system keychain** — Anthropic auth must be `ANTHROPIC_API_KEY` or `apiKeyHelper` via `--settings` [fact, high] (local `--help`; [headless] › *Start faster with bare mode*). It also disables system reminders and background tasks, and will **become the default for `-p` in a future release** [fact, high]. It keeps Bash, read, and edit tools, and loads context only from `--append-system-prompt`, `--settings`, `--mcp-config`, `--agents`, `--plugin-dir`, and (partially) `--add-dir` skills.
- `--safe-mode` sets `CLAUDE_CODE_SAFE_MODE=1` and disables CLAUDE.md, skills, plugins, hooks, MCP servers, custom commands/agents, output styles, workflows, themes, keybindings, LSP, and auto memory — but **auth, model selection, built-in tools, and permissions work normally**, and managed policy settings still apply [fact, high] (local `--help`; [cli-ref]). Unlike `--bare`, it does **not** force API-key auth.
- `--add-dir` grants file access to extra directories but **does not discover most `.claude/` configuration** from them; **CLAUDE.md from `--add-dir` directories is not loaded by default** unless `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1` [fact, high] ([cli-ref]; [memory] › *Load from additional directories*).
- `--mcp-config <configs…>` loads MCP servers from files **or inline JSON strings**, space-separated [fact, high] (local `--help`). With `-p` it **waits for still-pending servers before the first turn, up to `MCP_TIMEOUT` (30 s default)** (v2.1.221+) [fact, high] ([cli-ref]; [headless] › *Fail CI when a plugin or MCP server doesn't load*). Invalid entries are skipped silently and reported only in `system/init`'s `mcp_server_errors` [fact, high].
- **`--strict-mcp-config` is what actually isolates MCP**: it uses *only* `--mcp-config` servers and ignores every other MCP configuration [fact, high] (local `--help`; [cli-ref]). Without it, `--mcp-config` only *adds*.
- A repository's `.mcp.json` can declare MCP servers (stdio commands or HTTP endpoints, with env keys). Under `-p` there is **no workspace-trust dialog and no per-server approval prompt**, so those servers are connected or attempted automatically in a headless run [fact, high] ([headless] › *Start faster with bare mode*: "Without `--bare`, a `-p` session runs the hooks in a project's `.claude/settings.json` and connects the servers in its `.mcp.json`, even in a folder you've never trusted").

### 6. Plugins, skills, agents

- `--plugin-dir <path>` loads a plugin from a directory or `.zip` for one session (repeatable; a folder of plugins loads each child, v2.1.265+) and `--plugin-url <url>` fetches a plugin zip (repeatable) [fact, high] (local `--help`; [cli-ref]).
- `--disable-slash-commands` in 2.1.303 is documented as "Disable all skills" (local `--help`) / "Disable all skills and commands for this session" ([cli-ref]) — **semantics shifted from commands to skills**; treat the older "commands only" reading as stale [fact, high].
- `--agents <json-or-file>` defines custom subagents inline, or **with `--print` a path to a JSON file** (file form requires v2.1.281+); the value is validated at startup and Claude Code exits on an invalid one (v2.1.242+) [fact, high] (local `--help`; [cli-ref]). `--agent <agent>` selects one agent for the session and overrides the `agent` setting [fact, high].
- `--brief` enables the `SendUserMessage` tool for agent→user communication [fact, high] (local `--help`). In a headless run there is no user to message, so it adds a tool that can only fail or stall; leave it off [inference, high].
- `--chrome`/`--no-chrome` toggle Chrome integration; browser automation is irrelevant to a repo-only dispatch and should stay off [inference, high].
- `--forward-subagent-text` forwards subagent text/thinking as `assistant`/`user` messages with `parent_tool_use_id` set; requires `--print` + `stream-json` (v2.1.211+) [fact, high] (local `--help`; [cli-ref]).
- In `-p`, `--mcp-config`/`--plugin-dir` load failures are **not fatal** — they surface as `plugin_errors`/`mcp_server_errors` in `system/init`, and stderr warnings are suppressed when stderr is redirected [fact, high] ([headless] › *Fail CI when a plugin or MCP server doesn't load*). A dispatch that redirects stderr therefore loses those warnings; parse `system/init` if you need them.
- `claude plugin …` (install/enable/disable/eval/validate/marketplace) and `claude agents`/`attach`/`logs`/`stop`/`rm`/`respawn` are user-side management commands, not headless-run flags [fact, high] (from `claude plugin --help`, `claude agents --help`).

### 7. Authentication modes and what changes headless

- **`ANTHROPIC_API_KEY`, when present, is always used in `-p`** — no approval step (in interactive mode it prompts once) [fact, high] ([env-vars]). The installed CLI on this machine evidently has usable OAuth/subscription credentials: a `-p` run needed no API key (see §Gaps) [fact, high].
- `CLAUDE_CODE_OAUTH_TOKEN` / `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` (+ required `CLAUDE_CODE_OAUTH_SCOPES`) provision subscription auth for automated environments; the token takes precedence over keychain-stored credentials [fact, high] ([env-vars]).
- Third-party providers select by env var, not flag: `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, and Foundry via `ANTHROPIC_FOUNDRY_*` (`_API_KEY`, `_AUTH_TOKEN`, `_BASE_URL`, `_RESOURCE`) [fact, high] ([env-vars]; [cost] example setting `CLAUDE_CODE_USE_BEDROCK=1`). Setting a provider var to `""` is the documented way to neutralise a stale export, because `env` blocks can set but not unset [fact, high] ([env-vars]).
- `--bare` is the auth-mode cliff: it never reads OAuth creds or the keychain, so a subscription-auth deployment that adds `--bare` will start failing auth [fact, high] (local `--help`; [headless]).
- `--betas` requires API-key auth; `ANTHROPIC_BETAS` works with all auth methods [fact, high] ([cli-ref]; [env-vars]).
- `ANTHROPIC_BASE_URL` overrides the endpoint (a caller routes claude through a gateway this way); since v2.1.196 Remote Control is disabled when it points anywhere but `api.anthropic.com` [fact, high] ([env-vars]).
- Provider-managed hosts: `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST` makes Claude Code ignore provider/auth vars such as `CLAUDE_CODE_USE_BEDROCK`, `ANTHROPIC_BASE_URL`, and `ANTHROPIC_API_KEY` from settings files [fact, high] ([env-vars]) — relevant only if a deployment embeds claude.
- Headless-specific auth consequence: because a `-p` run shows **no trust dialog**, a repository-provided `env`/hook/MCP surface is not gated by a human [fact, high] ([headless]; [perm-modes]). A caller that scrubs credentials from the child environment prevents such a server from starting with its key; the failure surfaces only in `system/init`.

## Flag inventory table

Status: **verified** = present in local `--help` *and* accepted by the 2.1.303 parser; **hidden-accepted** = absent from local `--help` but parse-accepted; **settings/env** = not a CLI flag.

| Flag | CLI / settings / env | Status | Effect | Source |
|---|---|---|---|---|
| `-p`, `--print` | CLI | verified | Non-interactive; skips trust dialog and silent-ignores invalid settings files | `claude --help` |
| `--output-format text\|json\|stream-json` | CLI | verified | Output shape; `json` = single result object | `--help`; [cli-ref] |
| `--input-format text\|stream-json` | CLI | verified | Print-mode stdin format | `--help` |
| `--include-partial-messages` | CLI | verified | Stream deltas; needs `-p` + `stream-json` | `--help` |
| `--include-hook-events` | CLI | verified | Hook lifecycle events; needs `stream-json` | `--help` |
| `--replay-user-messages` | CLI | verified | Echo stdin user messages; needs stream in+out | `--help` |
| `--json-schema <schema>` | CLI | verified | Validated output in `structured_output`; hard error on bad schema | [headless]; local repro |
| `--permission-mode <mode>` | CLI | verified | `acceptEdits\|auto\|bypassPermissions\|manual\|dontAsk\|plan` (+`default`, undocumented in help) | `--help`; parser repro |
| `--permission-prompts host\|none` | CLI | verified | Who answers prompts in print mode; `none` denies | `--help`; [headless] |
| `--permission-prompt-tool <mcp__…>` | CLI | hidden-accepted | Route prompts to an MCP tool | [cli-ref]; parser repro |
| `--dangerously-skip-permissions` | CLI | verified | = `--permission-mode bypassPermissions` | `--help`; [cli-ref] |
| `--allow-dangerously-skip-permissions` | CLI | verified | Adds bypass to mode cycle only; useless headless | `--help` |
| `--allowedTools`, `--allowed-tools` | CLI | verified | Allow rules; auto-approve in `-p` | `--help`; [headless] |
| `--disallowedTools`, `--disallowed-tools` | CLI | verified | Deny rules; bare name removes tool from context | `--help`; [cli-ref] |
| `--tools <list>` | CLI | verified | Restrict built-in tool set (`""`/`default`/names) | `--help` |
| `--settings <file-or-json>` | CLI | verified | Session settings overlay; ≤2 MiB regular file | `--help`; [cli-ref] |
| `--setting-sources user,project,local` | CLI | verified | Which settings scopes load | `--help` |
| `permissions.defaultMode` | settings | settings | Starting mode; **ignored for `auto`/`bypassPermissions` in project/local** | [perm-modes] |
| `--restricted` | CLI | verified | Removes code-running tools+WebFetch, confines file tools, **refuses bypassPermissions** | `--help`; [cli-ref] |
| `--session-id <uuid>` | CLI | verified | Pin conversation id up front | `--help` |
| `--resume`, `-r` | CLI | verified | Resume by id/name/transcript path (machine-wide id search) | `--help`; [cli-ref] |
| `--continue`, `-c` | CLI | verified | Most recent conversation; `-p` needs `-p` again to include `-p` sessions | `--help`; [cli-ref] |
| `--fork-session` | CLI | verified | New session id on resume | `--help` |
| `--no-session-persistence` | CLI | verified | Print-only, **unresumable** | `--help` |
| `--bg`, `--background` | CLI | verified | Background session; **rejected with `-p`** | `--help`; [headless] |
| `--model <alias\|id>` | CLI | verified | Model; overrides `model` and `ANTHROPIC_MODEL` | `--help`; [cli-ref] |
| `--effort <level>` | CLI | verified | `low\|medium\|high\|xhigh\|max` per 2.1.303 parser+warning; docs add `ultracode` | `--help`; parser warning |
| `CLAUDE_CODE_EFFORT_LEVEL` | env | settings/env | `low\|medium\|high\|xhigh\|max\|auto`; **overrides `--effort`** | [env-vars] |
| `--fallback-model <chain>` | CLI | verified | Comma chain on overload; per-turn retry of primary | `--help`; [cli-ref] |
| `--max-budget-usd <amt>` | CLI | verified | Client-side estimate cap; can overshoot; print-only | `--help`; [cli-ref] |
| `--max-turns <n>` | CLI | hidden-accepted | Turn cap; exits with error on hit | [cli-ref]; parser repro |
| `CLAUDE_CODE_MAX_TURNS` | env | settings/env | Same; `--max-turns` wins; bad value rejected at startup | [env-vars] |
| `--autocompact <auto\|tokens>` | CLI | verified | Compact window 100k–1M | `--help`; parser repro |
| `--betas <list>` | CLI | verified | Beta headers; **API-key auth only** | `--help`; [cli-ref] |
| `ANTHROPIC_BETAS` | env | settings/env | Beta headers for all auth methods | [env-vars] |
| `CLAUDE.md` + `@path` imports | file | verified | Launch-time context; 4-hop import depth; 4 MiB skip | [memory]; local `CLAUDE.md` |
| `--system-prompt <text>` | CLI | verified | **Replaces** default prompt (drops tool guidance, git snapshot) | `--help`; [cli-ref] |
| `--system-prompt-file <path>` | CLI | hidden-accepted | Replace from file | [cli-ref]; parser repro |
| `--append-system-prompt <text>` | CLI | verified | Appends, keeps default prompt | `--help` |
| `--append-system-prompt-file <path>` | CLI | hidden-accepted | Append from file | [cli-ref]; parser repro |
| `--system-prompt-snapshot on\|off` | CLI | verified | Default `on`: record prompt once per conversation, reuse on resume | `--help`; [cli-ref] |
| `--exclude-dynamic-system-prompt-sections` | CLI | verified | Moves cwd/env/git-status sections into the first user message for cache reuse | `--help`; [cli-ref] |
| `--bare` | CLI | verified | Minimal mode, no discovery, **no OAuth/keychain**; sets `CLAUDE_CODE_SIMPLE=1` | `--help`; [headless] |
| `--safe-mode` | CLI | verified | Disables customizations but **keeps auth/model/permissions**; sets `CLAUDE_CODE_SAFE_MODE=1` | `--help`; [cli-ref] |
| `--add-dir <dirs…>` | CLI | verified | Extra file access; **no CLAUDE.md load by default** | `--help`; [memory] |
| `--mcp-config <configs…>` | CLI | verified | Add MCP servers (files or JSON); `-p` waits up to `MCP_TIMEOUT` | `--help`; [cli-ref] |
| `--strict-mcp-config` | CLI | verified | **Only** `--mcp-config` servers; the real isolation switch | `--help` |
| `--plugin-dir <path>` | CLI | verified | Session-only plugin(s); repeatable | `--help` |
| `--plugin-url <url>` | CLI | verified | Session-only plugin zip from URL | `--help` |
| `--agents <json-or-file>` | CLI | verified | Custom subagents; file form with `--print` (v2.1.281+) | `--help`; [cli-ref] |
| `--agent <agent>` | CLI | verified | Select one agent for the session | `--help` |
| `--disable-slash-commands` | CLI | verified | Disables **skills** (and commands) | `--help`; [cli-ref] |
| `--brief` | CLI | verified | `SendUserMessage` tool; no user headless | `--help` |
| `--chrome` / `--no-chrome` | CLI | verified | Browser integration toggle | `--help` |
| `ANTHROPIC_API_KEY` | env | settings/env | Always used in `-p` when present | [env-vars] |
| `CLAUDE_CODE_OAUTH_TOKEN` / `_REFRESH_TOKEN`(+`_SCOPES`) | env | settings/env | Provisioned subscription auth | [env-vars] |
| `CLAUDE_CODE_USE_BEDROCK` / `_USE_VERTEX` / `ANTHROPIC_FOUNDRY_*` | env | settings/env | Provider selection | [env-vars] |
| `ANTHROPIC_BASE_URL` | env | settings/env | Endpoint override (gateway route) | [env-vars] |
| `ENABLE_TOOL_SEARCH` | env | settings/env | Deferred MCP tool loading with a custom base URL; **undocumented** | absent from [env-vars]; §Gaps |
| `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` | env | settings/env | Idle wait for background work after final turn (default 600000) | [env-vars]; [headless] |

## Contradictions and version drift

1. **`--help` is knowingly incomplete, and this matters for template review.** [cli-ref] states outright: "`claude --help` does not list every flag, so a flag's absence from `--help` does not mean it is unavailable." Verified for 2.1.303: `--max-turns`, `--system-prompt-file`, `--append-system-prompt-file`, `--append-subagent-system-prompt`, `--permission-prompt-tool` are all absent from `claude --help` yet parse-accepted (trailing-invalid-enum probe) [fact, high]. **Consequence: any template-drift check is one-directional** — it can flag an extra flag, but "flag not in `--help`" is not evidence of a bad config.
2. **`--effort` value set disagrees.** Local `--help` and the binary's own runtime warning say `low|medium|high|xhigh|max`; [cli-ref] says additionally `ultracode` (v2.1.203+, = `xhigh` + ultracode on); [env-vars] lists `low|medium|high|xhigh|max|auto` for the env var [fact, high for local, med for docs]. For 2.1.303, treat the five as authoritative and `ultracode` as unverified.
3. **`--permission-mode default` vs help list.** `--help` lists `manual` where docs describe `default`; both are accepted by 2.1.303 (`default` confirmed at runtime, `manual` accepted by parser) [fact, high] ([cli-ref]; [perm-modes]). Not a bug — a naming alias — but any validator that reads its choices from `--help` must accept both.
4. **`--exclude-dynamic-system-prompt-sections` description differs.** Local: "per-machine sections (cwd, env info, memory paths, git status)". Docs: "per-user context, such as the auto memory location" [fact, high]. The local wording is more specific and is the one to design against.
5. **`--disable-slash-commands` semantics moved** from "commands" to "skills" (2.1.303 help: "Disable all skills") [fact, high].
6. **Project-local `bypassPermissions` is inert.** Docs say a project- or local-level `bypassPermissions` does not take effect for the starting mode [fact, high]. This is safety-relevant only if someone assumes the settings file grants elevation.
7. **`-p` default permission mode changed at v2.1.302** (now Manual/deny). Any pre-2.1.302 note that "a `-p` run gets auto mode when it doesn't fetch feature flags" is stale [fact, high] ([perm-modes]).
8. **`--bare` will become the `-p` default.** A template that relies on ambient discovery (CLAUDE.md, `.mcp.json`, hooks) will silently change behaviour on upgrade [fact, high] ([headless]). This is a forward-compat risk to record, not a current bug.
9. **`claude -p --help` vs `claude --help`** differ only in the trailing Commands section (the `-p` form has none); all options are identical [fact, high, local diff]. No drift to chase there.
10. **`--version` short-circuits option parsing**, so `claude --unknown-flag --version` exits 0 [fact, high, local]. Any probe that validates flags must not use `--version`.
11. **`claude agents --help` exposes dispatch-default flags** (`--permission-mode`, `--model`, `--effort`, `--agent`, `--restricted`, `--settings`, `--strict-mcp-config`, `--dangerously-skip-permissions`) that apply to *background* sessions dispatched from agent view, not to `-p` [fact, high]. Their near-identical spelling invites confusion with the `-p` surface; a one-shot run uses `-p` and should ignore `agents`.

## Gaps / could not verify

- **`is_error` on the JSON result.** Not documented on any page I read; [cost] uses `subtype` (`success` / `error_during_execution` / `error_max_budget_usd`) as the discriminator. Treat `is_error` as probable-but-unconfirmed [hypothesis, low]. Success should key on exit code + `subtype`.
- **`ultracode` for `--effort` on 2.1.303** — docs claim it, the binary's warning list omits it; I did not run a billed request to settle it [unverified].
- **`ENABLE_TOOL_SEARCH`** is not in the [env-vars] reference; it may be what keeps on-demand tool loading alive behind a custom `ANTHROPIC_BASE_URL`. Whether 2.1.303 still needs it is unverified; the variable may have been renamed or made unnecessary [unverified].
- **`MCP_TIMEOUT`** is named by [cli-ref] and by `claude --help`'s `--mcp-config` text but did not appear in the [env-vars] page text I retrieved; its default (30 s) is doc-sourced, not measured [fact, med].
- **Live-run deviation (disclosure).** While probing, `claude --permission-mode default -p hi` did not error at parse time and instead **started a real one-turn session**, printing a one-line greeting and a repository MCP server's failure notice. That was an unintended billed run — small (a single short turn), no file changes, and I verified `git status` afterwards showed only the pre-existing untracked paths. No further model calls were made. Every other flag probe ended in an `--output-format`/`--permission-mode` parse error before auth, as intended. The lesson is itself a finding: **`--permission-mode default` is accepted, so it cannot be used as a negative probe.** Because of this, the following remain unmeasured: real `json` result bytes, `is_error`, the `subtype` of a failing run, the exact startup latency `--mcp-config`/pending MCP servers add, and whether `usage` genuinely lacks subagent tokens in CLI (not SDK) output (doc-sourced only).
- **`--exclude-dynamic-system-prompt-sections` effect on cache hit rate** was not measured; the mechanism is documented but the payoff is inference.
- **Beta headers/`--betas` interactions** (e.g. which betas 2.1.303 sends by default) were left to `claude-deep` as out of scope.
- **Windows-only and container (root) behaviour** of `bypassPermissions` was read from docs only; untested here.
- **`--json-schema` + `--output-format json` interaction with `--resume`** (does a resumed run still emit `structured_output`?) is not documented and unverified.

## Sources

All accessed **2026-10-22**; CLI ground truth is `2.1.303 (Claude Code)` on macOS. Upstream doc pages are undated live pages.

**Checkout (primary, authoritative for this version)**

- `claude --version`, `claude --help`, `claude -p --help`, `claude mcp --help`, `claude plugin --help`, `claude agents --help`, `claude doctor --help`, `claude auth --help`, `claude auto-mode --help`, plus parse probes — 2.1.303, 2026-10-22.
- A repository checkout's `.claude/settings.json`, `CLAUDE.md`, `AGENTS.md`, and `.mcp.json` (server names and env **key names** only).

**Upstream documentation**

- [CLI reference](https://code.claude.com/docs/en/cli-reference) — flags, `--permission-mode` alias rules, `--bare`/`--safe-mode`/`--restricted`, `--max-budget-usd`, `--max-turns`, `--permission-prompts`, `--exclude-dynamic-system-prompt-sections`, system-prompt flags and snapshot semantics, `--mcp-config` wait, `--resume` machine-wide search, "`--help` does not list every flag".
- [Run Claude Code programmatically (headless)](https://code.claude.com/docs/en/headless) — `-p` basics, bare mode and its auth cliff, structured output and `structured_output`, `total_cost_usd`, background-work wait, SIGTERM/SIGINT, 10 MB stdin cap, `--permission-prompts none`, MCP/plugin load-error fields, "`--bare` … will become the default for `-p`".
- [Choose a permission mode](https://code.claude.com/docs/en/permission-modes) — mode table, `default` vs `manual`, which mode a session starts in, project/local `bypassPermissions` being inert, `-p` denial since v2.1.302, protected/critical paths, root refusal, `--restricted` incompatibility.
- [How Claude remembers your project (memory)](https://code.claude.com/docs/en/memory) — CLAUDE.md locations and load order, `@path` imports (4-hop), 4 MiB skip, `CLAUDE.md` vs `AGENTS.md`, `--add-dir` behaviour.
- [Environment variables](https://code.claude.com/docs/en/env-vars) — `ANTHROPIC_API_KEY` in `-p`, OAuth token vars, Bedrock/Vertex/Foundry selection, `ANTHROPIC_BETAS`, `CLAUDE_CODE_EFFORT_LEVEL` and its precedence over `--effort`, `CLAUDE_CODE_MAX_TURNS`, `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, `ANTHROPIC_BASE_URL`/Remote Control note.
- [Track cost and usage (Agent SDK)](https://code.claude.com/docs/en/agent-sdk/cost-tracking) — result message fields, `usage` excluding subagent tokens, `modelUsage`/`total_cost_usd` including them, error subtypes, `total_cost_usd` as a client-side estimate, `ENABLE_PROMPT_CACHING_1H`.

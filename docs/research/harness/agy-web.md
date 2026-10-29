# Antigravity CLI (agy) — stable surface for headless dispatch

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

Research note on the `agy` harness surface for a non-interactive run. Date of access: **2026-10-22**. Web pages are
treated as untrusted data; every command above came from local `--help`/read-only probes, never from
a page's instructions. **No model turn was executed** (no live billed run); the JSON envelope below
was obtained from print-mode slash commands, which the changelog states do not start an agent turn
or spend quota.

## Scope and versions checked

| Source | Version / date | Use |
|---|---|---|
| `agy --version` | **1.4.5** | installed CLI under test |
| `agy --help`, `agy help`, `agy mcp --help`, `agy plugin --help`, `agy agents --help`, `agy help install`, `agy help remote-control` | 1.4.5 | flag/subcommand inventory (authoritative) |
| `agy changelog` (994 lines, 1.4.5 → 1.0.x) | 1.4.5 | in-CLI release notes; local source of truth |
| Read-only probes: `agy models`, `agy --output-format json models`, `agy --output-format json agents`, `agy -p "/help\|/config\|/permissions\|/hooks\|/skills" --output-format json`, `agy plugin list --json`, `agy mcp list`, argument-validation probes | 1.4.5 | observed shape, exit codes, sandbox behaviour |
| A repository checkout @ 2026-10-22 | `AGENTS.md`, `GEMINI.md`, instruction discovery | documented surface |
| [Headless mode](https://antigravity.google/docs/cli/headless.md), [Rules](https://antigravity.google/docs/rules.md), [CLI reference](https://antigravity.google/docs/cli/reference.md), [Authenticate](https://antigravity.google/docs/install.md) (antigravity.google docs) | undated; content is 1.4.x-era | primary upstream docs |
| [google-antigravity/antigravity-cli CHANGELOG.md @ main](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md) | 1.4.5 top entry, identical to local `agy changelog` | confirms local changelog is upstream-current |

The checkout and installed CLI are dated 2026-10. Public docs lag the CLI in several places; every
disagreement is listed in *Contradictions and version drift* and resolved in favour of local help /
local changelog.

## Findings (by theme; tag each claim [fact|inference|hypothesis] and [high|med|low], inline citation)

Tags: **[fact|inference|hypothesis]** × **[high|med|low]**; each claim cites its source inline.

### 1. Print/headless mode and the JSON envelope

- **[fact][high]** Print mode is `-p` with aliases `--print` and `--prompt`; `--output-format` is
  `text` (default), `json`, or `stream-json`. Response goes to **stdout**, diagnostics (errors,
  progress, permission notices) to **stderr** `[agy --help]` and
  [headless](https://antigravity.google/docs/cli/headless.md).
- **[fact][high]** The `json` format prints exactly one envelope on completion. Fields: `conversation_id`,
  `status`, `response`, `error` (failure only), `duration_seconds`, `num_turns`, `usage`
  (`input_tokens`, `output_tokens`, `thinking_tokens`, `cache_read_tokens`, `total_tokens`), and
  `structured_output` / `json_schema` when `--json-schema` is used. Verified locally on the
  slash-command path (same envelope plus a `command` member; see below)
  [headless](https://antigravity.google/docs/cli/headless.md), `[agy -p "/help" --output-format json]`.
- **[fact][high]** Print-mode slash commands that the CLI answers itself (`/help`, `/config`,
  `/permissions`, `/hooks`, `/skills`, `/usage`, `/quota`, `/credits`, `/model`, `/effort`,
  `/changelog`) emit the same envelope with a top-level `command` object and `conversation_id: ""`,
  `num_turns: 0`, zero usage; they do not start a turn (`agy changelog` 1.1.11/1.1.12; observed).
  Example observed for `/help`:
  `{"conversation_id":"","status":"SUCCESS","response":"…","duration_seconds":0,"num_turns":0,"usage":{…},"command":{"name":"help","data":{"commands":[…]}}}`.
- **[fact][high]** `--input-format stream-json` reads one JSON object per line from stdin and runs one
  turn per message in a single conversation; it **requires** `--output-format stream-json`, drops any
  `-p` prompt, and exits 1/2 on malformed or unsupported input (docs table)
  [headless](https://antigravity.google/docs/cli/headless.md). `--json-schema` applies to the final
  `result` event only.
- **[fact][high]** `stream-json` emits `init` (once, with top-level `conversation_id` and
  `init.permission_mode`), `step_update` events, and a terminal `result` (same shape as `json`).
  Under `--dangerously-skip-permissions` the init `permission_mode` is `always-proceed`, otherwise
  `request-review` [headless](https://antigravity.google/docs/cli/headless.md).
- **[fact][high]** `--json-schema` accepts a schema string or a file path. Since 1.2.14 plain text,
  bare type names such as `"string"`, a missing schema file, and any schema whose root is not
  `"type":"object"` **fail at startup with exit 1** (`agy changelog`); the docs' claim that a
  primitive type name is accepted is stale (see drift).
- **[fact][high]** `--print-timeout` default is `0` = unlimited (`agy --help`; `agy changelog` 1.2.6
  changed it from 5 minutes to unlimited). It accepts Go duration strings (`15m`, `10m` — docs
  examples). **Critical:** when it expires mid-turn the CLI returns the partial output and **exits
  successfully** with a warning on stderr (`agy changelog` 1.1.28), so a timeout is not a failure
  signal.
- **[fact][high]** `--log-file` overrides the CLI log path; `--disable-slash-commands` turns off
  slash-command and skill expansion in print mode (added 1.1.9 with print-mode expansion).

### 2. Session capture and resume

- **[fact][high]** `conversation_id` in the `json` envelope (and in each `stream-json` `init`/`result`)
  is the resume key; resume with `--conversation <id>`; `-c`/`--continue` resumes the most recent
  conversation [headless](https://antigravity.google/docs/cli/headless.md), `[agy --help]`.
  Agent turns carry a UUID (`055a398f-db14-4c5f-abbb-1bf03f8120a7` in the docs example); the
  slash-command path emits `""`.
- **[fact][high]** The capture keys a parser should accept are `conversation_id`, `conversationId`, `session_id`, and `sessionId`, read newest-first from the last document; an empty string must be filtered out, so a slash-command envelope cannot poison the key. The resume form appends `--conversation {session}` *after* the same `-p/--output-format/--mode/--dangerously-skip-permissions/model/--effort` arguments.
- **[inference][med]** A run killed by the caller's own timeout before the terminal `result` prints leaves no `conversation_id`, because `json` emits only the final envelope. Capturing the id from the `stream-json` `init` event would give an up-front key (the analogue of Claude's `--session-id`), but a `json`-only parser would have to learn `jsonl`. Not required for correctness; it would make failed runs resumable.
- **[inference][high]** Conversation retention is undocumented, and conversation state is persisted under `~/.gemini/antigravity-cli/` — the sandbox probe saw the summary/trajectory store fail to open (see § 9). Resume is therefore only as reliable as a writable `$HOME`. No documented guarantee that a
  conversation can be resumed from a different cwd, `--project`, or auth mode; `--conversation`
  only says "resume a previous conversation by ID".
- **[fact][high]** Headless runs are stateless by default; each `--continue`/`--conversation` starts a
  new process. In-process multi-turn reuse is what `--input-format stream-json` is for
  [headless](https://antigravity.google/docs/cli/headless.md).
- **[inference][high]** `-c`/`--continue` must never appear in a one-shot run's argv: it would attach the run to whatever conversation the user last had, cross-contaminating state.

### 3. Permissions and execution modes

- **[fact][high]** Non-interactive flag: `--dangerously-skip-permissions` auto-approves all tool
  permission requests `[agy --help]`. Without it, a tool that needs approval is **soft-denied**: the
  run continues, **exits 0**, and prints a notice to stderr naming the tool
  [headless](https://antigravity.google/docs/cli/headless.md); since 1.1.20 benign tool errors and
  permission denials are not fatal to the exit code, and since 1.1.27 the refused actions are
  reported as `denied_actions` in the JSON envelope (`agy changelog`). So **exit 0 does not mean the edits happened** — a `git status` plus verify gate is what keeps this honest.
- **[fact][high]** `--mode` is `accept-edits | auto | plan` `[agy --help]`; it is applied in headless
  `-p` runs (fixed in 1.1.12: `--mode` had been ignored headlessly). An unrecognised value is a
  **warning only**, not fatal: `warning: unrecognized --mode value "bogus" (valid: accept-edits, auto,
  plan)` then the run continues, exit 0 (observed on `-p "/help" --mode bogus`).
- **[fact][high]** Since 1.3.2, `--dangerously-skip-permissions` no longer auto-approves the agent's
  implementation plan in plan mode; it only skips tool-permission prompts. Headless runs proceed
  through plan review automatically (1.1.28), so `--mode plan` does not stall a dispatch
  (`agy changelog`).
- **[fact][high]** `--sandbox` runs the agent's own terminated commands under the OS sandbox
  ("sandbox with terminal restrictions enabled", `agy --help`); `settings.json` has
  `enableTerminalSandbox` (default `false`) and `allowNonWorkspaceAccess` (default `false`)
  [reference](https://antigravity.google/docs/cli/reference.md), observed via `/config`. **Do not
  pass `--sandbox` on a dispatch**: it restricts the very builds/tests the plan needs.
- **[fact][high]** `--add-dir` adds a directory to the workspace (repeatable, `[agy --help]`). A single mapped checkout does not need it, and a run that forbids writes outside the checkout should not add one.
- **[fact][high]** Scoped pre-approval is possible via `permissions.allow` rules of the form
  `command(git)`, `command(regex:…)`, `write_file(src/)` in
  `~/.gemini/antigravity-cli/settings.json` [headless](https://antigravity.google/docs/cli/headless.md),
  observed shape `{"permissions":{"allow":["command(openspec)","command(git log)"]}}` via
  `agy -p "/permissions" --output-format json` (`scope: project|shared|global`).
- **[fact][high]** `--project <ID|name>` and `--new-project` exist (`[agy --help]`); `--new-project`
  creates a project for the session. Their headless semantics and interaction with conversation
  resume are undocumented (gap).

### 4. Configuration and instruction discovery

- **[fact][high]** **Workspace rules:** `AGENTS.md` and `GEMINI.md` are both read, with or without
  `.agents/AGENTS.md` / `.agents/GEMINI.md`, plus `.agents/rules/*.md` (legacy `.agent/rules/*.md`);
  discovery walks up from each file's directory to the workspace root and rules are **cumulative**,
  not replacement-based. `AGENTS.md`/`GEMINI.md` need no frontmatter and are always-on; files under
  `rules/` require YAML frontmatter with `trigger: always_on|model_decision|glob|manual`
  [rules](https://antigravity.google/docs/rules.md). 1.4.4 fixed the user-rules wording so
  `GEMINI.md` and `AGENTS.md` explicitly override earlier built-in instructions (`agy changelog`).
- **[fact][high]** **Global rules:** `~/.gemini/AGENTS.md`, `~/.gemini/GEMINI.md`,
  `~/.gemini/config/AGENTS.md`, `~/.gemini/config/GEMINI.md`, `~/.gemini/config/rules/*.md`, and for
  the CLI additionally `~/.gemini/antigravity-cli/rules/*.md`; plugin rules under
  `~/.gemini/antigravity-cli/plugins/<name>/rules/` [rules](https://antigravity.google/docs/rules.md),
  confirmed by `agy changelog` 1.2.15/1.1.13 fixes.
- **[fact][high]** Limits: 24 000 bytes per rule file (truncated), 20 000-token aggregate budget with
  automatic demotion to pointers [rules](https://antigravity.google/docs/rules.md).
- **[fact][high]** `settings.json` lives at `~/.gemini/antigravity-cli/settings.json`; documented keys
  include `permissions`, `toolPermission` (`request-review|proceed-in-sandbox|always-proceed|strict`),
  `artifactReviewPolicy` (`asks-for-review|agent-decides|always-proceed`), `enableTerminalSandbox`,
  `allowNonWorkspaceAccess`, `enableTelemetry` (default `true`), `modelProvider`, `colorScheme`,
  `altScreenMode`, `verbosity`, `notifications`
  [reference](https://antigravity.google/docs/cli/reference.md), [install](https://antigravity.google/docs/install.md).
  The 1.4.5 `/config` payload exposes more keys than the docs list (`model`, `modelProvider`,
  `sandboxProxy`, `customModelsConfig`, `contextWindow`, `gcp`, `trustedWorkspaces`,
  `disableSlashCommands`, `queuedMessages`, `pickerGrouping`, `statusLine`, `title`, …) — see drift.
- **[fact][high]** Skills, plugins, hooks, subagents and sidecars are first-class and discovered from
  global + workspace + plugin scopes; `agy plugin validate [--strict] [--json]` checks manifests,
  hooks, sidecars, skills, rules and agents (`agy plugin --help`, `agy changelog` 1.3.3/1.4.0/1.4.1).
  `agy -p "/hooks" --output-format json` returned `{"hooks":[]}` here — no hooks configured.
- **[inference][high]** In a headless run the checkout's `AGENTS.md` is discovered because the child's cwd is the workspace. `--add-dir` is not needed for that; a thin `GEMINI.md` shim is harmless because `AGENTS.md` is read alongside it.

### 5. Environment variables and authentication

- **[fact][high]** Auth paths: cached keyring/OAuth session (interactive sign-in first), **Gemini API
  key** (`modelProvider: "gemini"` in `~/.gemini/antigravity-cli/settings.json` **plus**
  `GEMINI_API_KEY` in the environment — the key alone has no effect), or **ADC**
  (`AGY_ADC_AUTH=true`, optionally `GOOGLE_CLOUD_QUOTA_PROJECT`, `GOOGLE_CLOUD_LOCATION`)
  [install](https://antigravity.google/docs/install.md). `GOOGLE_API_KEY` and `.env` files are not
  read; the CLI only checks the key is non-empty at startup, so a bad key fails on the first turn.
- **[fact][high]** `GOOGLE_GEMINI_BASE_URL` points model requests at a custom Gemini-compatible
  endpoint [install](https://antigravity.google/docs/install.md). The CLI appends `/v1beta/...` itself.
- **[fact][high]** No terminal in CI: headless uses cached credentials, and an unauthenticated headless
  run exits with an `authentication required` error instead of hanging
  [headless](https://antigravity.google/docs/cli/headless.md). This is the intended failure under a
  `GEMINI_API_KEY` (no browser) or pre-seeded keyring.
- **[fact][high]** UI/terminal env vars found in the local changelog: `AGY_CLI_OSC7501`,
  `AGY_CLI_OSC7501_MESSAGES`, `AGY_CLI_HIDE_LOGO`, `AGY_CLI_DISABLE_ESCAPE_SEQUENCE_OPTIMIZATIONS`
  (`agy changelog` 1.4.0/1.1.19). `NO_COLOR`/`CLICOLOR`/`CLICOLOR_FORCE` are honoured by plugin
  commands (1.4.3). For agent commands the CLI sets `GIT_EDITOR`/`HGEDITOR`/`JJ_EDITOR=false` and
  `PAGER`/`GIT_PAGER`/`JJ_PAGER=cat` (1.3.6/1.3.3) — useful for non-TTY runs.
- **[fact][high]** **No documented CI/telemetry env var.** Telemetry is a settings key
  (`enableTelemetry`, default `true`); no environment opt-out is documented in the pages fetched
  (gap).
- **[fact][high]** **`agy models` is not an auth probe**: in this sandbox the CLI reported
  `error getting token source: You are not logged into Antigravity.` yet still printed 13 models on
  stdout and exited 0. Do not use `agy models` as an auth check; any such check must be a probe that actually needs a credential.

### 6. Exit codes and status values

- **[fact][high]** `0` success; `1` startup/failure of a turn that never ran (unknown model, invalid
  `--effort`, invalid `--json-schema`, unknown `--agent`, malformed stream-json input); `2`
  print-mode **usage** errors (`-p` without a value, unknown flag, `--print` + `--prompt-interactive`,
  stream-json `control_request` or a CLI-handled slash command); `3` a headless run that ended on a
  model/agent API failure, with a structured `AGY_ERROR: {...}` line on stderr (`agy changelog`
  1.2.6/1.2.10/1.3.4; local probes below). Status values in the envelope: `SUCCESS`, `ERROR`,
  `CANCELED`, `INTERRUPTED`, `INVALID`, `WAITING`, `RUNNING`
  [headless](https://antigravity.google/docs/cli/headless.md).
- **[fact][high]** Probed locally on 1.4.5:
  - `agy -p` → **2**, `flag needs an argument: -p` + usage dump.
  - `agy --bogus-flag` → **2**, `flags provided but not defined: -bogus-flag`.
  - `agy -p "/help" --effort xl` → **1**, `error: invalid model selection (--model "" --effort "xl"): invalid --effort "xl" (valid: low, medium, high, xhigh, max)`.
  - `agy -p "/help" --mode bogus` → **0** after a warning (see § 3).
  - `agy -p "/help" --output-format bogus` → **0**, plain text output; `--output-format` is **not**
    validated (a typo silently loses the JSON envelope).
- **[fact][high]** `--print-timeout` expiry is **not** a non-zero exit (partial output + success,
  1.1.28). `--json-schema` rejection is exit 1 at startup (1.2.14).

### 7. Model, agent, effort and the `models`/`agents` subcommands

- **[fact][high]** `agy models` lists 13 slugs on this install: `gemini-4-argon-{medium,high,low}`,
  `gemini-3.8-flash-{high,medium,low}`, `claude-opus-5-5-{low,medium,high}`,
  `claude-sonnet-5-5-{low,medium,high}`, `gpt-oss-120b-medium` (observed, exit 0). Docs list
  `gemini-3.7/3.6/3.1-pro` — stale.
- **[fact][high]** `--effort` vocabulary is `low|medium|high|xhigh|max` (`agy --help`; the error
  message above). `s|m|l|xl` are rejected at startup with exit 1.
- **[fact][high]** `--model` + `--effort` together resolve to an effort variant: since 1.3.4 a base
  model name without `--effort` picks the model's default thinking level, and the CLI logs whenever a
  supplied model name resolves to a different model, "such as alias resolution, `--effort` variant
  selection" (`agy changelog` 1.1.28/1.3.4). Implication: a model slug whose suffix names an effort (`gemini-3.8-flash-medium`) combined with `--effort high` resolves to the high variant; the suffix is effectively superseded by `--effort` **[inference][med]**.
- **[fact][high]** `agy agents` returned an empty list here (`{"data":{"agents":[]}}`), so `--agent`
  would exit 1 (1.3.4: unknown agent in a headless run lists the available agents). The template
  correctly omits `--agent`.
- **[fact][high]** Machine-readable subcommand output works only as the **global** flag placed before
  the subcommand: `agy --output-format json models` returns the standard envelope with
  `command.name = "models"` and `data.models[]` (observed); `agy models --output-format json` fails
  with `Error: flags provided but not defined: -output-format` (observed). The changelog wording
  ("models and agents subcommands … through an `--output-format` flag", 1.1.12) reads as a
  subcommand flag — drift.

### 8. Subcommand inventory (`agy --help`)

`agent`/`agents` (list available agents), `changelog` (release notes; print-mode `/changelog`
answers it), `help`, `install` (`--dir`, `--skip-aliases`, `--skip-path`), `mcp`
(add/remove/list/enable/disable), `mic-serve`, `models`, `plugin`/`plugins`
(list [--json], import, install, uninstall, enable, disable, validate [--strict] [--json], link,
help), `remote-control` (start/status/stop), `update`. Note `agy install --help` and
`agy changelog --help` printed nothing and exited 0; `agy help install` / `agy help remote-control`
work — per-subcommand `--help` is not universal in 1.4.5 (observed).

`agy mcp` manages a user-level `mcp_config.json` (changelog 1.1.16); `agy mcp list` prints a table of
configured servers and their command lines. **`agy mcp list` prints any credential embedded in an
MCP server's arguments in clear text** (observed with this workstation's Sentry server; value
redacted here). Never capture that output into a log, prompt, or commit.

### 9. Sandboxed `$HOME` — the operational fact, corrected

- **[fact][high]** In this session's sandbox (writes confined to the checkout, `$HOME` read-only) every
  `agy` invocation printed, before producing anything:
  `ERROR: logging before google.Init: … Failed to redirect output for CLI: creating log file: opening
  log file: open ~/.gemini/antigravity-cli/log/cli-<timestamp>.log: operation not permitted`, then
  `Failed to initialize crash reporter: failed to setup crash output: open
  ~/.gemini/antigravity-cli/crashes/crash_<pid>_<uuid>.log: operation not permitted`, then
  `summary store open failed (corrupt_on_open), recreating: summary_store: open write db: unable to
  open database file: no such file or directory` and `store_client … cannot reopen for
  reconciliation`.
- **[fact][high]** **`agy models` and `agy agents` did not fail** in that sandbox: they produced their
  stdout (`models`: 13 rows; `agents`: `[]`), exited **0**, and wrote the errors to stderr. The premise that they "fail" is only true in the sense of noisy diagnostics; the dispatch
  consequence is different: captured stderr contains this noise, and conversation/state persistence
  is what actually breaks. `/help`, `/config`, `/permissions`, `/hooks`, `/skills` and
  `--output-format json` also worked, with clean JSON on stdout.
- **[inference][high]** The `--log-file` flag can move the CLI log into the workspace and silence the
  first error, but the summary/trajectory SQLite store has no documented path override; a read-only `$HOME` therefore makes session resume unreliable. A caller should either run `agy` with a writable `$HOME`/`XDG_*` (not sandboxed) or treat agy session capture as best-effort.
- **[inference][high]** A caller that spawns `agy` directly as a normal child process (with no OS sandbox of its own) is unaffected; this only bites when the caller itself runs inside a sandboxed agent/container with a read-only home.

### 10. Per-run MCP isolation

- **[fact][high]** `agy` has no documented `--mcp-config` / empty-MCP flag; user-level MCP servers from
  `mcp_config.json` load into every run (`agy mcp list` observed with two configured servers). An empty-MCP placeholder has **no effect** on the agy template, unlike claude's `--mcp-config`.
- **[inference][high]** Consequence: every run inherits the user's user-level MCP servers —
  extra tools in the prompt (context cost), extra startup latency (headless blocks on MCP startup,
  changelog 1.1.9), and a credential-exposure surface if those servers carry tokens. This is a known
  limit, not a regression; a deployment that cares should keep `agy`'s user MCP config minimal or use
  a dedicated `$HOME`.

## Flag inventory table (flag | CLI/config/env | status | effect | source)

Status: **local** = verified in 1.4.5 `--help`/probe; **docs** = documented only (not exercised);
**drift** = docs disagree with local; **absent** = expected but not present.

| Flag | CLI/config/env | Status | Effect | Source |
|---|---|---|---|---|
| `-p`, `--print`, `--prompt` | CLI | local | Run one prompt non-interactively, then exit | `agy --help`; [headless](https://antigravity.google/docs/cli/headless.md) |
| `--output-format` | CLI | local | `text` (default) \| `json` \| `stream-json`; **not validated** (bogus value falls back to text) | `agy --help`; probe |
| `--input-format` | CLI | local | `text` \| `stream-json`; NDJSON prompts on stdin, one turn each; requires stream-json output | `agy --help`; [headless](https://antigravity.google/docs/cli/headless.md) |
| `--json-schema` | CLI | local | Inline schema or file path; restricts output; object root only (1.2.14) | `agy --help`; `agy changelog` |
| `--print-timeout` | CLI | local/drift | Go duration or `0` = unlimited (default 0); expiry returns partial output + exit 0 | `agy --help`; changelog 1.2.6/1.1.28 |
| `--log-file` | CLI | local | Override CLI log path (default under `~/.gemini/antigravity-cli/log/`) | `agy --help` |
| `--disable-slash-commands` | CLI | local | Disable slash-command/skill expansion in print mode | `agy --help`; changelog 1.1.9 |
| `--mode` | CLI | local | `accept-edits` \| `auto` \| `plan`; invalid value = warning only | `agy --help`; probe; changelog 1.3.5/1.1.12 |
| `--dangerously-skip-permissions` | CLI | local | Auto-approve all tool permission requests (not plan review since 1.3.2) | `agy --help`; changelog 1.3.2 |
| `--sandbox` | CLI | local | Run agent's commands under OS terminal sandbox (do not use on dispatch) | `agy --help` |
| `--add-dir` | CLI | local | Add directory to workspace (repeatable) | `agy --help` |
| `--conversation` | CLI | local | Resume a conversation by ID | `agy --help`; [headless](https://antigravity.google/docs/cli/headless.md) |
| `-c`, `--continue` | CLI | local | Continue the most recent conversation (never use on dispatch) | `agy --help` |
| `--model` | CLI | local | Pin model slug; unknown model = exit 1 in headless | `agy --help`; changelog 1.3.4 |
| `--agent` | CLI | local | Select custom agent; unknown name = exit 1 headless | `agy --help`; changelog 1.3.4 |
| `--effort` | CLI | local/drift | `low\|medium\|high\|xhigh\|max`; anything else = exit 1 | `agy --help`; probe |
| `--project` | CLI | local | Project ID or name for the session | `agy --help`; changelog 1.1.18 |
| `--new-project` | CLI | local | Create a new project for this session | `agy --help` |
| `-i`, `--prompt-interactive` | CLI | local | Run initial prompt interactively and continue; conflicts with `--print` (exit 2) | `agy --help`; changelog 1.3.4 |
| `--remote-control` | CLI | local | Create a remote-control connection at startup (ADC unsupported) | `agy --help`; changelog 1.3.3 |
| `permissions.allow` | `~/.gemini/antigravity-cli/settings.json` | local/docs | Scoped pre-approval rules (`command(git)`, `command(regex:…)`, `write_file(src/)`) | [headless](https://antigravity.google/docs/cli/headless.md); `/permissions` probe |
| `toolPermission` | settings.json | local/docs | Global preset (`request-review` default, `always-proceed`, …) | [reference](https://antigravity.google/docs/cli/reference.md); `/config` probe |
| `artifactReviewPolicy` | settings.json | local/docs | `asks-for-review` (default) \| `agent-decides` \| `always-proceed` | [reference](https://antigravity.google/docs/cli/reference.md); `/config` probe |
| `enableTerminalSandbox` | settings.json | local/docs | OS sandbox for agent commands (default false) | [reference](https://antigravity.google/docs/cli/reference.md); `/config` probe |
| `allowNonWorkspaceAccess` | settings.json | local/docs | Allow file tools outside workspace roots (default false) | [reference](https://antigravity.google/docs/cli/reference.md); `/config` probe |
| `enableTelemetry` | settings.json | docs | Metric/crash streaming (default true; no env opt-out documented) | [reference](https://antigravity.google/docs/cli/reference.md) |
| `modelProvider` | settings.json | docs | Set to `gemini` to use a Gemini API key | [install](https://antigravity.google/docs/install.md) |
| `enableAdc` | `~/.gemini/config/config.json` (`userSettings`) | docs | Enable ADC instead of `AGY_ADC_AUTH=true` | [install](https://antigravity.google/docs/install.md) |
| `GEMINI_API_KEY` | env | docs | Gemini API key; only effective with `modelProvider: "gemini"` | [install](https://antigravity.google/docs/install.md) |
| `GOOGLE_GEMINI_BASE_URL` | env | docs | Custom Gemini-compatible endpoint; CLI appends `/v1beta/…` | [install](https://antigravity.google/docs/install.md) |
| `AGY_ADC_AUTH` | env | docs | `true` = Application Default Credentials auth | [install](https://antigravity.google/docs/install.md); changelog 1.3.6/1.4.1 |
| `GOOGLE_CLOUD_QUOTA_PROJECT`, `GOOGLE_CLOUD_LOCATION` | env | docs | ADC quota project / location (optional) | [install](https://antigravity.google/docs/install.md) |
| `AGY_CLI_OSC7501`, `AGY_CLI_OSC7501_MESSAGES` | env | local | Terminal status reporting toggle/verbosity | changelog 1.4.0 |
| `AGY_CLI_HIDE_LOGO` | env | local | Suppress banner logo | changelog 1.1.19 |
| `AGY_CLI_DISABLE_ESCAPE_SEQUENCE_OPTIMIZATIONS` | env | local | Bypass renderer optimisations | changelog 1.1.19 |
| `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` | env | local | Colour control for plugin commands | changelog 1.4.3 |
| `agy mcp add/remove/list/enable/disable` | subcommand | local | Manage user-level `mcp_config.json` | `agy mcp --help`; changelog 1.1.16 |
| `agy plugin list --json` / `validate --strict --json` | subcommand | local | Plugin inventory / CI validation | `agy plugin --help`; changelog 1.3.4/1.4.0 |
| `agy models` / `agy agents` | subcommand | local | Static-ish catalogs; **exit 0 even when unauthenticated**; not an auth check | probes |
| `agy --output-format json models\|agents` | global flag | local/drift | Machine-readable subcommand output; **must precede** the subcommand | probe; changelog 1.1.12 |
| `agy install` (`--dir`, `--skip-aliases`, `--skip-path`) | subcommand | local | Configure PATH/aliases; skip on run hosts | `agy help install` |
| `agy remote-control start/status/stop` | subcommand | local | Background daemon | `agy help remote-control` |
| `agy update` | subcommand | local | Update CLI (do not run) | `agy --help` |

## Contradictions and version drift

| Claim | Stale source | 1.4.5 reality | Resolution |
|---|---|---|---|
| `--print-timeout` default `5m` / "waits up to five minutes" | [headless](https://antigravity.google/docs/cli/headless.md) flag table and prose | `--help`: `(default 0s)`; changelog 1.2.6: default changed from 5 min to unlimited | local wins |
| `--effort` is `low, medium, high` | [headless](https://antigravity.google/docs/cli/headless.md) flag table/prose | `low\|medium\|high\|xhigh\|max` (`--help`, runtime error message) | local wins |
| `--json-schema` accepts `string`/`number`/… type names | [headless](https://antigravity.google/docs/cli/headless.md) | 1.2.14: bare type names, plain text, missing files, non-object roots fail at startup (exit 1) | local changelog wins |
| Model catalog includes `gemini-3.7/3.6/3.1-pro` | [headless](https://antigravity.google/docs/cli/headless.md) | `agy models` lists 13 slugs: `gemini-4-argon`, `gemini-3.8-flash`, `claude-opus/sonnet-5-5`, `gpt-oss-120b` | local wins |
| `models`/`agents` take `--output-format` as a subcommand flag | changelog 1.1.12 wording | only `agy --output-format json models` works; `agy models --output-format json` = `flags provided but not defined: -output-format` | local probe wins |
| `settings.json` key list is complete | [reference](https://antigravity.google/docs/cli/reference.md) | 1.4.5 `/config` payload adds `model`, `modelProvider`, `permissions`, `sandboxProxy`, `customModelsConfig`, `contextWindow`, `gcp`, `trustedWorkspaces`, `disableSlashCommands`, `queuedMessages`, `pickerGrouping`, `statusLine`, `title`, `seasonal*`, `historySize` | `/config` probe wins |
| Headless flag table is the full flag set | [headless](https://antigravity.google/docs/cli/headless.md) | `--mode`, `--add-dir`, `--project`, `--new-project`, `--log-file`, `--disable-slash-commands`, `--remote-control`, `--prompt-interactive` are all in `--help` but absent from the docs table | `--help` wins |
| `--help` works per subcommand | (assumed) | `agy install --help` / `agy changelog --help` print nothing, exit 0; `agy help install` works | local probe |
| Precedence between `AGENTS.md` and `GEMINI.md` | neither page states an order | both are always-on, cumulative; 1.4.4 only guarantees both override built-ins | unresolved; a thin `GEMINI.md` shim makes the ordering moot |
| Local `agy changelog` might lag upstream | — | top entry and text match [GitHub CHANGELOG @ main](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md) exactly for 1.4.5/1.4.4 | no drift at the top end |

## Gaps / could not verify

- **No live agent turn.** The `json` envelope for a real turn is documented and matches the
  slash-command envelope's shape, but the local probe only exercised `/help`, `/config`,
  `/permissions`, `/hooks`, `/skills`, `models`, `agents`. Fields that appear only on real turns
  (`denied_actions`, `error`, `structured_output`) are taken from local changelog/docs, not observed.
- **`--conversation` semantics across cwd / `--project` / auth mode** are undocumented; retention is
  undocumented. Whether a captured id from a failed run is actually resumable cannot be tested here
  without a billed run.
- **`--project` / `--new-project` headless behaviour** (does `--new-project` mutate state? does
  `--project` isolate conversations?) — unverified; not recommended for the template until tested.
- **`--mode auto` vs `accept-edits` under `--dangerously-skip-permissions`** — the bypass likely makes
  the mode difference moot for tool approvals, but Auto mode also adds its own sandbox-review agent
  (1.3.5/1.4.4); behaviour in a headless run was not tested. `accept-edits` is the conservative choice.
- **Telemetry opt-out for CI** — only the `enableTelemetry` settings key is documented; no
  environment variable was found. Could not verify whether `enableTelemetry: false` is honoured for
  `GEMINI_API_KEY` runs (1.4.1 says consumer telemetry is disabled for business/ADC accounts).
- **`--sandbox` + `--dangerously-skip-permissions` interaction** — not tested (and not needed).
- **Hooks/sidecars/subagents details** — the note covers their existence and discovery scopes; their
  schemas are out of scope here (and the local probe showed no hooks configured).
- **Exit code of `agy models --output-format json`** (the failing form) was not captured separately;
  the error text is recorded, the code is inferred to be 2 by analogy with other flag errors.
- **`agy update` / `agy install`** were only read via help — not executed, per the constraints.

## Sources (accessed 2026-10-22, with version/commit where known)

Local (authoritative for this checkout; agy 1.4.5):

- `agy --version`, `agy --help`, `agy help`, `agy mcp --help`, `agy plugin --help`,
  `agy agents --help`, `agy help install`, `agy help remote-control`, `agy changelog` (994 lines,
  1.4.5 → 1.0.x).
- Read-only probes: `agy models`; `agy --output-format json models`; `agy --output-format json
  agents`; `agy plugin list --json`; `agy mcp list` (output redacted); `agy -p
  "/help|/config|/permissions|/hooks|/skills" --output-format json`; `agy -p` (no value);
  `agy --bogus-flag`; `agy -p "/help" --effort xl`; `agy -p "/help" --mode bogus`;
  `agy -p "/help" --output-format bogus`.
- A repository checkout's root `AGENTS.md` and `GEMINI.md`.

Primary upstream:

- [Headless mode — Antigravity CLI](https://antigravity.google/docs/cli/headless.md)
  (antigravity.google; undated page, content references 1.4.x models; flag table verified stale above).
- [Rules — Google Antigravity Docs](https://antigravity.google/docs/rules.md) (undated; discovery
  scopes, frontmatter, 24 KB / 20 k-token limits).
- [CLI commands / reference — Antigravity Docs](https://antigravity.google/docs/cli/reference.md)
  (undated; `settings.json` key table, slash commands).
- [Authenticate — Antigravity Docs](https://antigravity.google/docs/install.md) (undated; current
  enough to name Gemini 4 Argon; CLI auth: keyring, `GEMINI_API_KEY` + `modelProvider`, ADC,
  `GOOGLE_GEMINI_BASE_URL`).
- [google-antigravity/antigravity-cli `CHANGELOG.md` @ `main`](https://github.com/google-antigravity/antigravity-cli/blob/main/CHANGELOG.md)
  (top entry **1.4.5**; byte-for-byte consistent with local `agy changelog` for 1.4.5/1.4.4).

Not used: third-party repos/blogs returned by search (harbor PR, scion harness docs, agy.dev mirror,
skill marketplaces) — they are UGC/secondary and were not needed for any material claim; no fetched
page's instructions were followed.

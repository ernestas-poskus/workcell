# Codex CLI — stable surface for headless dispatch

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

Research note on the harness surface for a non-interactive run. Scope: `codex-cli` **0.163.0**, non-interactive
mode. Written 2026-10-22. The note is about the *documented, stable* surface; experimental
flags-in-depth, prompt-cache tuning and war stories belong to the sibling `codex-deep` note.

Every claim is tagged `[fact]` (directly observed in this checkout or in a primary source),
`[inference]` (follows from evidence but not directly observed) or `[hypothesis]` (plausible,
unverified), with `[high|med|low]` confidence.

## Scope and versions checked

| What | Version / date | Notes |
| --- | --- | --- |
| Installed CLI | `codex-cli 0.163.0`, 2026-10-22 | npm-shim path `…/node/v26.7.0/bin/codex`; `codex --version` |
| Local help authority | 0.163.0, 2026-10-22 | `codex --help`, `exec --help`, `exec resume/fork/review --help`, `features --help`, `sandbox --help`, `mcp --help`, `resume --help`, `doctor --help`, `review --help` |
| Local config evidence | 0.163.0 | `codex features list`, `codex mcp list` and `-c` probes (read-only, no model call) |
| Upstream source | tag `rust-v0.163.0` | `codex-rs/exec/src/exec_events.rs`, `codex-rs/features/src/lib.rs`, `codex-rs/exec/src/main.rs` |
| Upstream docs | fetched 2026-10-22 | `learn.chatgpt.com` pages (non-interactive mode, config reference, environment variables); a docs *mirror* page reports generation from `openai/codex` commit `6a673e7` on 2026-02-28 |
| Checkout | 2026-10-22 | A repository checkout's `.codex/config.toml` and `.codex`-adjacent config |

**Local `--help` wins over web docs for this checkout.** The public docs are rendered from a
February 2026 commit and describe a codebase roughly eight months older than 0.163.0; several
drifts are recorded below. No model call was made: all probes are help text, feature listing, or
`codex mcp list` (a read-only config query).

## Findings

### 1. `codex exec`: the flag surface a dispatcher actually gets

All of the following are `[fact][high]` from local `codex exec --help` (0.163.0, 2026-10-22) unless
noted.

- `codex exec [OPTIONS] [PROMPT]` and `codex exec [OPTIONS] <COMMAND>` where the subcommands are
  `resume`, `fork`, `review`, `help`.
- Prompt: positional `[PROMPT]`; if omitted **or `-`** is used, instructions are read from stdin.
  If stdin is *piped* **and** a prompt is also given, stdin is appended as a `<stdin>` block
  (`codex exec --help`; corroborated by the [Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)
  docs, fetched 2026-10-22). For a dispatcher the prompt is already one argv string, so neither form is needed — but passing a *bare path* as the prompt is still not a prompt, so the prompt must tell the model to read it.
- `-c, --config <key=value>` — dotted path, TOML-parsed, raw string on parse failure. This is the
  only config channel that survives `--ignore-user-config`.
- `--enable <FEATURE>` / `--disable <FEATURE>` — repeatable, exactly equivalent to
  `-c features.<name>=true|false`.
- `--strict-config` — "Error out when config.toml contains fields that are not recognized by this
  version of Codex". Which layers it validates (file vs `-c`) is **not** stated — see Gaps.
- `-m, --model <MODEL>`; `-p, --profile <CONFIG_PROFILE_V2>` — "Layer `$CODEX_HOME/<name>.config.toml`
  on top of the base user config" (a *file* per profile, matching the docs' profile-file statement,
  [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference)).
- `-C, --cd <DIR>` — working root. Note the short form exists; older notes often write only `--cd`.
- `-s, --sandbox <read-only|workspace-write|danger-full-access>`.
- `--approve-for-me` — "Route approval requests through automatic review using the
  workspace-write sandbox". A newer, less-blasted alternative to the bypass flag.
- `--dangerously-bypass-approvals-and-sandbox` — skips prompts and sandboxing; "intended solely for
  running in environments that are externally sandboxed".
- `--dangerously-bypass-hook-trust` — runs enabled hooks without persisted hook trust **for this
  invocation**; "intended only for automation that already vets hook sources". New in this surface
  relative to older docs; directly relevant to a dispatcher that cannot answer a hook-trust prompt.
- `--add-dir <DIR>` — additional writable roots alongside the primary workspace.
- `--skip-git-repo-check`, `--ignore-user-config` ("`$CODEX_HOME/config.toml` is not loaded; auth
  still uses `CODEX_HOME`"), `--ignore-rules` (skip user and project execpolicy `.rules`).
- `--ephemeral` — "Run without persisting session files to disk". **This is the switch that kills
  `exec resume`**, so a session-reusing dispatcher must not pass it.
- `--json` — "Print events to stdout as JSONL".
- `-o, --output-last-message <FILE>` — final agent message to a file (and still to stdout, per docs).
- `--output-schema <FILE>` — JSON Schema for the model's final response shape.
- `--color <always|never|auto>` (default `auto`), `-i/--image`, `--oss`, `--local-provider
  <lmstudio|ollama>`, `--thread-source <SOURCE>`, `--cyber-access-program
  <standard|daybreak_blue|daybreak_red>` (OpenAI-provider only; not supported with `review`).
- **Absent from `codex exec` in 0.163.0** (present at top level or gone): `-a/--ask-for-approval`,
  `--search`, `--full-auto`, `--yolo`, `--no-alt-screen`, `--no-daemon`, `--remote*`, `--worktree`
  is present on exec but `-P/--permission-profile` is not (that is `codex sandbox` only).
  `[fact][high]` (local help). Consequence: **approval policy and web search are config-only for
  `exec`** — set them with `-c approval_policy=…`, `-c web_search=disabled|live`, not with flags.

The docs page on non-interactive mode says `--full-auto` "is no longer supported" and points at
`--sandbox workspace-write` or `--approve-for-me`; that matches 0.163.0's help and is the clearest
documented signal that the old automation flags are gone (`[fact][high]`).

### 2. `exec resume` / `exec fork` / `exec review`

`[fact][high]` from local help, 0.163.0. This matters because a retry path is a *resume*, and the resume surface is deliberately smaller than the fresh one.

| Surface | `exec` (fresh) | `exec resume` | `exec fork` | `exec review` |
| --- | --- | --- | --- | --- |
| `-C/--cd` | yes | **no** | **no** | **no** |
| `-s/--sandbox` | yes | **no** | **no** | **no** |
| `-a/--ask-for-approval` | **no** | **no** | **no** | **no** |
| `-p/--profile` | yes | **no** | **no** | **no** |
| `--add-dir` | yes | **no** | **no** | **no** |
| `--oss` / `--local-provider` | yes | **no** | **no** | **no** |
| `--json` / `-o` / `--output-schema` | yes | yes | yes | yes |
| `-m` | yes | yes | yes | yes |
| `-c`, `--enable`, `--disable`, `--strict-config` | yes | yes | yes | yes |
| `--skip-git-repo-check`, `--ignore-user-config`, `--ignore-rules` | yes | yes | yes | yes |
| `--dangerously-bypass-approvals-and-sandbox`, `--dangerously-bypass-hook-trust` | yes | yes | yes | yes |
| `--ephemeral`, `--worktree`, `--thread-source` | yes | yes | yes | yes |

- `exec resume [SESSION_ID] [PROMPT]`, `--last`, `--all`; session id is a UUID **or a session name**,
  "UUIDs take precedence if it parses".
- `exec fork <SESSION_ID> [PROMPT]` — forks into a *new* session; `[hypothesis][med]` useful for
  "retry without consuming the captured prefix key".
- `exec review --uncommitted | --base <BRANCH> | --commit <SHA> [--title <TITLE>]`, with `--json`.
  Note the **top-level `codex review` has no `--json` and no `-m`** — for machine-readable review
  the `codex exec review` form is the one to use.
- Because `resume` accepts no `-C` (confirmed by its help), the resumed run's working directory is the session's, not an argument — and it also cannot re-grant `--add-dir`, re-pick a profile, or change sandbox mode. `[fact][high]`

### 3. What a "feature flag" means: stage, default, effective state

`codex features list` prints `key`, `stage`, and `effective` (`[fact][high]`, local output,
2026-10-22). The stage vocabulary is **five** values, not four
([`codex-rs/features/src/lib.rs`, tag `rust-v0.163.0`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/features/src/lib.rs)):

- `UnderDevelopment` — "not ready for external use"; enabling one emits a warning event:
  "Under-development features are incomplete and may behave unpredictably", suppressible with
  `suppress_unstable_features_warning = true` (`[fact][high]`, same file; key also in the
  [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference)).
- `Experimental { name, menu_description, announcement }` — surfaced through the interactive
  `/experimental` menu. A headless dispatcher cannot use the menu; it can only set the key.
- `Stable` — "the feature flag is kept for ad-hoc enabling/disabling".
- `Deprecated` — "should not be used anymore".
- `Removed` — "the feature flag is useless but kept for backward compatibility reason": a **no-op**
  that still parses, so old configs and old wrappers do not break.

Effective state is `default_enabled` from the registry, overridden by `[features]` in config,
`-c features.<name>=…`, `--enable`/`--disable`
(`[fact][high]`; `Features::with_defaults` + the `FeatureSpec` table in `lib.rs`). Two consequences:

1. `codex features list` is **not** a statement about a dispatch that passes `--ignore-user-config`
   — the list reflects the current shell's user config. In this checkout, for example,
   `instant_interrupt` (under development, default `false`) reports `true`, which can only come from
   user config. `[fact][high]`
2. A `Removed` flag reporting `true` means the config still sets it, not that it does anything.
   `sqlite`, `item_ids`, `tool_search_always_defer_mcp_tools`, `collaboration_modes`, `steer`,
   `resize_all_images`, `tui_app_server` all report `true` here while being `removed`. `[fact][high]`

**Flags a headless dispatcher should care about** (`[inference][med]` unless marked; stage and
default from `lib.rs`):

| Feature key | Stage / default | Why a dispatch cares |
| --- | --- | --- |
| `plugins` | stable / true | Plugin-bundled MCP servers and plugin lifecycle are loaded outside `config.toml`; `--disable plugins` visibly removed the plugin-provided MCP servers in this checkout. `[fact][high]` for the `codex mcp list` effect |
| `apps` | stable / true | ChatGPT-connector surface; a dispatch wants no connectors. Disabling it did **not** remove the plugin MCP servers here — the two gates are independent. `[fact][high]` |
| `multi_agent` | stable / **true** | Collab tools are on by default: a dispatched agent can spawn subagents (cost, scope, and harness-nondeterminism). |
| `worktrees` | stable / true | "managed worktree creation and repository-aware sessions" — a dispatch that forbids worktrees may want `--disable worktrees` as a belt-and-braces gate. |
| `goals` | stable / true | "persisted thread goals and automatic goal continuation"; unclear what this does in `exec`. `[hypothesis][med]` |
| `hooks` | stable / true | Lifecycle hooks from `hooks.json`; unpromptable at a TTY-less run, hence `--dangerously-bypass-hook-trust`. |
| `unified_exec` / `shell_tool` / `view_image` | stable / true | Tool surface; defaults are what the prompt already assumes. |
| `network_proxy` | experimental / false | Domain-filtered egress; not needed when the child is trusted, but it is the documented alternative to blanket network access. |
| `web_search_request`, `web_search_cached` | **deprecated** / false | Superseded by the `web_search` config key; do not set these. `[fact][high]` |

### 4. Config keys that matter to a dispatch (and which ones are inert here)

`[fact][high]` for types/defaults from the
[Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference) (fetched
2026-10-22); a repository's `.codex/config.toml` can set them.

- `approval_policy` — `on-request | never | { granular = {…} }`. `untrusted` is **unsupported**, and
  `on-failure` is **deprecated**; the docs say use `on-request` for interactive and `never` for
  non-interactive. Not settable per-command for `exec` (no `-a`).
- `sandbox_mode` — `read-only | workspace-write | danger-full-access`. Docs state `codex exec` defaults to **read-only**, which is why a bypass flag is the difference between a run that can edit and one that cannot.
- `sandbox_workspace_write.network_access` — boolean, "allow outbound network access inside the
  workspace-write sandbox". Default `false`. A repository's project config may set it `true`; a bypass flag makes it moot, but under `--approve-for-me`/`workspace-write` this key is what lets `cargo`/`pytest` reach a registry.
- `sandbox_workspace_write.writable_roots`, `.exclude_tmpdir_env_var`, `.exclude_slash_tmp` — the
  rest of the workspace-write policy.
- `model_reasoning_effort` — `low|medium|high|xhigh|max|ultra` (availability depends on model);
  `model_reasoning_summary` — `auto|concise|detailed|none`. These are the cost/latency knobs a caller can pass with `-c` today.
- `features.<name>` — boolean or a per-feature table (`features.code_mode.enabled`,
  `features.rollout_budget.*`, …).
- `mcp_servers.<id>.*` — `command|url`, `env`, `env_vars`, `bearer_token_env_var`, `enabled`,
  **`required`**. `required = true` changes the failure mode: "if you configure an enabled MCP
  server with `required = true` and it fails to initialize, `codex exec` exits with an error instead
  of continuing without that server" (non-interactive docs, fetched 2026-10-22).
- `shell_environment_policy.inherit` — `all | core | none`; plus `.filters` (canonical
  include/exclude map), `.set`, `.ignore_default_excludes` (default `true`, i.e. by default it does
  **not** strip `KEY`/`SECRET`/`TOKEN`-named variables). Relevant because a caller that scrubs credentials from the child environment gets codex's own second line of defence here.
- `projects.<path>.trust_level` — `trusted | untrusted`; "untrusted projects skip project-scoped
  `.codex/` layers, including project-local config, hooks, and rules". Project config is loaded
  **only when the project is trusted**, and trust is recorded in *user* config.
- `model_providers.<id>` — provider table (base URL, auth, transport). `-c model_providers.*` is the
  documented channel; `openai_base_url`/`chatgpt_base_url` exist as first-class keys.
- `history.persistence` — `save-all | none`; `history.max_bytes`. A one-shot run that keeps its own logs elsewhere wants `none`. `[inference][med]`
- `suppress_unstable_features_warning` — silence the under-development warning in a controlled run.

**What `--ignore-user-config` removes.** Its help says only that `$CODEX_HOME/config.toml` is not loaded and auth still uses `CODEX_HOME`. So `[fact][high]` every key above supplied *by the user config* is inert in a headless run, and the run's real config surface is: argv flags + `-c` overrides + (possibly) a project `.codex/config.toml`. Whether the
project layer survives without the user config's trust record is **unverified** — see Gaps.

### 5. The `-c mcp_servers={}` assumption is wrong (empirical)

A common assumption is that `-c mcp_servers={}` empties the MCP table. Measured on 0.163.0 (`codex mcp list`, 2026-10-22, read-only):

| Probe | Effect |
| --- | --- |
| `codex mcp list` | lists a project-config server and a user-config server as enabled, plus plugin-managed servers (`code-review`, `codex_app`, `cua_repl`, `node_repl`, `computer-use`, `dart`, `rust-docs`) |
| `codex mcp list -c mcp_servers.rust-docs.enabled=false` | `rust-docs` flips to **disabled** — dotted `-c` paths **do** reach the MCP table |
| `codex mcp list -c mcp_servers={}` | **nothing changes** — every server is still listed and enabled |
| `codex mcp list --disable plugins` | plugin-managed servers (`code-review`, `codex_app`, `cua_repl`) drop out; the config-layer servers and `rust-docs` stay |
| `codex mcp list --disable apps` | plugin-managed servers still present — independent gate |

Conclusion: `[fact][high]` for the observed commands — an **empty table override contributes nothing**; the config layer merges tables rather than replacing them. `[inference][high]`: `-c mcp_servers={}` is a **no-op**, so a run inherits whatever MCP servers the loaded layers define. With `--ignore-user-config`, the user layer is gone and the project layer's survival depends on trust (`[inference][med]`); plugin-bundled servers are gated by `plugins`, not by config (`[inference][med]`). The cheap hardening is `--disable plugins` (and, if the project layer is not wanted, ensuring the project is untrusted or explicitly disabling each project server by `-c mcp_servers.<id>.enabled=false`, which is brittle — a per-run `CODEX_HOME` would be the robust isolation but it also relocates auth, see §8).

### 6. Environment variables

`[fact][high]` for the documented list, from
[Environment variables](https://learn.chatgpt.com/docs/config-file/environment-variables) (fetched
2026-10-22):

- `CODEX_HOME` — root for "config, auth, logs, sessions, skills, and standalone package metadata";
  default `~/.codex`; "if you set it, the directory must already exist". `CODEX_SQLITE_HOME`
  overrides where SQLite-backed state lands.
- `CODEX_API_KEY` — API key for "Exec, review, TypeScript SDK, remote exec-server". The docs warn to
  set it **inline**, not job-wide, where repository-controlled code runs.
- `CODEX_ACCESS_TOKEN` — ChatGPT/Codex access token for trusted automation; `codex login
  --with-access-token` for persistence.
- `CODEX_CA_CERTIFICATE` — PEM CA bundle for HTTPS/login/WebSocket clients; **takes precedence over**
  `SSL_CERT_FILE`, which is the documented fallback.
- `CODEX_NON_INTERACTIVE` (installer only), `CODEX_INSTALL_DIR` (installer only), `RUST_LOG`
  (`codex exec` defaults to `error`), and workload-identity variables
  (`OPENAI_FEDERATION_RULE_ID`, `OPENAI_IDENTITY_TOKEN_FILE`, `OPENAI_WORKLOAD_IDENTITY_CONTEXT`).
- **`OPENAI_BASE_URL` is not in the documented stable env list.** The documented base-URL control is
  the `openai_base_url` config key `[fact][high]`. `[inference][high]` Codex still honours `OPENAI_BASE_URL` in practice (an API-key/gateway route may depend on it), but for a *reproducible* argv the safer spelling is `-c openai_base_url=…`.

### 7. JSONL event contract

`[fact][high]`, exact serde shapes from
[`codex-rs/exec/src/exec_events.rs` at `rust-v0.163.0`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/exec_events.rs)
(byte-identical to `main` as fetched 2026-10-22, so no drift between the tag and current `main`):

```
ThreadEvent (internally tagged by "type"):
  thread.started  → { thread_id: String }            // first event; the resumable id
  turn.started    → {}
  turn.completed  → { usage: Usage }
  turn.failed     → { error: { message } }
  item.started / item.updated / item.completed → { item: ThreadItem }
  error           → { message }

Usage = { input_tokens, cached_input_tokens, cache_write_input_tokens,
          output_tokens, reasoning_output_tokens }        // all i64; cache_write defaults 0
```

`ThreadItem` is `{ id, ...tagged by type }` with `type` ∈ `agent_message` (`text`), `reasoning`
(`text`), `command_execution` (`command`, `aggregated_output`, `exit_code: Option<i32>`, `status`),
`file_change` (`changes: [{path, kind: add|delete|update}]`, `status`), `mcp_tool_call` (`server`,
`tool`, `arguments`, `result`, `error`, `status`), `collab_tool_call`, `web_search`, `todo_list`,
`error`. Status enums are `in_progress|completed|failed` (plus `declined` for command execution).

The docs' sample stream matches this, e.g.
`{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122,"reasoning_output_tokens":0}}`
([Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode), fetched 2026-10-22).

Stream discipline (`[fact][high]`, docs): progress goes to **stderr**; only the final agent message
goes to **stdout**; with `--json`, **stdout becomes the JSONL stream**. So the dispatcher's
"capture stdout as JSONL" is correct, and stderr is where diagnostics (including under-development
feature warnings) land.

**Version-drift note for usage parsing.** A parser configured for `info.last_token_usage.*` is targeting the shape of the old `token_count` event. 0.163.0 emits the *v2* shape: a single `turn.completed` carrying a flat `usage.*`, so a fallback to that path is the load-bearing code path. `[fact][high]` (source + tag schema).

### 8. Auth modes and what changes headless

`[fact][high]` unless marked.

- `codex exec` **reuses saved CLI auth by default** (docs). For CI, the documented route is
  `CODEX_API_KEY` set inline for the invocation only — "Do not set `OPENAI_API_KEY` or
  `CODEX_API_KEY` as a job-level environment variable in workflows that check out or run
  repository-controlled code", because build scripts/tests/lifecycle hooks in the same job can read
  it. That failure mode is why the "inline env for the child only" rule exists: the child runs the repository's tests.
- Subscription auth: `~/.codex/auth.json` holds access tokens ("treat like a password"); a
  ChatGPT/Codex access token can be seeded via `CODEX_ACCESS_TOKEN` or `codex login
  --with-access-token`. In a container, the docs say to seed `auth.json` through secure storage and
  let Codex refresh it in place.
- Under subscription auth, model traffic goes to `chatgpt_base_url` (default `https://chatgpt.com/backend-api/`), which Codex requires to be HTTPS — which is why a subscription-auth deployment needs a loopback HTTPS relay or a trusted certificate.
- Credential store: `cli_auth_credentials_store` = `file | keyring | auto | ephemeral` (requirements
  doc). `ephemeral` "keeps credentials in memory for the current process" — a plausible way to avoid
  leaving auth state behind, if a keyring is not wanted. `[inference][med]`
- `forced_login_method = chatgpt | api` documents that the two auth modes are distinct products,
  not just transports, and billing/rate-limit behaviour differs (`[inference][high]`).
- Headless differences that matter: (a) no TTY to answer approval or hook-trust prompts, hence
  `--dangerously-bypass-approvals-and-sandbox` / `--dangerously-bypass-hook-trust`; (b) `-a` does
  not exist on `exec`, so `approval_policy` can only come from config; (c) `--ephemeral` trades
  session persistence (and therefore resume/cache reuse) for a clean filesystem.

### 9. Exit codes

`[fact][high]`: the local `--help` documents no exit codes, and the fetched non-interactive docs
page's "Exit codes" section was not present in the markdown body that served
(`learn.chatgpt.com/docs/non-interactive-mode.md`, 2026-10-22). `[hypothesis][med]` the contract is
the conventional 0 = success, non-zero = failure. There is a maintainer issue,
[openai/codex #33898](https://github.com/openai/codex/issues/33898) (indexed title, not fetched),
claiming `--output-last-message` "doesn't create parent dirs; write fails after the turn completes
yet exits 0, silently losing the final message" — i.e. **exit 0 is not proof the deliverable
exists**. A caller that verifies the diff itself rather than trusting the child's status is therefore right to do so. Treat exit code as one weak signal among several. `[inference][high]`

## Flag inventory table

Status: `stable` = documented and in 0.163.0 help; `absent` = not accepted by that command in
0.163.0; `undocumented` = in help but not in the fetched public docs; `drift` = docs describe a
different spelling/behaviour.

| Flag / key | Kind | Status | Effect | Source |
| --- | --- | --- | --- | --- |
| `--json` | CLI (`exec`, `resume`, `fork`, `review`, `exec review`) | stable | stdout becomes JSONL event stream | local help 0.163.0 |
| `--enable` / `--disable <FEATURE>` | CLI (all major commands) | stable | `-c features.<name>=true|false`, repeatable | local help |
| `-c, --config key=value` | CLI (every command incl. `sandbox`, `mcp`, `features`, `doctor`) | stable | dotted TOML override; the only config channel under `--ignore-user-config` | local help |
| `--strict-config` | CLI (`exec`, `resume`, `fork`, `review`, interactive) | stable | error on unrecognized `config.toml` fields | local help |
| `-m, --model` | CLI | stable | model id | local help |
| `-p, --profile <name>` | CLI (`exec`, resume, sandbox; **absent** on `exec resume/fork/review`) | stable | layers `$CODEX_HOME/<name>.config.toml` | local help; config reference |
| `-C, --cd <DIR>` | CLI (exec only; absent on resume/fork/review) | stable | working root | local help |
| `--skip-git-repo-check` | CLI | stable | allow non-git cwd | local help |
| `--ignore-user-config` | CLI | stable | skip `$CODEX_HOME/config.toml`; auth still from `CODEX_HOME` | local help |
| `--ignore-rules` | CLI | stable (newer surface) | skip user+project execpolicy `.rules` | local help; docs |
| `--ephemeral` | CLI | stable | no session files on disk → no resume | local help; docs |
| `-s, --sandbox` | CLI (exec only) | stable | `read-only` (exec default) / `workspace-write` / `danger-full-access` | local help; docs |
| `--approve-for-me` | CLI | stable (newer surface) | auto-review approvals inside `workspace-write` | local help |
| `--dangerously-bypass-approvals-and-sandbox` | CLI | stable | no prompts, no sandbox | local help |
| `--dangerously-bypass-hook-trust` | CLI | stable (newer surface) | run hooks without persisted trust, this invocation | local help |
| `--add-dir <DIR>` | CLI (exec only) | stable | extra writable roots | local help |
| `--output-schema <FILE>` | CLI | stable | JSON Schema for final response | local help; docs |
| `-o, --output-last-message <FILE>` | CLI | stable | write final message to file (also stdout) | local help; docs |
| `--oss` / `--local-provider` | CLI (exec only) | stable | local model provider (`lmstudio`/`ollama`) | local help |
| `--thread-source`, `--cyber-access-program` | CLI | undocumented | thread source classification / OpenAI cyber program | local help only |
| `-i, --image` | CLI | stable | attach images to prompt | local help |
| `--full-auto`, `--yolo` | CLI | **drift / absent** | removed; docs say use `--sandbox workspace-write` or `--approve-for-me`; `--yolo` appears only in config-reference prose | docs; local help has neither |
| `--search`, `-a/--ask-for-approval` | CLI | **absent on `exec`** | top-level only; `exec` sets these via config | local help |
| `approval_policy` | config | stable | `on-request` / `never` / `granular{...}`; `untrusted` unsupported, `on-failure` deprecated | config reference |
| `sandbox_mode` | config | stable | `read-only` / `workspace-write` / `danger-full-access` | config reference |
| `sandbox_workspace_write.network_access` | config | stable | outbound network inside workspace-write (default false) | config reference; checkout `.codex/config.toml` |
| `sandbox_workspace_write.writable_roots` | config | stable | extra writable roots | config reference |
| `model_reasoning_effort` | config | stable | `low|medium|high|xhigh|max|ultra` (model-dependent) | config reference |
| `model_reasoning_summary` | config | stable | `auto|concise|detailed|none` | config reference |
| `features.<name>` | config | stable / stage-dependent | per-feature boolean or table | `features/src/lib.rs`; config reference |
| `suppress_unstable_features_warning` | config | stable | silence under-development warning | `features/src/lib.rs`; config reference |
| `mcp_servers.<id>.*` (`required`) | config | stable | MCP server defs; `required` turns init failure into an exit | config reference; docs |
| `shell_environment_policy.inherit` | config | stable | `all|core|none`; `.filters`, `.set`, `.ignore_default_excludes` | config reference |
| `projects.<path>.trust_level` | config | stable | `trusted|untrusted`; gates project `.codex/` layers | config reference |
| `model_providers.<id>`, `openai_base_url`, `chatgpt_base_url` | config | stable | provider/base-URL routing | config reference |
| `history.persistence`, `history.max_bytes` | config | stable | `save-all|none`, cap | config reference |
| `force_login_method`, `cli_auth_credentials_store` | config | stable | pin auth mode; `file|keyring|auto|ephemeral` | config reference / requirements |
| `CODEX_HOME` | env | stable | config/auth/logs/sessions root (must exist) | env docs |
| `CODEX_API_KEY` | env | stable | inline API key for exec/review/SDK | env docs |
| `CODEX_ACCESS_TOKEN` | env | stable | ChatGPT/Codex access token | env docs |
| `CODEX_CA_CERTIFICATE` | env | stable | PEM CA bundle; precedence over `SSL_CERT_FILE` | env docs |
| `SSL_CERT_FILE` | env | stable | fallback CA bundle | env docs |
| `CODEX_CA_CERTIFICATES` (plural) | env | **drift / undocumented** | not in the documented list; a loopback-TLS relay may set it — `SSL_CERT_FILE` is documented and is the fallback | env docs vs observed practice |
| `OPENAI_BASE_URL` | env | **undocumented** | not in the stable env list; prefer `-c openai_base_url=…` | env docs |
| `RUST_LOG` | env | stable | `codex exec` defaults to `error` | env docs |
| `thread.started` / `turn.completed` / `item.completed` / `error` | JSONL events | stable | machine-readable event stream | `exec_events.rs` @ tag |
| `codex features list|enable|disable` | CLI | stable | inspect stage+effective; persist `[features]` into `config.toml` | local help; docs |
| `codex exec resume <id>|--last` | CLI | stable | continue a persisted session | local help |
| `codex exec fork <id>` | CLI | stable | new session from an old one | local help |
| `codex exec review --uncommitted|--base|--commit` | CLI | stable | machine-readable review | local help |

## Contradictions and version drift

| # | Contradiction | Local 0.163.0 (wins) | Public docs / older notes | Date / version |
| --- | --- | --- | --- | --- |
| 1 | Feature stages | **five**: `under development`, `experimental`, `stable`, `deprecated`, `removed` | an earlier four-stage list omits `deprecated`; the mirror docs page lists all five | local `codex features list` + `features/src/lib.rs` @ `rust-v0.163.0`; mirror generated 2026-02-28 from commit `6a673e7` |
| 2 | `--full-auto` | absent from `exec --help` | docs: "`--full-auto` is no longer supported. Update scripts to use `--sandbox workspace-write` … or `--approve-for-me`" | docs fetched 2026-10-22; CLI 0.163.0 |
| 3 | `--yolo` | absent from every local `--help` | config reference mentions "if you use `--yolo` or another full access sandbox setting" | config reference fetched 2026-10-22 |
| 4 | `CODEX_CA_CERTIFICATE` vs `CODEX_CA_CERTIFICATES` | a relay setup sets the **plural** | env docs list only the **singular**, as the higher-precedence variable over `SSL_CERT_FILE` | env docs fetched 2026-10-22 |
| 5 | `OPENAI_BASE_URL` | sometimes passed as the API-key route | absent from the documented stable env list; `openai_base_url` is the documented key | env docs fetched 2026-10-22 |
| 6 | `-c mcp_servers={}` | does not remove servers (merge semantics) | an assumption that it empties the MCP table | measured 2026-10-22 on 0.163.0 |
| 7 | `exec resume` flags | no `-C`, `-s`, `-a`, `-p`, `--add-dir`, `--oss` | older notes record only that `-C` is rejected — correct, but incomplete | local help 0.163.0 |
| 8 | Public docs vintage | — | docs mirror was generated 2026-02-28; the CLI is from ~2026-10; the docs mention `gpt-6.1-sol`, `approval_policy = { granular = … }`, `permissions.<name>`, `default_permissions` | mirror page metadata; CLI `--version` |
| 9 | `codex review` vs `codex exec review` | `exec review` has `--json`, `-m`, `-o`, `--output-schema`; top-level `review` has none of them | docs describe `codex review` as non-interactive without noting the split | local help 0.163.0 |
| 10 | `--search` | top-level only; `exec` does not accept it | docs discuss web search in config, not as an exec flag | local help 0.163.0 |
| 11 | Undocumented-in-docs exec flags | `--thread-source`, `--cyber-access-program`, `--dangerously-bypass-hook-trust` | not in the fetched non-interactive docs page | local help 0.163.0 |
| 12 | Tool/permission profiles | `codex sandbox -P/--permission-profile`, `--include-managed-config`, `--sandbox-state-json` exist | config reference documents `permissions.<name>` and `default_permissions` | local help 0.163.0; config reference |

## Gaps / could not verify

No live model run was made (explicitly out of scope: no billed runs), so the following need a real
dispatch or a unit-level Rust probe:

1. **Exit codes.** No documented table, no run. The 0/non-zero assumption is unverified, as is the
   maintainer-issue claim (#33898, not fetched) that `--output-last-message` can fail to write and
   still exit 0.
2. **Whether `--ignore-user-config` also drops the project `.codex/config.toml` layer.** Trust
   records live in user config; if ignoring user config also loses trust, the project layer is
   skipped (§4, §5). This decides whether a project-config MCP server can reach a run.
   Testable by a dispatch whose log greps MCP initialization, or by a `doctor --json` probe.
3. **Which MCP servers a real `codex exec` actually initializes** under the current argv. The
   `codex mcp list` probes show config-layer behaviour, not the exec child's union with
   plugin/marketplace sources.
4. **Whether `--disable plugins` fully excludes plugin MCP servers in `exec`** (it did in
   `codex mcp list`).
5. **Whether `--strict-config` validates the `-c` override layer, the project layer, or only
   `config.toml`** — the help says "config.toml", which under `--ignore-user-config` may be nothing.
6. **`--approve-for-me` under no TTY**: who reviews, and does a rejected approval become a failed
   item or a hung run? `[hypothesis][low]`
7. **`exec resume` working-directory resolution** — whether it uses the recorded session cwd, the
   process cwd, or fails when the recorded path is gone.
8. **`exec fork` semantics** (new thread id? does the original thread's prefix key stay valid?).
9. **`--ephemeral` + `--json`**: whether `thread.started` is still emitted (it should be, but
   `--ephemeral` is the one flag whose whole point is not persisting the thread).
10. **`CODEX_CA_CERTIFICATES` (plural)**: documented alias or silent no-op.
11. **Model availability**: whether `gpt-5.3-codex` is still selectable on 0.163.0 (server-driven
    catalog), and what `fast_mode`/`ultrafast_mode` do in `exec`.
12. **Feature-flag cost effects** (`multi_agent`, `goals`) in non-interactive mode — the registry
    documents what they gate, not what they cost in `exec`.

## Sources (accessed 2026-10-22)

Local ground truth (authoritative for this checkout; `codex-cli 0.163.0`):

- `codex --version`, `codex --help`, `codex exec --help`, `codex exec resume|fork|review --help`,
  `codex features --help` / `features list`, `codex sandbox --help`, `codex mcp --help`,
  `codex mcp list` (+ `-c` probes), `codex resume --help`, `codex doctor --help`,
  `codex review --help` — run 2026-10-22.
- A repository checkout's `.codex/config.toml` (and `.codex`-adjacent config).

Primary upstream (pinned to the checked CLI's version):

- [`codex-rs/exec/src/exec_events.rs` @ `rust-v0.163.0`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/exec_events.rs)
  — "Top-level JSONL events emitted by `codex exec`"; fetched 2026-10-22; identical to `main` at
  fetch time.
- [`codex-rs/features/src/lib.rs` @ `rust-v0.163.0`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/features/src/lib.rs)
  — `Stage` enum, `FeatureSpec` table (`default_enabled`), under-development warning; fetched
  2026-10-22.
- [`codex-rs/exec/src/main.rs` @ `rust-v0.163.0`](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/exec/src/main.rs)
  — confirms `codex-exec` parses `CliConfigOverrides` at the top level (why `-c` works everywhere);
  exit-code handling is in `run_main`, not fetched.

Official docs (unversioned; rendered from commit `6a673e7`, 2026-02-28, per the mirror's own
metadata — ~8 months older than the CLI):

- [Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode) — "Run Codex headlessly
  for automation and CI/CD integration"; JSONL sample, stdin forms, `--ephemeral`,
  `--ignore-user-config`/`--ignore-rules`, `--full-auto` removal, `required = true` MCP failure,
  API-key guidance; fetched 2026-10-22.
- [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference) — `config.toml`
  key table (`approval_policy`, `sandbox_mode`, `sandbox_workspace_write.*`, `model_reasoning_*`,
  `shell_environment_policy.*`, `mcp_servers.<id>.required`, `projects.<path>.trust_level`,
  `history.*`, `features.*`, `suppress_unstable_features_warning`, provider keys), project-config
  trust rules, requirements keys; fetched 2026-10-22.
- [Environment variables](https://learn.chatgpt.com/docs/config-file/environment-variables) —
  `CODEX_HOME`, `CODEX_SQLITE_HOME`, `CODEX_API_KEY`, `CODEX_ACCESS_TOKEN`, `CODEX_CA_CERTIFICATE`,
  `SSL_CERT_FILE`, installer variables, `RUST_LOG`; fetched 2026-10-22 (markdown variant).
- [Feature Flags - Codex CLI](https://mintlify.wiki/openai/codex/cli/feature-flags) and
  [codex features - Codex CLI](https://mintlify.wiki/openai/codex/cli/features) — third-party
  Mintlify mirror of the docs source (page metadata: generated 2026-02-28 from commit `6a673e7`);
  used only for the documented stage vocabulary and the `--enable`/`-c`/`[features]` precedence;
  superseded by `features/src/lib.rs` for anything precise. Fetched 2026-10-22.

Indexed but not fetched (treated as hypothesis, not evidence):

- [openai/codex issue #33898](https://github.com/openai/codex/issues/33898) — "`--output-last-message`
  doesn't create parent dirs; write fails after the turn completes yet exits 0, silently losing the
  final message" (title as indexed, 2026-10-22).

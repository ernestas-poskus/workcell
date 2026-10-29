# Codex — experimental features and headless dispatch practice

> **Provenance.** Copied from the project's harness research (2026-10-22) and de-branded for this crate. This copy is maintained here.

## Scope and versions checked

- Installed CLI: **codex-cli 0.163.0** (`codex --version`), npm `@openai/codex`, macOS aarch64,
  native binary. Upstream release `rust-v0.163.0` was published **2026-10-15**
  ([release API](https://api.github.com/repos/openai/codex/releases/tags/rust-v0.163.0)).
- Upstream head on the research date is **0.164.0-alpha.12 / 0.164.0-alpha.11**; the 0.163.0 line
  also has `alpha.11.1` / `alpha.11.2` patch tags. So 0.163.0 is the current *stable* minor, not the
  newest artefact ([releases feed](https://github.com/openai/codex/releases.atom), accessed
  2026-10-22).
- Ground truth is the installed CLI: `codex --version`, `codex exec --help`, `codex features list`,
  `codex sandbox --help`, `codex exec resume --help`, `codex exec fork --help`, `codex debug --help`,
  `codex doctor --summary`, and probes that only list features or show help. No model was invoked.
- Source read at the installed tag `rust-v0.163.0`:
  [`exec_events.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/exec_events.rs),
  [`exec/src/lib.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/lib.rs),
  [`event_processor_with_jsonl_output.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/event_processor_with_jsonl_output.rs).
- Docs read on 2026-10-22 from the current (unversioned) docs set: non-interactive mode, agent
  approvals & security, auto-review, configuration reference, advanced config, feature maturity,
  prompt caching. These are *current* docs and can describe newer builds than 0.163.0; local
  `--help` wins where they disagree.
## Findings (by theme; tag each claim [fact|inference|hypothesis] and [high|med|low], inline citation)

### 1. The feature registry is a config namespace, not a promise

- `[fact][high]` `codex features list` prints `name`, `stage`, `effective state` for ~200 flags.
  Stages seen at 0.163.0: `under development`, `experimental`, `stable`, `deprecated`, `removed`
  (local `codex features list`). Official maturity guidance is blunt: **under development = "Not
  ready for use. Don't use."**; experimental = "Unstable… Use at your own risk"; stable = "safe for
  production use" ([Feature Maturity](https://learn.chatgpt.com/docs/feature-maturity.md)).
- `[fact][high]` `--enable <f>` / `--disable <f>` are documented as "Equivalent to `-c
  features.<name>=true|false`" (local `codex exec --help`), and `codex features enable|disable`
  writes `config.toml` (local `codex features --help`). The three inputs therefore target the same
  `[features]` table; config-file syntax is `[features]` / `features.<name> = true`
  ([Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md), `features.*` rows).
- `[fact][high]` Their precedence is *not* literally equivalent to `-c`. Local probes:
  `--disable goals --enable goals` and `--enable goals --disable goals` both report `false`
  (disable wins regardless of argv order); `-c features.goals=false --enable goals` reports `true`
  in either order (feature flags are applied after general overrides). So effective order is
  config.toml < `-c` < `--enable` < `--disable`. Measured with `codex features list` only; no model
  call.
- `[fact][high]` **"removed" means accepted for compatibility and ignored.** The config reference
  labels `features.personality` "Removed feature flag. Accepted for compatibility and ignored."
  Probe: `codex features list --enable apply_patch_freeform` exits 0 with no warning and no effect,
  while `--enable definitely_not_a_feature` exits 1 with `Error: Unknown feature flag`. The third
  column on a removed row is a stale value, not behaviour: `collaboration_modes` and `tui_app_server`
  print `true` while other removed flags print `false`.
- `[inference][med]` The registry is therefore safe to *pin* against future defaults (e.g. keep
  `features.apps=false`) but unsafe to *rely on* for behaviour: a removed flag silently does nothing.
  Any config that depends on a feature should be re-checked with `codex features list` after an
  upgrade.
- `[fact][high]` Enabling an under-development flag has a supported warning suppressor
  (`suppress_unstable_features_warning`), which implies enabling one emits a warning by default
  ([Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md)). In
  `exec --json`, config warnings are emitted as `item.completed` with `item.type="error"`
  (source: `ConfigWarning` arm of the JSONL processor), so a parser must not treat an error-shaped
  item as fatal.

### 2. Flags that are decision-relevant to headless dispatch

Short list; full table below.

- `[fact][high]` `goals` (stable, on) enables "persisted goals and automatic continuation"
  ([Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md)). For a
  one-shot dispatch that must end at one turn, this is a continuation surface, not a requirement;
  `--disable goals` is the determinism lever. Whether exec allows a goal to continue past its first
  turn is unverified.
- `[fact][high]` `multi_agent` (stable, on) enables `spawn_agent`, `send_input`, `resume_agent`,
  `wait_agent`, `close_agent`; `multi_agent_v2` is stable but **off**. A dispatch that spawns
  subagents pays extra tokens and wall clock; `--disable multi_agent` is the cost/ops lever.
- `[fact][high]` `exec_permission_approvals`, `code_mode`, `code_mode_only`, `context_management`,
  `async_compaction`, `credential_masking`, `token_budget`/`rollout_budget`, `agent_message_board`,
  `skip_host_skill_discovery` are all **under development, off** at 0.163.0. Only `code_mode_host`
  is stable/on, and it is inert without `code_mode`. `features.context_management.experimental_mode`
  is documented as "The feature is not currently available."
- `[fact][high]` `network_proxy` is experimental and off. It enforces domain rules *only* when
  command network access is already enabled; it does not grant network access
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md),
  [Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md)). Under a
  no-sandbox dispatch it is irrelevant.
- `[fact][high]` `guardian_approval` (stable, on) is the auto-review reviewer. It only acts when
  approvals are interactive; with `approval_policy="never"` "there is nothing to review"
  ([Auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review.md)).
- `[fact][high]` `fast_mode`/`ultrafast_mode` (stable, on) only "enable controls advertised by the
  active model"; they are not a spend switch. Speed tiers are selected elsewhere (`service_tier`).
- `[fact][high]` `daemon_auto_start` (stable, on) manages the shared local app-server daemon.
  `exec` itself drives an in-process app-server client (source: `InProcessAppServerClient` in
  `exec/src/lib.rs`), so `[inference][med]` a dispatch should not need the daemon — but a host with
  the daemon on may share thread/state services with it. Not verified.
- `[fact][high]` `hooks` (stable, on) loads lifecycle hooks from `hooks.json`/inline TOML; project
  hooks load only when the project `.codex/` layer is trusted
  ([Advanced Config](https://learn.chatgpt.com/docs/config-file/config-advanced.md)). A dispatch in a
  fresh checkout therefore has two unknowns: whether project config is trusted, and whether hooks
  run. `--dangerously-bypass-hook-trust` exists specifically for automation that has vetted hook
  sources (local `exec --help`).
- `[fact][high]` `memories` is stable but **off**; `plugins`, `remote_plugin`, `plugin_sharing`,
  `apps`, `browser_use*`, `computer_use`, `in_app_browser`, `image_generation` are stable and on.
  None is needed to edit a checkout from a prompt, but each expands prompt/tool surface. Disabling
  them is a reproducible-prefix and least-tool-surface measure — an inference, not a documented
  requirement.

### 3. Approval and sandbox mechanics at 0.163.0

- `[fact][high]` `codex exec` **already defaults to `approval_policy = never`**: the source sets
  `approval_policy: Some(AskForApproval::Never)` with the comment "Default to never ask for
  approvals in headless mode" (`exec/src/lib.rs`). The one exception is auto-review: if the resolved
  `approvals_reviewer` is `auto_review`, `build_exec_config` rebuilds without the forced `never`
  (same file).
- `[fact][high]` `--dangerously-bypass-approvals-and-sandbox` sets `sandbox_mode =
  danger-full-access` (source). Approval is already `never` in exec, so in `exec` the flag's real
  effect is "no OS sandbox"; the docs' "no sandbox; no approvals" describes the TUI too
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md)).
- `[fact][high]` `--approve-for-me` is *not* a bypass. It routes approvals through the workspace-write
  sandbox plus a reviewer agent (`approvals_reviewer = "auto_review"`), only for eligible interactive
  approvals; denials return a rationale and the agent is told to find a safer path; failures fail
  closed; it "does not expand `writable_roots`, enable network access, or weaken protected paths"
  ([Auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review.md)). In `exec`, approval
  server-requests that do surface are rejected with "command execution approval is not supported in
  exec mode" (`exec/src/lib.rs`), so a reviewer is the only thing that can resolve them.
  `[inference][med]` `--approve-for-me` is a real headless option but adds reviewer model calls and a
  sandbox boundary; it is not demonstrated practice for a fully unattended dispatch.
- `[fact][high]` `workspace-write` keeps network **off** unless `[sandbox_workspace_write]
  network_access = true`; `.git`, `.codex`, `.agents`, `.aws` under a writable root stay read-only,
  so `git commit` fails inside that sandbox
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md),
  [Advanced Config](https://learn.chatgpt.com/docs/config-file/config-advanced.md)).
- `[fact][high]` `approval_policy = "untrusted"` is retired and "can prevent either client from
  starting"; `on-failure` is deprecated in favour of `on-request`/`never`
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md),
  [Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md)). A stale
  checkout-local or profile config carrying it is a hard startup risk.
- `[fact][high]` The OS sandbox can fail to *start*, not just deny a command. On this macOS host
  `codex sandbox -- true` exits **71** with `sandbox-exec: sandbox_apply: Operation not permitted`
  (nested sandbox refused). Docs acknowledge the same class on Linux containers where bwrap/seccomp
  are blocked and recommend running with `--sandbox danger-full-access` *inside* the container
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md)).
  `[inference][high]` This is an argument for the bypass flag on a containerised run host: it
  removes a failure mode that is unrelated to the agent's task.

### 4. Reproducibility knobs

- `[fact][high]` `--ignore-user-config` skips `$CODEX_HOME/config.toml`; **auth still uses
  `CODEX_HOME`** (local `exec --help`). `--ignore-rules` skips user and project execpolicy `.rules`.
  `--strict-config` errors on unrecognised `config.toml` fields (same help).
- `[fact][high]` `--ignore-user-config` does **not** skip project-scoped `.codex/config.toml` layers;
  those load when the project is trusted, and are a separate loader layer (source `LoaderOverrides`
  has `ignore_project_config` used only for the `--worktree` gate; docs: project config "loads only
  when you trust the project"). `[inference][high]` This is the layer a `-c mcp_servers={}` override is aimed at: project-local MCP servers that `--ignore-user-config` cannot remove.
- `[fact][high]` Profiles are now V2: `-p/--profile <name>` overlays
  `$CODEX_HOME/<name>.config.toml` on the base user config; `[profiles.*]` in `config.toml` is not
  read since 0.134.0 ([Advanced Config](https://learn.chatgpt.com/docs/config-file/config-advanced.md)).
  `exec resume`, `exec fork`, `exec review` do **not** accept `-p` (local help), so a profile cannot
  be applied on a resumed run — it must be restated with `-c`, or not used at all.
- `[fact][high]` `--worktree` is rejected with `--ignore-user-config` and with `--ephemeral`, and is
  not supported on `exec resume`/`exec review` (source `bail!`s). These are runtime checks, not clap
  conflicts: the flags parse, then the process exits non-zero.
- `[fact][high]` `-c` values are parsed as TOML, falling back to a string; dotted paths set nested
  keys ([Advanced Config](https://learn.chatgpt.com/docs/config-file/config-advanced.md)). Every `-c` override uses this, so it is one quoting bug away from silently becoming a string — `--strict-config` in a smoke test is the cheap detector.
- `[fact][high]` Without a prompt argument (or with `-`), exec reads the prompt from stdin; with a
  prompt *and* piped stdin it appends a `<stdin>…</stdin>` block (local help; source
  `resolve_root_prompt`). `[inference][high]` A positional prompt plus a null stdin is the safe pattern; a stray pipe would silently append context and change the prefix.

### 5. Exec JSONL contract at 0.163.0

Primary source: `exec_events.rs` and `event_processor_with_jsonl_output.rs` at `rust-v0.163.0`, plus
the docs' sample ([Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode.md)).

- `[fact][high]` Event variants are exactly `thread.started` (first, carries `thread_id`),
  `turn.started`, `turn.completed`, `turn.failed`, `item.started`, `item.updated`, `item.completed`,
  `error`. There is **no `token_count` event** in this contract (the `ThreadEvent` enum is closed). A parser configured for `info.last_token_usage.*` is targeting a legacy Codex-v1 shape; the `turn.completed.usage.*` path is what reports on 0.163.0.
- `[fact][high]` Item types: `agent_message`, `reasoning`, `command_execution`, `file_change`,
  `mcp_tool_call`, `collab_tool_call`, `web_search`, `todo_list`, `error`. `item.updated` is emitted
  only for `todo_list`. `file_change` and `web_search` complete as `item.completed`
  (`web_search` also has `item.started`). Empty reasoning summaries are suppressed.
- `[fact][high]` `turn.completed` carries `usage.{input_tokens, cached_input_tokens,
  cache_write_input_tokens, output_tokens, reasoning_output_tokens}`, but
  `usage_from_last_total()` copies **`ThreadTokenUsage.total`** — i.e. the *thread's running total*,
  not this turn's usage (source; corroborated by open issue
  [#49574](https://github.com/openai/codex/issues/49574), open since 2026-09-30, which shows the
  same mapping at 0.159.2 and main).
- `[fact][high]` Non-fatal warnings (`ConfigWarning`, `Warning`, `DeprecationNotice`,
  `ModelRerouted`) are emitted as `item.completed` with `item.type="error"`; `error` notifications
  are non-terminal. Only `turn.failed`, or `turn.completed` with status Failed/Interrupted, ends the
  process (source). `[inference][high]` A log classifier must key on `turn.failed`/exit code, not on
  the presence of an `error` line.
- `[fact][high]` An **interrupted** turn emits *no* terminal event — the processor goes straight to
  shutdown (source). The process still exits 1 because the exec loop flags Interrupted
  (`exec/src/lib.rs`), but a JSONL consumer waiting for a terminal line will wait forever if the
  child is signalled.
- `[fact][high]` `-o/--output-last-message` is written only on a completed turn
  (`emit_final_message_on_shutdown`); on failure/interruption no file is written (source). With
  `--output-schema`, the final `agent_message.text` is a JSON string, and if no agent message is
  present the final message falls back to a plan item's text (source) — a schema consumer can
  therefore receive plan prose rather than the requested object.
- `[fact][high]` All non-JSON output goes to stderr; stdout must be one JSON object per line
  (`exec/src/lib.rs` header). `[inference][high]` Enabling unstable features may add warning items
  (section 1), which is cosmetic for parsers but changes the prefix/cache only if prompts change.

### 6. Exit codes

- `[fact][high]` exec exits **1** when a non-retry `Error` notification was seen, when the turn
  completed with status Failed or Interrupted, or when an approval/server request was rejected;
  otherwise **0** after a completed turn (source). Transient `Error` with `will_retry=true` does not
  set the failure flag, so a run that recovers still exits 0. Clap-level invalid flags exit **2**
  (probe: `--full-auto`). `[inference][high]` Exit 0 means "the turn ended", not "the task succeeded"; a separate diff/verify gate remains necessary.

### 7. Model and effort practice

- `[fact][high]` `codex debug models` (refreshed, read-only) on this account at 0.163.0 returns 9
  slugs: `gpt-6.1-sol`, `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`,
  `gpt-5.6-luna`, plus hidden `gpt-reserve`, `codex-auto-review`. The bundled catalog adds `gpt-5.5`
  and hidden Daybreak variants. **`gpt-5.3-codex` is in neither list.**
- `[fact][high]` `model_reasoning_effort` accepts model-advertised levels `low, medium, high, xhigh,
  max, ultra` (config reference; bundled catalog). Catalog defaults: `low` for `gpt-6.1-sol`,
  `gpt-6-astra`, `gpt-5.6-sol`; `medium` for `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-terra/luna`.
  `ultra` is described as "Maximum reasoning with automatic task delegation". `model_reasoning_summary`
  is `auto|concise|detailed|none`; `model_verbosity` is `low|medium|high` (Responses API only)
  ([Config Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md)).
- `[inference][med]` With no explicit effort, the newest workhorse models run at **low** — cheaper
  but weaker for a multi-file implementation. Pinning effort is a reproducibility and quality
  decision, not only a cost one. (Cannot verify what exec does when the slug is unknown; no model
  call was made.)
- `[fact][high]` `codex doctor --summary` gives a preflight with auth, sandbox, MCP and config
  checks; `codex doctor --json` is redacted. On the research host it reported auth configured,
  6 MCP servers (4 stdio, 2 HTTP, 1 disabled), "restricted fs + enabled network · approval Never",
  and a failed state-DB integrity check with thread rows unqueryable. `[inference][med]` A
  `doctor`-based preflight is cheap and catches auth/config breakage before a billed run.

### 8. Prompt cache and warm reuse

- `[fact][high]` OpenAI prompt caching is automatic, and `prompt_cache_retention` selects the retention mode; cached tokens still count toward tokens-per-minute limits ([Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching.md)). Cache lifetime and pricing policy belong to the companion caching research shipped with the `warmpath` crate; this note records only the surface.
- `[fact][high]` Reuse requires the rendered prefix to match: model, tool set/order/descriptions,
  `parallel_tool_calls`, structured-output format, reasoning effort and verbosity can all change the
  cached prefix ([same](https://developers.openai.com/api/docs/guides/prompt-caching.md)).
  `[inference][high]` A stable prompt + fixed `-m` + a fixed tool/config set is the right shape; adding per-run `-c` values that alter tools/format (e.g. `--output-schema`) can make a retry miss the prefix even at the same thread.
- `[fact][high]` Cache reads are billed at a discount but do not reduce TPM accounting; `[inference]
  [med]` a resumed run can therefore still hit a rate limit even with a high hit rate.

### 9. Resume and fork semantics

- `[fact][high]` `codex exec resume <id|name>` and `codex exec fork <id>` are separate subcommands.
  **Neither accepts `-C/--cd`, `-s/--sandbox`, `-p/--profile`, or `--color`** (local help). Both
  accept `-c`, `-m`, `--json`, `-o`, `--output-schema`, `--skip-git-repo-check`,
  `--ignore-user-config`, `--ignore-rules`, `--ephemeral`, and the two `--dangerously-bypass-*` flags.
- `[fact][high]` Lookup rules (source `resolve_resume_thread_id`): a value that parses as a UUID is
  used directly; `--last` and name/title lookups go through the thread list, filter by cwd unless
  `--all`, and exclude archived threads. `[inference][high]` Explicit `thread_id` capture is the only resume form that survives a broken state DB, a moved checkout, or archiving.
- `[fact][high]` `--ephemeral` writes no session files (local help), so an ephemeral run cannot be
  resumed. `[inference][high]` Do not add `--ephemeral` to a run that relies on retry/warm resume.
- `[fact][high]` `exec resume` does not inherit the sandbox mode, and since it rejects `-s`, the mode
  must be restated with `-c sandbox_mode="…"` when a sandbox is used
  ([practitioner account, UGC](https://github.com/Enderfga/claw-orchestrator/commit/cca8a5860ab5889df8f1bcddd61b8b4b29221778.patch),
  section 0.147.0 notes). The bypass flag *is* accepted on resume, so a full-access run keeps full access.
- `[fact][high]` When `-C` is omitted, exec builds the resume request with `cwd = Some(config.cwd)`,
  and `config.cwd` is the process cwd when no `-C` was given — it therefore overrides the persisted
  thread cwd (open issue [#40702](https://github.com/openai/codex/issues/40702), filed 2026-08-25 by
  a maintainer-adjacent reporter; source confirms the `cwd: Some(config.cwd…)` construction).
  `[inference][high]` A no-`-C` resume is only safe when the child is launched with cwd set to the intended workspace and warm reuse is refused across workspaces; the mechanism is "process cwd wins", not "persisted cwd is preserved".
- `[fact][high]` `exec fork` creates a new thread from an existing one (new `thread_id`) and shares
  the conversation prefix, so it can be cheaper than a fresh thread for a related task; `[inference]
  [med]` it is a plausible tool for a follow-up dispatch but is untested here.

## Feature-gate table (feature | stage | default | dispatch relevance | source)

"Default" is the third column of `codex features list` on the research host (an *effective* state;
the user config may contribute, so read it as default-plus-host). Stage and relevance are from the
installed CLI and docs; relevance is my inference unless quoted.

| feature | stage | default | dispatch relevance | source |
|---|---|---|---|---|
| `goals` | stable | on | persisted goals + automatic continuation; disable for strict one-turn determinism | config ref; local list |
| `multi_agent` | stable | on | spawn/send/wait/close-agent tools; extra cost; disable to isolate | config ref; local list |
| `multi_agent_v2` | stable | **off** | v2 collaboration; leave off | local list |
| `multi_agent_v2_dynamic_tools` | under development | off | — | local list |
| `hooks` | stable | on | project hooks load only if project trusted; reproducibility/trust risk; consider `false` | advanced config; local list |
| `apps` | stable | on | connector/app tools; not needed for a prompt-driven edit; `mcp_servers={}` does not disable apps | config ref; local list |
| `browser_use`, `browser_use_external`, `browser_use_full_cdp_access` | stable | on | browser tooling; not needed headless | local list |
| `computer_use` | stable | on | native-app tooling; not needed headless | local list |
| `guardian_approval` | stable | on | auto-reviewer; inert unless `--approve-for-me`/on-request | auto-review docs |
| `fast_mode`, `ultrafast_mode` | stable | on | speed-tier *controls*, not activation; not a spend switch | config ref |
| `daemon_auto_start` | stable | on | shared app-server daemon; exec uses in-process client — verify on host | local list; exec source |
| `worktrees` | stable | on | `exec --worktree`; mutually exclusive with `--ignore-user-config` | exec source |
| `memories` | stable | off | cross-session memory; keep off for reproducibility | local list |
| `plugins`, `remote_plugin`, `plugin_sharing` | stable | on | plugin catalog/tools; extra surface | config ref; local list |
| `exec_permission_approvals` | under development | off | exec approval routing; keep off | local list |
| `code_mode` | under development | off | model writes code against tool APIs; changes tool surface; keep off | config ref; local list |
| `code_mode_host` | stable | on | code-mode host; inert without `code_mode` | local list |
| `code_mode_only`, `code_mode_only_strict_3p_tools`, `code_mode_prewarm`, `code_mode_tool_search` | under development | off | keep off | local list |
| `context_management` | under development | off | docs: "not currently available" | config ref |
| `async_compaction` | under development | off | background compaction; unneeded for one-shot | local list |
| `credential_masking` | under development | off | log masking; a caller may already scrub env | local list |
| `collaboration_modes` | removed | true | accepted and ignored; value meaningless | config ref ("accepted for compatibility and ignored") |
| `agent_message_board` | under development | off | inter-agent messaging | local list |
| `network_proxy` | experimental | off | enforces domains only if command network already on; irrelevant under full access | agent security docs |
| `token_budget`, `rollout_budget` | under development | off | potential hard token cap; not production-ready | local list; config ref |
| `skip_host_skill_discovery` | under development | off | host-skill injection into prefix; determinism candidate, unverified | local list |
| `worktree` / `apply_patch_*` | removed | — | ignored; do not depend on them | config ref; probe |

## Approval/sandbox matrix

At 0.163.0 the exec default is `approval_policy=never` unless auto-review is configured (source).
"Matrix" therefore compares *sandbox and reviewer* choices, not approval prompts.

| mode / flags | filesystem | network | approvals | verdict for a headless dispatch |
|---|---|---|---|---|
| `--dangerously-bypass-approvals-and-sandbox` (hidden alias `--yolo`) | unrestricted | direct | none (`never` in exec) | **Full-access choice.** Also avoids OS-sandbox startup failure (macOS seatbelt EPERM observed; Linux container caveat documented). Trust the external boundary. |
| `--sandbox danger-full-access` (without bypass) | unrestricted | direct | `never` in exec | Effectively the same in `exec`; in the TUI it is not the same flag. Adds nothing for dispatch. |
| `--sandbox workspace-write -a never` | workspace writable; `.git`, `.codex`, `.agents` and `.aws` under it stay read-only | **off** | none | Would fail on `git commit` and any network fetch; plan's tests needing network fail. |
| `--sandbox workspace-write -a never -c sandbox_workspace_write.network_access=true` | same | on, direct | none | Works for builds needing network; OS sandbox must be able to start. |
| `--sandbox read-only -a never` | reads only | off | none | Only for verification/review, not implementation. |
| `--approve-for-me` | workspace-write | off unless enabled | reviewer agent (`auto_review`), interactive policy | Adds reviewer model calls, fails closed, keeps the boundary; not a bypass. Untested headless. |
| `-c sandbox_mode="…"` on `exec resume` | restated | restated | restated | Required if a deployment uses a sandbox at all: resume rejects `-s` and does not inherit the mode. |

## Resume and usage accounting (the harness contract)

- **Contract.** `thread.started{thread_id}` → … → `turn.completed{usage}` / `turn.failed{error}`. Use
  the `thread_id` as the prefix key, and treat `item.completed` `agent_message` (with `item.phase`
  if present) as the summary. The docs' sample matches this shape
  ([Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode.md)).
- **The accounting trap.** `turn.completed.usage` is the **thread total**, not the turn. A parser
  that treats it as this run's usage over-reports on any resumed thread; the fix is either to
  baseline-subtract a previously captured total or to read per-turn `last_token_usage` from the
  rollout file, which the JSONL stream does not expose. Primary evidence: source
  `usage_from_last_total()` + open issue
  [#49574](https://github.com/openai/codex/issues/49574); practitioner measurements: 13,856 → 27,727
  → 41,613 for three identical trivial turns on 0.147.0
  ([UGC commit](https://github.com/Enderfga/claw-orchestrator/commit/cca8a5860ab5889df8f1bcddd61b8b4b29221778.patch)).
- **The legacy shape.** `token_count` / `info.last_token_usage.*` is not emitted by 0.163.0's exec
  JSONL (closed event enum); a parser keeps working only through a `usage.*` fallback.
- **Resume validity checklist.** thread id must exist in `CODEX_HOME` (state DB or rollout); the run
  must not have been `--ephemeral`; a name/`--last` resume must match cwd unless `--all` and must not
  be archived; `-C`, `-s`, `-p` are unavailable, so cwd comes from the child process and sandbox mode
  must be restated with `-c`. `fork` is the same shape but produces a new `thread_id`.
- **Cost/quota reporting.** exec JSONL reports tokens only (thread total, including
  `cache_write_input_tokens` at 0.163.0); it has no quota/window fields. Rate-limit failures arrive
  as `error`/`turn.failed` messages, and quota windows must be measured outside the CLI.
- **Cache lifetime and pricing** are provider policy, not CLI surface; the companion caching research
  shipped with the `warmpath` crate owns them. The CLI facts that matter here are the prefix key
  (`thread_id`), the event shape, and the fact that a change to model, tool set or output format can
  move the cache boundary.

## Failure modes and practitioner reports (label UGC)

Primary issues (open, maintainer-tracked):

- `[fact][high]` **Review-path hang.** `codex exec review --base` on a Git failure emits the correct
  JSONL `error` and then never ends the turn: no terminal event, process alive until an external
  watchdog kills it. Reported on 0.152.0/0.151.0, still open at the research date, with a
  source-level explanation (the review error path never produces `TurnCompleted`)
  ([#41984](https://github.com/openai/codex/issues/41984)). Not the dispatch path, but it means
  "exec always terminates" is not a safe assumption for every exec subcommand; a supervising caller should keep its own timeout.
- `[fact][high]` **Resume cwd override.** `exec resume` without `-C` sets the thread cwd to the
  process cwd, silently rebinding a thread created in another checkout
  ([#40702](https://github.com/openai/codex/issues/40702), open).
- `[fact][high]` **Cumulative usage contract.** `turn.completed.usage` is a thread total although the
  SDK type describes per-turn usage; open issue
  [#49574](https://github.com/openai/codex/issues/49574) (0.159.2, still present on main).

Practitioner/UGC (hypotheses, useful only as corroboration):

- `[UGC][med]` A `codex exec` wrapper found it was summing thread totals (cost overstated ~(N+1)/2,
  3.6× on a 7-turn session), and that codex enforces its own context window (~258,400 for
  gpt-5.6-sol) far below the model's published 1.05 M — neither number appears in exec JSONL, only in
  the rollout file ([commit](https://github.com/Enderfga/claw-orchestrator/commit/cca8a5860ab5889df8f1bcddd61b8b4b29221778.patch),
  tested on 0.147.0).
- `[UGC][med]` A detailed `exec --json` event cheatsheet (tested 0.157.1) documents the event/item
  shapes, the 64 KiB `aggregated_output` truncation, duplicate `id` keys on `web_search` items, and
  warns that `turn.completed.usage` is the thread total
  ([untether cheatsheet](https://github.com/littlebearapps/untether/blob/master/docs/reference/runners/codex/exec-json-cheatsheet.md)).
  The shapes agree with 0.163.0 source; the version is older, so treat version-specific details as
  hypotheses.
- `[UGC][low]` A GitHub Actions project reports a Codex harness retry loop draining tokens on
  exhausted rate-limit reconnects, fixed by capping retries after `Reconnecting... X/Y` notices
  ([PR](https://github.com/github/gh-aw/pull/41385)); consistent with the source fact that a
  `will_retry` error does not fail the run. Not verified against 0.163.0.

Documented failure modes:

- `[fact][high]` Auth in automation: use `CODEX_API_KEY` scoped to the invocation, never a job-level
  env var; `~/.codex/auth.json` is password-equivalent; `--ignore-user-config` keeps auth from
  `CODEX_HOME` ([Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode.md),
  local help). A runner with neither fails before any model call.
- `[fact][high]` An enabled MCP server with `required = true` that fails to initialise makes `codex exec` exit with an error rather than continuing ([Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode.md)). Keeping `required = true` off any server a run may inherit avoids the class.
- `[fact][high]` A workspace-write sandbox blocks `git commit` (`.git` read-only) and network by
  default; approval requests that surface in exec are rejected, not answered
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md); exec
  source). `[inference][high]` This produces "the agent says it committed, the tree is clean, the
  commit does not exist" failures unless the prompt forbids committing and the verifier checks
  commits — a verifier should do both.
- `[fact][high]` Windows/desktop/daemon surfaces (`app-server`, `remote-control`, `queue`, `agents`,
  `archive`, `delete`, `cloud`, `exec-server`) exist but are experimental/irrelevant to a
  one-shot dispatch; `queue` can inject a message into an existing session, and `archive` makes a
  thread unresumable by name/`--last` (local `--help`; source lookup rules). Keep a one-shot run on `exec`/`exec resume` only.
- `[inference][med]` Project trust is the remaining unknown in a fresh checkout: project
  `.codex/config.toml`, project hooks and project rules load only when trusted, and `exec` cannot
  answer a trust prompt. Treat project-local layers as unknown and neutralise them from argv.

## Contradictions and version drift

- **Docs vs local help (sandbox subcommands).** Current docs show `codex sandbox macos|linux|windows`
  ([Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md)). At
  0.163.0 `codex sandbox --help` has only `uninstall` plus a positional command; `codex sandbox macos`
  treats `macos` as the command (exit 71 on this host). Local help wins: no platform subcommands.
- **Docs vs local help (`--yolo`).** Docs call `--yolo` an alias; it is accepted (probe exits 0) but
  is not listed in `codex exec --help`. Hidden aliases should not be relied on in a committed
  template.
- **Docs vs local help (`--full-auto`).** Docs say it is gone; 0.163.0 rejects it with clap exit 2.
  Agrees, but the failure mode is an argv error, not a deprecation warning.
- **Feature stage vs CLI surface.** `remote_control` is a **removed** feature flag (value `false`),
  yet `codex remote-control` is listed as `[experimental]` in top-level help and `daemon_auto_start`
  is stable. The flag being "removed" does not mean the CLI surface is gone — do not infer one from
  the other.
- **Docs' "equivalent to `-c features.x=true`" vs observed precedence.** The `-c` form loses to
  `--enable`, and `--disable` beats `--enable` regardless of order (local probes). "Equivalent" is
  syntactic, not a precedence guarantee.
- **A stale model pin.** `gpt-5.3-codex` is not in the bundled or refreshed catalog; notes that cite measured runs on it are from an earlier model generation.
- **Current docs are newer than the installed binary.** `learn.chatgpt.com` describes features
  (`prompt_cache_options`, `features.network_proxy` table form, auto-review config) that are not part
  of 0.163.0's `--help` surface; the config reference is a superset. Any doc claim about a CLI flag
  was checked against `--help` before use here.

## Gaps / could not verify

- No model was invoked (constraint), so runtime behaviour is unverified for: `--approve-for-me` in
  `exec`, goal auto-continuation beyond one turn, hook trust in a fresh checkout, actual cache-hit
  rates and warm-TTL savings, and whether `gpt-5.3-codex` still resolves as a legacy server alias.
- Project-trust default for `exec` (does a never-seen checkout count as trusted?) could not be
  established without a run; only the config-loading rule (trusted → project layers) is documented.
- 0.163.0's release-note body could not be read: the GitHub API response was truncated at the asset
  list cap, and the Atom feed only carries the alpha entries. No changelog-level claims are made.
- The model catalog is account/workspace-dependent; absence from the list is not proof that an API
  rejects the slug.
- `~/.codex/config.toml` was not read (policy); effective user-config facts come only from `features
  list` and `doctor --summary`.
- Feature "default" values in the table may include host config (the third column is *effective*
  state); a clean-host default could differ for `memories`, `plugins`, `recommended_plugins`.
- Under-development flags were not enabled, so their runtime effects are documented-only.

## Sources (accessed 2026-10-22, with version/commit where known)

- Local `codex-cli 0.163.0`: `codex --version`, `codex exec --help`, `codex features list`,
  `codex features --help`, `codex sandbox --help`, `codex exec resume --help`,
  `codex exec fork --help`, `codex debug --help`, `codex debug models --bundled`,
  `codex doctor --summary`; probes with `codex features list` (enable/disable precedence, removed and
  unknown flags), `codex exec --yolo --help`, `codex exec --full-auto --help`,
  `codex sandbox -- true` (exit 71). 0.163.0, macOS aarch64.
- OpenAI Codex source at tag `rust-v0.163.0`:
  [`codex-rs/exec/src/exec_events.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/exec_events.rs);
  [`codex-rs/exec/src/lib.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/lib.rs);
  [`codex-rs/exec/src/event_processor_with_jsonl_output.rs`](https://raw.githubusercontent.com/openai/codex/rust-v0.163.0/codex-rs/exec/src/event_processor_with_jsonl_output.rs).
- [Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode.md), ChatGPT/Codex docs,
  accessed 2026-10-22 (current, unversioned).
- [Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md), accessed
  2026-10-22.
- [Auto-review](https://learn.chatgpt.com/docs/sandboxing/auto-review.md), accessed 2026-10-22.
- [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md), accessed
  2026-10-22.
- [Advanced Configuration](https://learn.chatgpt.com/docs/config-file/config-advanced.md), accessed
  2026-10-22.
- [Feature Maturity](https://learn.chatgpt.com/docs/feature-maturity.md), accessed 2026-10-22.
- [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching.md), OpenAI API docs,
  accessed 2026-10-22.
- [openai/codex release `rust-v0.163.0`](https://github.com/openai/codex/releases/tag/rust-v0.163.0)
  and [releases feed](https://github.com/openai/codex/releases.atom): 0.163.0 published 2026-10-15;
  0.164.0-alpha.12 current on 2026-10-22.
- [openai/codex#49574](https://github.com/openai/codex/issues/49574), "TypeScript SDK: resumed
  turn.completed.usage returns thread totals…", opened 2026-09-30, open at access.
- [openai/codex#41984](https://github.com/openai/codex/issues/41984), "`codex exec review`… hangs
  forever", opened 2026-09-01, open at access (reproduced 0.152.0/0.151.0).
- [openai/codex#40702](https://github.com/openai/codex/issues/40702), "`codex exec resume` overrides
  persisted cwd when `-C` is omitted", opened 2026-08-25, open at access.
- [Feature Flags (docs mirror)](https://mintlify.wiki/openai/codex/cli/feature-flags), generated from
  openai/codex commit `6a673e7`, dated 2026-02-28 — secondary, used only for the stage vocabulary;
  its CLI examples (`js_repl`) are stale for 0.163.0.
- UGC: [untether `exec --json` cheatsheet](https://github.com/littlebearapps/untether/blob/master/docs/reference/runners/codex/exec-json-cheatsheet.md),
  tested against 0.157.1.
- UGC: [claw-orchestrator commit `cca8a58`](https://github.com/Enderfga/claw-orchestrator/commit/cca8a5860ab5889df8f1bcddd61b8b4b29221778.patch),
  "read codex token usage as the cumulative total it is", 2026-08-15, tested 0.147.0.
- UGC: [github/gh-aw#41385](https://github.com/github/gh-aw/pull/41385), rate-limit reconnect retry
  loop report, 2026 (adjacent project; unverified against 0.163.0).

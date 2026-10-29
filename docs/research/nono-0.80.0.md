# `nono` 0.80.0: the sandbox `workcell` delegates to

Scope: `nono` 0.80.0 as installed on this host (`/opt/homebrew/bin/nono`, verified `nono 0.80.0`)
and its source at tag `v0.80.0` (released 2026-10-13). This is upstream behaviour, not `workcell`
policy. `workcell`'s own handling is in [`workcell-audit.md`](workcell-audit.md); the probes that
exercise this are in [`../../scripts/escape-drill.sh`](../../scripts/escape-drill.sh) with rationale
in [`../escape-testing.md`](../escape-testing.md). No sandboxed child was run for this document.

## Identity and repository move

`[fact][high]` The project moved: `always-further/nono` now 301-redirects to
**`github.com/nolabs-ai/nono`** (~4.7k stars, not archived). Maintainers are Luke Hinds
(`@lukehinds`), Aleksy Siek, and Conor Gannon, under the Nolabs org (`MAINTAINERS.md` at the tag).
"Always Further" is the earlier brand/blog (`alwaysfurther.ai`); the repository, docs, and packs
are `nolabs-ai`. `v0.80.0` is the newest tag. A source citation pinned to `always-further/nono`
resolves, but a raw path should be pinned to the tag and the current org.

## Architecture

`[fact][high]` **macOS (Seatbelt).** `nono` does **not** write a `.sb` file or shell out to
`sandbox-exec`. It assembles an SBPL profile **string at runtime**, calls Apple's private
`sandbox_init()` through FFI (`unsafe extern "C" { fn sandbox_init(...) }`,
the `nono` crate's `src/sandbox/macos.rs` L24-26, L942-966), then execs. The base is `(deny default)`; it
also uses `sandbox_extension_issue_file` / `sandbox_extension_consume` for
`com.apple.app-sandbox.read*` extensions.

`[fact][high]` **Two execution modes, and they differ in enforcement.**

| Mode | Shape | Consequence |
|---|---|---|
| `nono run` / `nono shell` (**supervised**) | `nono` forks; the parent stays **unsandboxed** to provide audit, rollback, proxy, and diagnostics; only the child is sandboxed | domain-filtered networking works **only** here |
| `nono wrap` (**direct**) | apply the sandbox and exec; `nono` disappears | no supervisor, so no proxy filtering; audit/rollback unavailable |

`workcell` always builds `nono -s run --no-diagnostics …` (`src/sandbox.rs:584`), i.e. supervised.
A caller that switched to `wrap` would silently lose the egress allowlist.

`[fact][high]` **Linux** is Landlock (via the `landlock` crate: `Ruleset`, `PathBeneath`,
`AccessFs`/`AccessNet`, `Scope::Signal`/`Scope::AbstractUnixSocket`, ABI autodetect) plus a seccomp
BPF fallback for network policy (`SECCOMP_RET_ERRNO` block-all; user-notification for proxy-only
destination checks). No bubblewrap, no namespaces by default; NEP-0004 (draft, 2026-09-29) proposes
opt-in user+net namespaces, permanently default-off. `workcell` currently refuses non-macOS hosts
(§ Platform limits), so the Linux path is not exercised.

## Profile semantics

`[fact][high]` Composition, from the schema and `docs/cli/features/profiles-groups.mdx`:

```text
((default_groups + groups.include) - groups.exclude)
  + filesystem.{allow, read, write}
  + commands.allow
  - (deny_groups + filesystem.deny + commands.deny)
  + bypass_protection
  + CLI flags
```

| Key | Semantics |
|---|---|
| `extends` | string \| array \| null; bases merged left-to-right, child wins; `filesystem`, `groups`, `commands`, `diagnostics` are **additive** across inheritance; `network.network_profile: null` clears an inherited profile |
| `groups.include` / `exclude` | names from `policy.json`; **exclusion wins** over inclusion |
| `workdir.access` | `none` \| `read` \| `write` \| `readwrite`, default `none`; `$WORKDIR` expands to `--workdir` or cwd |
| `filesystem.allow` | read+write directory, recursive |
| `filesystem.read` | read-only directory, recursive |
| `filesystem.allow_file` | read+write **single file** (this is why `antigravity-oauth-token` is writable) |
| `filesystem.read_file` | read-only single file |
| `filesystem.deny` | deny rules; path/pattern based, glob expanded at start |
| `bypass_protection` | removes a **deny**; still requires a matching grant, and non-existent paths are dropped with a warning |

`[fact][high]` **Deny beats allow.** Schema: "Denies apply regardless of other allow rules."
Verified statically on 0.80.0:
`nono why --path ~/Library/Keychains --op read --allow ~/Library` → `DENIED … Policy:
filesystem.deny`.

`[fact][high]` A deny glob nested inside a writable grant works on macOS — it is compiled to an
anchored Seatbelt `regex` emitting `deny file-read-data`, `deny file-write*`, `deny
network-outbound` (the `nono` CLI crate's `src/policy.rs` L1004-1047), so it also covers files created
**after** sandbox start. On Linux the same profile only covers paths that existed at start, because
Landlock has no deny primitive; the source warns about it and the schema says so.

`[fact][high]` On macOS the deny is `file-read-data` / `file-write*`, **not** `file-read-metadata`:
`stat`/`test -d` on a denied path stays allowed by design ("allow discovery, deny content"). Any
deny is therefore not a confidentiality boundary.

`[fact][med]` Whether `bypass_protection` lifts `filesystem.deny` or only group-level denies is
ambiguous between the schema ("group-level deny rules") and the Seatbelt doc (implies any deny).
`workcell` relies on it only for the two keychain DBs.

## The `network.block: false` correction

This is the most misread key in the profile, so state it precisely.

`[fact][high]` **`block: false` is not an "allow everything" switch, and `allow_domain` does not
merely annotate — it activates the proxy.** Any of `network_profile`, `allow_domain`,
`deny_domain`, `credentials`, or `upstream_proxy` activates proxy mode; activation sets
`NetworkMode::ProxyOnly` (the `nono` CLI crate's `src/proxy_runtime.rs` L3094), which on macOS emits
`(deny network*)` followed by `(allow network-outbound (remote tcp "localhost:PORT"))`
(`macos.rs` L866-895). `block` only adds the hard block on top.

`[observed][high]` Static policy evaluation of `workcell`'s real profile (no sandboxed child):

```console
$ nono run --dry-run -p src/sandbox/profiles/agent.profile.json --workdir "$PWD/tmp" -- /bin/echo hi
  …
  deny  7 sensitive paths kept blocked inside your grants (-v to show)
  net  proxy
  ipc  localhost:0
```

`net proxy` is the kernel rule above. A direct off-box connection is therefore denied, and a child
that ignores `HTTPS_PROXY` does **not** reach the internet. The earlier hypothesis "the allowlist is
advisory; any non-proxy-aware client escapes" is **refuted** for supervised mode.

`[fact][med]` Two caveats keep this from being a blanket claim:

- With no proxy inputs at all the same profile shape resolves to `net outbound allowed`; the block
  comes from proxy activation, not from `block: false`.
- `block: true` **plus** proxy inputs is not blocked — proxy wins, with a warning (`block_wins` only
  when the proxy would not activate). `--block-net` + `--allow-domain` is a hard CLI error
  ("contradictory: domain filtering requires proxy mode"), but the profile path warns and proxies.

## `open_port` and `ipc localhost:0`

`[fact][high]` CLI: `--open-port <PORT>` = "Allow **bidirectional localhost TCP** on a port: connect
+ listen"; `--listen-port` = listen only; `--allow-connect-port` = connect only (Linux Landlock
V4+). Port `0` means a wildcard and is macOS-only.

`[observed][high]` `workcell`'s profiles set `open_port: [0]` (`src/sandbox/profiles/agent.profile.json:21`,
`review.profile.json:21`), which resolves to `ipc localhost:0` — connect **and** listen on **every**
loopback port. That is broader than "the local compression proxy's port": it reaches any local
proxy, admin/DB service, MCP server, or the supervising harness's own localhost listeners, and it
lets the child bind ports a later unsandboxed process may dial. The live drill's `loopback_service`
probe has since measured it on a plain host (2026-10-28): the parent-started loopback service was
**reachable** (`FINDING`), while non-allowlisted external hosts stayed refused — so the wildcard is
an egress path, not a proxy bypass on its own.

## Child environment

`[fact][high]` `nono` does **not** pass the parent environment through unfiltered: it calls
`cmd.env_clear()` then re-adds surviving variables (the `nono` CLI crate's `src/exec_strategy.rs` L405-470). Dropped include: `LD_*`/`DYLD_*`, `BASH_ENV`/`ENV`/`CDPATH`/`GLOBIGNORE`/`BASH_FUNC_*`/
`PROMPT_COMMAND`/`IFS`, `PYTHONPATH`/`PYTHONSTARTUP`, `NODE_OPTIONS`/`NODE_PATH`, `PERL5*`,
`RUBYOPT`/`RUBYLIB`/`GEM_*`, `JAVA_TOOL_OPTIONS`/`_JAVA_OPTIONS`, `DOTNET_STARTUP_HOOKS`, `GOFLAGS`,
`OP_*` tokens, and `NONO_CAP_FILE`/`NONO_SUPERVISOR_PATH`.

Passed through unchanged: `HOME`, `XDG_*`, `TMPDIR`, `USER`, `SSH_AUTH_SOCK`, `TERM`. The profile
can add `allow_vars` / `deny_vars` / `set_vars` (`PATH` and `NONO_*` are reserved).

`[fact][high]` The `nono` crate's `src/scrub.rs` is **output redaction only** ("output hygiene, not sandbox
policy") for diagnostics/audit — it is not child-env filtering.

## Launcher-environment knobs

`[fact][high]` These are read from the environment of **the process that starts `nono`**, not the
child, and are not in the child blocklist (only `NONO_CAP_FILE` / `NONO_SUPERVISOR_PATH` are):

| Variable | Effect |
|---|---|
| `NONO_ALLOW` | feeds `--allow` (comma-delimited) — silently widens grants |
| `NONO_LOG_FILE` | writes a log to an arbitrary path **as the host user** |
| `NONO_UPSTREAM_PROXY`, `NONO_UPSTREAM_BYPASS`, `NONO_PROXY_PASS` | redirect / bypass the filtering proxy |
| `NONO_RUN_TOKEN`, `NONO_DETACH_*`, `NONO_STARTUP_TIMEOUT`, `NONO_THEME` | run/session controls |

This is the basis for the audit's F10: the supervisor's environment is a pre-sandbox execution
surface, and `workcell::scrub_env` filters only the `NONO_` prefix, as a helper the caller must
invoke.

## CLI flag reference (as used by `workcell`)

`[fact][high]` `-s` is the global `--silent`, not a mode; the subcommand is `run`;
`--no-diagnostics` only suppresses the non-zero-exit footer; `--workdir DIR` is the base for
`$WORKDIR` expansion and does not itself grant access (whether it also `chdir`s the child is
`[fact][med]`); `--allow-cwd` grants cwd at the level the profile sets (defaults to read-only, which
is why a read-only review needs it); `--allow/-a` rw dir recursive (env `NONO_ALLOW`), `--read/-r`
ro dir, `--write/-w`, `--allow-file` rw file, `--read-file` ro file, `--open-port` as above,
`--profile/-p` name **or path**.

`[fact][high]` `run`/`wrap` use `#[command(trailing_var_arg = true)]` with a required positional
`command: Vec<OsString>`, so **nothing after the program name is parsed as an option**. Argument
injection is only possible if agent-controlled tokens are placed *before* the program name. Verified:
`nono run --dry-run --allow /tmp /bin/echo --read /etc` prints the child as
`/bin/echo --read /etc` and grants only `/tmp`.

`[fact][high]` Robustness detail worth knowing: a grant that overlaps `nono`'s own state root fails
closed — `nono why --allow ~` errors with "Refusing to grant '/Users/…' … overlaps protected nono
state root '/Users/…/.local/state/nono'".

## Advisories, and which ones do not affect this deployment

`[fact][high]` The repository carries 12 advisories. Relevance to `workcell` on macOS 26.5.1 with
`nono` 0.80.0:

| Advisory | Class | Affects macOS workcell? | Why |
|---|---|---|---|
| **GHSA-wjv5-93q3-xm73** (HIGH, 2026-09-16, "patched: not yet released") | tool-sandbox **shim** falls through to the full `nono` CLI when broker env is absent; a mediated command's argv is then parsed as `nono` subcommands (`ps`, `stop`, `rollback`, `session cleanup`) | **Possibly — needs confirmation** | `main()` in v0.80.0 still branches `if maybe_run_internal_tool_sandbox_entrypoint() { return; }` else full `Cli::parse()`; the fallthrough shape is unchanged. `workcell` invokes the `nono` CLI directly rather than through a mediated-tool shim, so the stated trigger (broker env absent + mediated argv) is not the workcell call shape — but this is not proven. |
| GHSA-q7m6-rr8w-vjff (med, 2026-09-30, ≤0.78.0) | macOS recursive `filesystem.deny` blocked files beneath a denied dir but **not Unix sockets** beneath it (`connect(2)` is network-outbound) | **No** | Fixed in 0.79.0; 0.80.0 is clean. |
| GHSA-27vp-2mmc-vmh3 / CVE-2026-47128 (med, 2026-05-17, <0.55.0) | Linux sandbox escape via D-Bus: `systemd-run --user` spawns outside the sandbox | **No** | Linux-only, and fixed before 0.80.0. `workcell` refuses non-macOS hosts. |
| GHSA-vhq2-h2q7-8mmc (high) | seccomp legacy-x86_64-ABI bypass | **No** | Linux seccomp path; fixed 0.74.0. |
| GHSA-6hww, GHSA-7q3j, GHSA-gcpc | proxy host bypass on the same port (Landlock); hostname-normalisation bypass of deny lists; DNS tunnel through the proxy | **No** | Linux/Landlock and egress-proxy classes, all fixed 0.74.0; the macOS Seatbelt path is not the affected code. |
| GHSA-8r33 (high) | endpoint path-authorisation bug | **Not on 0.80.0** | Fixed 0.73.0; note the advisory's ≥0.73 range is still marked "not yet" patched, so treat as version-scoped, not proof for every later release. |

`[fact][high]` Structural limits from the docs/NEP rather than advisories, relevant if a
Landlock-shaped policy ever replaces Seatbelt: Landlock cannot express deny-inside-grant; its network
rules are TCP-port only (no destination IP, no UDP/ICMP); pathname `AF_UNIX` mediation
(`linux.af_unix_mediation`) is **off by default** and excluded from `wrap` and WSL2, and its
seccomp-user-notify resume has a documented pointer-argument race; abstract-socket scoping needs
ABI v6; macOS cannot filter bind/inbound by port, so any `--listen-port`/`open_port` makes
`network-bind`/`network-inbound` a blanket allow.

`[fact][med]` `nono`'s docs do **not** document the nested-sandbox refusal; they only say
restrictions stack if run inside an App Sandbox. The `EPERM` observed on this host is a platform
fact (see [`macos-seatbelt.md`](macos-seatbelt.md) §1), not a `nono` error contract.

## Could not verify

- Whether `block: true` plus `allow_domain` in a **profile** is rejected like the CLI flags are
  (CLI errors; the profile path warns and proxies).
- Whether `bypass_protection` lifts `filesystem.deny` or only group denies.
- Whether `--workdir` also sets the child's working directory.
- Whether `GHSA-wjv5-93q3-xm73` is actually fixed in 0.80.0 (the advisory says "not yet released",
  and the 0.80.0 code shape is unchanged).
- Any nested-`nono` refusal contract.
- The `alwaysfurther.ai` blog, Hacker News, and Reddit claims: not relied on.

## Sources

Accessed 2026-10-28. Version: `nono` 0.80.0, tag `v0.80.0` (2026-10-13), macOS 26.5.1 arm64.

- Repository and metadata — <https://github.com/nolabs-ai/nono> (redirect from
  <https://github.com/always-further/nono>), `MAINTAINERS.md`, release `v0.80.0`
- Security advisories — <https://github.com/nolabs-ai/nono/security/advisories>
- Execution modes — <https://nono.sh/docs/cli/features/execution-modes>
- Seatbelt internals — <https://nono.sh/docs/cli/internals/seatbelt>
- Security model — <https://nono.sh/docs/cli/internals/security-model>
- Landlock internals — <https://nono.sh/docs/cli/internals/landlock>
- Profile schema — `nono-profile.schema.json` at `v0.80.0`; groups/composition —
  <https://nono.sh/docs/cli/features/profiles-groups>
- Source at `v0.80.0`, read in the two workspace crates: the `nono` crate's `src/sandbox/macos.rs` L24-26,
  L866-895, L942-966, `src/sandbox/linux.rs`, and `src/scrub.rs`; the `nono` CLI crate's
  `src/proxy_runtime.rs` L3094, `src/policy.rs` L1004-1047, and `src/exec_strategy.rs` L405-470
- Local static evaluation (no sandboxed child): `nono --version`, `nono run --dry-run -p …`,
  `nono why --path|--host …`, `nono run --allow '--profile=…'` (parse rejection).

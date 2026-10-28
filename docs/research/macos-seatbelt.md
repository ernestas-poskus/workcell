# macOS Seatbelt: the platform underneath the sandbox

Scope: the macOS sandbox primitives `workcell` depends on, at host macOS 26.5.1 (arm64) and
`nono` 0.80.0. This document is platform evidence, not a description of `workcell`'s policy; the
policy itself is audited in [`workcell-audit.md`](workcell-audit.md) and exercised by
[`../../scripts/escape-drill.sh`](../../scripts/escape-drill.sh) (probe rationale:
[`../escape-testing.md`](../escape-testing.md)).

Confidence labels: `[fact][high]` = primary source or directly observed; `[fact][med]` = one
source or a version-dependent claim; `[inference]` = derived, not observed. **No sandboxed child
was executed for this document**: nested Seatbelt refuses to initialise inside the session (§1).
The escape drill has since run from a plain host shell (2026-10-28), and its results are in
[`../escape-testing.md`](../escape-testing.md); statements here that the drill did not cover remain
hypotheses with a named test, not results.

## 1. Nested apply is refused, and that is a reliability property, not an escape

`[fact][high]` A Seatbelt sandbox cannot be applied to an already-sandboxed process, and cannot be
removed or extended once applied. Mark Rowe (Chromium macOS sandbox author), 2024-11-27:

> It is not possible to sandbox an existing process from the outside... Once a process has a sandbox
> applied to it, it is not possible for it to disable or remove the sandbox, nor is it possible for
> it to apply additional sandbox policies.

`[fact][high]` The kernel records the refusal as
`Sandbox: sandbox-exec(...) deny(1) forbidden-sandbox-reinit`; the `fence` project states plainly
that nested Seatbelt sandboxes are not allowed and there is no workaround.

`[observed][high]` On this host, inside the DSH session sandbox:

- `sandbox-exec -p '(version 1)(allow default)' /usr/bin/true` → exit 71,
  `sandbox-exec: sandbox_apply: Operation not permitted`.
- `log show` → `log: Cannot run while sandboxed`.
- `nono setup --check-only` → `sandbox initialization failed: Operation not permitted`, which
  `workcell::preflight` turns into a launch refusal (`src/sandbox.rs`).

Consequence for this work: **the live drill cannot run from inside a sandboxed session** and must be
started from a plain host shell. Consequence for deployers: any tool that re-applies a sandbox
(`swift test`, Nix Darwin, a nested agent runtime) dies with `EPERM` inside the profile. That is a
denial-of-service / compatibility behaviour, **not** a way out. No SBPL operation can grant the
re-init.

## 2. SBPL: what the profile language actually promises

`[fact][high]` There is no official Apple specification for SBPL. Chromium's *Mac Sandbox V2*
design document (updated 2025-01-30) says so explicitly and is the best public description.

| Construct | Meaning | Consequence for a policy author |
|---|---|---|
| `path` / `literal` | exact path | too narrow for trees |
| `subpath` | the path and all descendants | the usual grant form |
| `regex` | matches the path string | how glob-style deny lists are compiled |
| `vnode-*` filters | evaluate **resolved vnodes** | a symlink placed at an allowed path does **not** grant its target |

`[fact][high]` Filters are evaluated against resolved vnodes, so `subpath "/tmp"` and
`subpath "/private/tmp"` are different rules on the same tree; Apple's own 26.5.1 profiles use
`/private/tmp` 41 times and `/tmp` 14 times for this reason.

`[fact][high]` `(allow file-read-metadata)` is unfiltered in 397 of the 511 bundled profiles in
`/System/Library/Sandbox/Profiles` on 26.5.1 (`bsd.sb` line 17). Metadata is deliberately allowed
where content is denied: **existence, type, and size remain observable** even under a deny. This is
a discovery oracle, not a confidentiality boundary — relevant to any policy that denies a file by
path (see the audit's F14).

`[fact][high]` A profile built as `(allow default)` plus narrow `(deny file-write*)` leaves
everything else allowed, including file mounting and LaunchServices/launchd IPC. The
gemini-cli PR #28423 documents this shape as the precondition for the CVE-2023-32364 chain;
`nono` avoids it with a `(deny default)` base.

## 3. Process and IPC primitives that are not filesystem rules

| Primitive | Mechanism | Gate | Evidence |
|---|---|---|---|
| LaunchServices → launchd | ask a system service to launch a bundle; the new process is a child of launchd, not of the caller | `mach-lookup com.apple.coreservices.launchservicesd` (a per-profile allow-list entry in Apple's profiles) | Kalman, CVE-2023-32364 (2023-09-26): "If an application is started by launchd and it doesn't have the `com.apple.security.app-sandbox` value set, it will run unsandboxed!" `[fact][high]` |
| `open` a `.command`/bundle | cheap variant of the above | same LaunchServices gate | `[inference]` — untested here |
| AppleEvents | drive another app (`osascript`) | `appleevent-send` + per-destination predicate (`application.sb:878`) **and** a separate TCC Automation consent attributed to the *responsible* process | `[fact][high]` |
| `launchctl` / LaunchAgents | register a job that launchd runs later | needs write to `~/Library/LaunchAgents` **and** the launchd Mach domain; a launchd job is not a child, so `sandbox(7)`'s "new processes inherit the sandbox of the parent" does not apply | `[fact][med]` rule, `[inference]` exploit |
| Pasteboard / distributed notifications | Mach + shared memory (`ipc-posix-shm` name `apple.shm.notification_center`, `bsd.sb:28`) | allowed IPC | data-leak channel, not an escape `[fact][med]` |
| `task_for_pid` / `task_name_for_pid` | acquire another task port | `com.apple.security.cs.debugger` entitlement | not reachable from a sandboxed agent `[fact][high]` |
| `NSFileCoordinator` / `filecoordinationd` | confused-deputy write on the caller's behalf | — | `[inference]`; no verifiable 2024–2026 exploit found |

`[fact][high]` Project Zero's framing still holds (2014-07-30): an unsandboxed service reachable
from inside the sandbox is the escape surface. Seatbelt confines *the process*, not the services it
can ask for work.

## 4. Loader and environment surface

`[fact][high]` `DYLD_*` only affects processes you `exec`, so it cannot lift the sandbox **you** are
already in — an injection lands inside the sandbox. It is blocked where it would matter: with SIP
enabled, `DYLD_*` is ignored for SIP-protected binaries (`man 1 dyld`, 26.5.1), and hardened
binaries ignore it unless signed with `com.apple.security.cs.allow-dyld-environment-variables`
(Apple Hardened Runtime docs). Non-hardened or ad-hoc children you spawn **are** injectable.

`[fact][high]` `__interpose` is link-time only; `os_log` is exfiltration-adjacent, not an escape.

The consequence for `workcell` is in the audit (F10): the dangerous position for `DYLD_*` is the
**unsandboxed supervisor** (`nono` itself), before Seatbelt initialises — not the sandboxed child.

## 5. CVE table, with corrections

| CVE | Claim | Status on this host | Note |
|---|---|---|---|
| **CVE-2019-3069** | a Seatbelt bypass | **does not exist** | `[fact][high]` MITRE returns `CVE_RECORD_DNE` / 404. Do not cite it. |
| CVE-2025-30462 | "A library injection issue... Apps that appear to use App Sandbox may be able to launch without restrictions." Published 2025-03-31; fixed in 15.4 / 14.7.5 / 13.7.5 | fixed before 26.x | the "looks sandboxed, runs unsandboxed" class `[fact][high]` |
| CVE-2025-43358 | "a shortcut may bypass sandbox restrictions", published 2025-09-15 | fixed **in** macOS 26 → not applicable to 26.5.1 | version-scoped `[fact][high]` |
| CVE-2023-32364 | mount devfs → symlink a bundle → `open` → unsandboxed launchd child | chain fixed | the canonical LaunchServices escape; still the model to test `[fact][high]` |

## 6. Deprecation status

`[fact][high]` `/usr/bin/sandbox-exec` is still present on 26.5.1 and its man page still reads
"execute within a sandbox (DEPRECATED)". `sandbox_init(3)` and the named profiles are deprecated
too. Rowe's assessment: Apple considers this layer deprecated in favour of App Sandbox, but "in
practice, the App Sandbox is not a usable replacement for many large applications". There is no CLI
replacement, and Apple still ships 511 profiles. Deprecated is not removed, and nothing in the
26.x release notes changes enforcement.

## 7. What this means for the workcell model

`[inference]` Three platform facts shape the threat model:

1. A Seatbelt failure is **loud and fail-closed**: a profile that cannot apply exits 71 rather than
   running unconfined. `workcell` relies on this (`[nono:` refusal shape).
2. The interesting escapes are **not** SBPL bugs but services: LaunchServices, AppleEvents, launchd,
   and any unsandboxed local listener. A policy that blocks files and leaves Mach IPC broad is
   porous in a way a file-grant review will not show.
3. Metadata is deliberately observable. A deny that name-matches a path protects **content**, not
   existence.

## Could not verify

- **Mickey Jin, "A New Era of macOS Sandbox Escapes" (POC 2024)** — likely the best 2024-era bypass
  survey. The PDF could not be fetched (`web_fetch` refuses `application/pdf`). **Unread.** Every
  claim that would have come from it is absent from this document.
- Eclectic Light Co: searched for a Seatbelt-internals article and found only a tangential Big Sur
  11.5.2 post. No Howard Oakley article backs §§2–3, so those rest on Chromium/Apple/Kalman.
- CVE-2025-31191 appeared only in a secondary "hazmat" document and was not checked against Apple;
  it is omitted rather than asserted.
- Except for the nested-apply `EPERM` (observed directly), "still works on 26.x" judgements are
  inferred from fix-version data, never from a run.

## Sources

Accessed 2026-10-28 unless noted. Version: macOS 26.5.1, arm64; `nono` 0.80.0.

- Mark Rowe, "Sandboxing on macOS", 2024-11-27 — <https://bdash.net.nz/posts/sandboxing-on-macos/>
- Chromium, *Mac Sandbox V2* design (SBPL has no official spec), updated 2025-01-30 —
  <https://chromium.googlesource.com/chromium/src/+/refs/heads/main/sandbox/mac/seatbelt_sandbox_design.md>
- `fence` troubleshooting, nested sandboxes — <https://github.com/fencesandbox/fence/blob/main/docs/troubleshooting.md>
- Gergely Kalman, CVE-2023-32364 (launchd/LaunchServices escape), 2023-09-26 —
  <https://gergelykalman.com/CVE-2023-32364-a-macOS-sandbox-escape-by-mounting.html>
- gemini-cli PR #28423 (`(allow default)` + narrow deny shape) — <https://github.com/google-gemini/gemini-cli/pull/28423>
- Apple security releases: CVE-2025-30462, CVE-2025-43358 —
  <https://support.apple.com/en-us/100100>
- Apple, Hardened Runtime / `com.apple.security.cs.allow-dyld-environment-variables`.
- Local: `/System/Library/Sandbox/Profiles/bsd.sb` (line 17), `application.sb` (line 878),
  `man 1 dyld`, `man 1 sandbox-exec`, `sandbox(7)` on macOS 26.5.1.
- Project Zero, "The Sandbox Myth" framing, 2014-07-30.

# `workcell` research

The platform, delegate, and audit documents are crate-local. The harness notes were copied from
the project's harness research (2026-10-22) and de-branded for this crate; that copy is
maintained here. Line-number citations (`file:line`) in the copied notes were accurate in the
archive at the date shown; the provenance header and de-branding edits shift them, so treat
them as historical pointers rather than exact offsets.

The evidence behind the sandbox's policy: what the platform refuses, what the delegate enforces,
what this crate's own code does, and what the coding-agent CLIs read and write on a host. The
purpose is that each commitment in [`security-model.md`](../security-model.md) can be traced to a
source, a probe, or a test rather than to a comment.

| Document | What it establishes |
|---|---|
| [`macos-seatbelt.md`](macos-seatbelt.md) | The platform: nested-apply refusal, SBPL weaknesses, LaunchServices/launchd, AppleEvents, DYLD, CVEs (with a correction), and deprecation. |
| [`nono-0.80.0.md`](nono-0.80.0.md) | The delegate: architecture, profile semantics and precedence, the `network.block` correction, `open_port`, environment filtering, and advisories. |
| [`workcell-audit.md`](workcell-audit.md) | The crate's own code: findings with `file:line`, the grant table, and the findings that were refuted on re-read. |
| [`harness/`](harness/README.md) | The harness surface the embedded profiles encode: home/config versus state paths, non-interactive permission flags, MCP/plugin discovery, exit-code traps, background/idle behaviour, and version drift for claude 2.1.303, codex 0.163.0, and agy 1.4.5. |

## Reading the evidence

The documents label what kind of evidence a statement rests on, and the label is part of the
claim. `[fact][high]` means a primary source or a direct observation; `[fact][med]` means one
source or a version-dependent observation; `[inference]` means it follows from evidence but was
not observed; `[hypothesis]` means it is plausible and unverified. The audit uses a separate
legend (`CONFIRMED`, `PLAUSIBLE`, `REFUTED`) for findings about the crate's own code. A measured
probe, a cited source, and a test assertion are not interchangeable, and each document says which
one its verdicts use.

## Verdicts are version-scoped

Every verdict in this directory is scoped to **`nono` 0.80.0 on macOS 26.5.1, Apple Silicon**. A
different `nono`, a different macOS, or a Linux host (which this release refuses) can change any
verdict. The same applies inside [`harness/`](harness/README.md): each note names the installed
harness version it describes, and the public documentation for all three harnesses was already
drifting from the installed binaries on the date of access.

## Provenance and sync

The platform, delegate, and audit documents are crate-local and reviewed against the profiles. The
harness notes were written as harness research on 2026-10-22 and copied here de-branded; they are
maintained here now. One rule keeps the set honest: a change to a profile grant or a launch flag is
a change to the evidence, so update [`../security-model.md`](../security-model.md) and
[`../sandbox-hypotheses.md`](../sandbox-hypotheses.md) in the same change, and re-read the relevant
harness note when an upgrade changes a surface it pins.

## Honest limits

- Seatbelt shares the host kernel. This is a capability boundary, not a virtual machine.
- `nono` is pre-1.0; the contract is written against a wrapper's flags and a profile schema, not a
  stability guarantee.
- The harness research is documentation- and source-level: no harness was dispatched for it. The
  crate's live evidence is separate and dated in [`../escape-testing.md`](../escape-testing.md) and
  [`../live-drill.md`](../live-drill.md).
- A grant recorded here as deliberate (harness state, keychain files, loopback) is a residual
  someone has to own, not a defect to discover. The "Exposed and not contained" section of
  [`../security-model.md`](../security-model.md) is the list.

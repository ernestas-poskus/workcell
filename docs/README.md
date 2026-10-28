# workcell docs

Crate-local documentation. `workcell` is publishable and usable on its own, so the evidence for
what the sandbox does — and what it does not — lives here rather than in a parent repository.

| Document | What it establishes |
|---|---|
| [`security-model.md`](security-model.md) | The crate-level sandbox policy: what is contained, what is deliberately exposed, and why each unusual grant exists. |
| [`escape-testing.md`](escape-testing.md) | What the escape drill probes, the safety rules, how to run it, the static assertions, and the live results. |
| [`live-drill.md`](live-drill.md) | Dated live results (real harnesses, egress, policy variants) and their limits. |
| [`performance.md`](performance.md) | Wrapper cost on one host, where it comes from, and how to re-measure it. |
| [`sandbox-hypotheses.md`](sandbox-hypotheses.md) | One row per hypothesis with its verdict: what was refuted, what was confirmed, and what still needs a live probe. |
| [`research/macos-seatbelt.md`](research/macos-seatbelt.md) | The platform: nested-apply refusal, SBPL weaknesses, LaunchServices/launchd, AppleEvents, DYLD, CVEs (with a correction), deprecation. |
| [`research/nono-0.80.0.md`](research/nono-0.80.0.md) | The delegate: architecture, profile semantics and precedence, the `network.block` correction, `open_port`, environment filtering, advisories. |
| [`research/workcell-audit.md`](research/workcell-audit.md) | The crate's own code: findings with `file:line`, plus the refutations. |

## Reproducing the evidence

```sh
make sandbox-why     # the static policy assertions; needs no sandbox, runs anywhere
make escape-drill    # the live probes; needs a plain macOS host, refuses inside another sandbox
```

`make sandbox-why` is the cheap regression check: it asks `nono` what it would decide about the
rendered profiles and holds it to their intended shape, so a profile edit or a `nono` upgrade is
caught without a sandbox-capable host. `make escape-drill` is evidence, not a gate, and is
deliberately outside `make verify`.

## Scope

Every observation is scoped to **`nono` 0.80.0 on macOS 26.5.1, Apple Silicon**. A different `nono`,
a different macOS, or a Linux host (which this release refuses) can change any verdict here. The
drill and the assertions are version-scoped on purpose; neither is a claim about a future release.

The live drill ran on one host that could initialize a Seatbelt profile on 2026-10-28 (Darwin
25.5.0, Apple Silicon, `nono` 0.80.0): 26 `PASS`/`DENIED`, 6 `FINDING`, 1 `SKIP`, no `FAIL` or
`UNKNOWN` in the safe run, and one additional clipboard `FINDING` under `--aggressive`. The
per-probe results are in [`escape-testing.md`](escape-testing.md). Static assertions remain the
cheap regression check; a live result is one-host evidence, not a guarantee.

## Honest limits

- Seatbelt shares the host kernel. This is a capability boundary, not a virtual machine.
- `nono` is pre-1.0; the contract is written against a wrapper's flags and a profile schema, not a
  stability guarantee.
- A grant recorded here as deliberate (harness state, keychain files, loopback)
  is a residual someone has to own, not a defect to discover. The "Exposed and not contained"
  section of [`security-model.md`](security-model.md) is the list.

# workcell agent instructions

These instructions apply to any agent working on `workcell/`. This file is canonical for the crate.
Paths are relative to this directory, and the commands run from it.

## Read first

1. [`README.md`](README.md) — what the crate is, what the sandbox contains, and its honest limits.
2. [`docs/`](docs/README.md) — the escape-testing evidence, the hypothesis verdicts, and the research
   behind them.
3. [`src/lib.rs`](src/lib.rs) — the module map and the guarantee each module carries.
4. The module you are changing and its tests. The tests are the contract in executable form.

## Direction and boundaries

`workcell` is a **domain-agnostic substrate**. It knows about Git, worktrees, processes, files,
instruction files, and an OS sandbox. It knows nothing about a task tracker, a model, a routing
policy, a plan, a brief, a review workflow, or an agent's name.

- **No domain types.** Do not add a type, field, error variant, log line, or fixture that names a
  tracker, a plan, a work request, a caller's workflow, or a consumer. A caller maps its concepts onto
  this crate's neutral types (a label becomes a branch prefix, a run becomes a worktree, a prompt
  becomes bytes).
- **No reverse dependency.** This crate never depends on a consumer. Do not add a path dependency
  to one, and do not import a consumer's types in tests.
- **One nested workspace.** This directory is its own Cargo workspace (`[workspace]` in
  `Cargo.toml`). It carries its own lockfile, README, LICENSE, CHANGELOG, and docs so it can stand
  alone. Do not add it to a parent workspace.
- **Publishable, not published.** Keep it packageable (`cargo package --list` is a gate). The public
  API is the compatibility surface once released: a change to a public type, function signature,
  error variant, or documented guarantee is a semver decision.

## The sandbox is unconditional

The sandbox is the reason this crate exists. It is not a feature with a switch.

- **No disable path.** Do not add a configuration field, environment variable, CLI flag, or
  constructor that runs an agent unconfined. A missing binary, an unusable host, an invalid profile,
  or an unusable state root is a refusal, never a fallback.
- **Never weaken a profile** in `src/sandbox/profiles/`. A grant is a deliberate residual: if you
  widen one, say why in the profile's `meta.description`, record the residual in
  [`docs/sandbox-hypotheses.md`](docs/sandbox-hypotheses.md), and keep the tests in
  `tests/sandbox.rs` asserting it.
- **A deny is not containment unless it beats the grant around it.** Assert the *reason* a decision
  was denied, not only the verdict ("not granted" and "a deny rule covers it" are different
  guarantees).
- **Preflight is the last gate.** Any new launch path must go through `resolve` → `preflight` →
  `wrap_*`; do not spawn the sandbox binary anywhere else.

## Tests stay offline

The unit and integration tests must run without a real sandbox. A fake `nono` script stands in for
the binary; it records argv and environment, answers the preflight subcommands, and executes the
command after `--`. Anything that needs a real Seatbelt profile belongs in
[`scripts/escape-drill.sh`](scripts/escape-drill.sh) as evidence, not in the suite.

`scripts/sandbox-why.sh` sits in between: it asks a real `nono` what it *would* decide, needs no
sandbox, and is the cheap regression check for a profile or `nono` change.

## Gates

Run all of these from this directory before declaring a change complete:

```sh
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo nextest run --status-level fail      # tests must stay offline
cargo test --doc                           # nextest does not run doc-tests
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo package --list --allow-dirty >/dev/null
```

`make verify` runs the same set; `.github/workflows/ci.yml` runs it in CI. `make sandbox-why` and
`make escape-drill` are evidence, not gates.

## The profiles stay caller-neutral

The embedded profiles deny the caller's in-checkout run-control paths under
`SandboxOptions::state_dir` (default `.workcell`). Never hard-code a consumer's directory name in a
profile, a test, a script, or a doc: render the `__STATE_DIR__` token through `Sandbox` and pass the
name from the caller. The token appears only in the six denial entries, never in a grant, so the
artifact paths a caller grants per run stay readable.

## House style

- Domain-agnostic, evidence-first prose. A doc comment states the contract and why; it does not
  narrate the implementation.
- Prefer a real bound (a cap, a deadline, a refusal) over a comment promising one.
- An error that cannot observe a surface is a refusal, not a pass.
- Cite the test that proves a guarantee. If a guarantee has no test, it is a wish.

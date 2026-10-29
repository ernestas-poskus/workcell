# workcell agent instructions

These instructions apply to any agent working on `workcell/`. This file is canonical for the crate.
Paths are relative to this directory, and the commands run from it.

## Read first

1. [`README.md`](README.md) — the crate's modules and limits.
2. [`src/lib.rs`](src/lib.rs) — the module map and the guarantee each module carries.
3. The module you are changing and its tests. The tests are the contract in executable form.

## Direction and boundaries

`workcell` is a **domain-agnostic substrate**. It knows about Git, worktrees, processes, files,
instruction files, launch templates, and verification records. It knows nothing about a task
tracker, a model, a routing policy, a plan, a brief, a review workflow, or an agent's name.

- **No domain types.** Do not add a type, field, error variant, log line, or fixture that names a
  tracker, a plan, a work request, a caller's workflow, or a consumer.
- **No reverse dependency.** This crate never depends on a consumer.
- **One nested workspace.** Keep it out of the parent workspace.
- **Publishable, not published.** Keep it packageable and its public API compatible.
- **Async-first.** Filesystem work on the runtime uses Tokio's blocking pool; child processes use
  Tokio's process driver. Do not block an async path on `std::fs`/`std::process`.

## Gates

`cargo fmt --check`, `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`,
`cargo nextest run`, `cargo doc --no-deps`, and `cargo package --list` must pass.

## Commits

`type(component): intent` messages, as in the root `AGENTS.md`.

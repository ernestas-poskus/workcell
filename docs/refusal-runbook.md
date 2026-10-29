# When the sandbox refuses

A refusal is a control working, not an outage. This runbook says what each refusal means, what to
check, and what to do — with the one rule that matters: **there is no disable path.** Do not add a
flag, environment variable, or profile edit that runs the agent unconfined. Fix the cause or stop
dispatching.

## Preflight refused (`Error::Sandbox` / `Error::Config`, before any launch)

| Message names | Check | Fix |
| --- | --- | --- |
| `nono … not found or not executable` | `command -v nono`, `nono --version` | install `nono` (>= the crate's minimum) |
| `--version … older than` | `nono --version` | upgrade `nono`; the embedded profiles are reviewed against a floor |
| `setup --check-only exited …` | `nono setup --check-only` on a plain host | a nested sandbox or unsupported host cannot enforce the policy — dispatch from a plain macOS host |
| `profile validate …` | `nono profile validate --strict <published profile>` | the embedded profile is compiled in, so this means the installed `nono` is too old for the schema |
| `published … profile no longer matches` | the profile directory | it was edited; delete the published profiles and re-run preflight (the bytes are content-addressed) |
| `state root … unusable` | `nono` state location, `HOME`/`XDG_STATE_HOME` | make the state root writable, or fix the environment the supervisor inherits |

## The shared-checkout guard tripped (exit 3, after the child exited)

The repository-local `.git` surface changed while the agent ran: a hook, config, info file, or mode
bit. Detection is the contract; the guard does not repair.

1. Do not integrate the branch, and do not reuse the checkout for another run.
2. Diff the guard's reported surface against the expected one; keep the evidence.
3. Discard the shared checkout (or reset to the recorded base) before the next dispatch.
4. Prefer worktree mode: the agent writes its own worktree, so the integration checkout's `.git` is
   outside its grants entirely.

## A run was refused by the sandbox (`sandbox refused`)

The child did not start: the wrapper was built but `nono` refused to apply it. This is a
non-retryable `sandbox` failure.

- Confirm the host still passes `nono setup --check-only` and the published profiles still match.
- If an operator edited a profile, delete the published copy and re-run preflight.
- Do not treat it as an agent failure and retry the same launch.

## Verification or `auth_check` reports unavailable

Both run sandboxed: verification in the verification profile (network denied, no credentials,
writable checkout), `auth_check` in the read-only review profile with its harness's grants.

- A check that needs a network host: add that host to `verify.profile.json` deliberately (a
  reviewed profile change), never by disabling the sandbox.
- A check that writes its cwd or a state path: give it a writable path in the reviewed profile, or
  change the check.
- An auth check that prints a permission error: confirm the harness's caller-supplied grants
  include its credential store.

## Doctor lines that are warnings, not failures

- `state directory … inside the shared checkout` — enable worktree mode (the default). Note that
  `workspace.state_root` relocates the archive/control file, **not** the state directory.
- `nono … newer than the tested version` — run `make escape-drill` and `make sandbox-why` after the
  upgrade before trusting the matrix.
- `package caches: read-only` — pre-fetch a dependency that is not already cached; it cannot be
  fetched inside the sandbox.

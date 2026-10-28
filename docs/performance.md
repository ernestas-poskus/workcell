# Wrapper performance (2026-10-27)

The sandbox wrapper is not free: `preflight` publishes and validates the profiles, and `nono`
expands the profile's recursive checkout denies at sandbox setup. This page records the cost
measured on one host so a change can be compared against it.

Host: macOS 26.5.1, Apple Silicon, `nono` 0.80.0. Three iterations per measurement; medians with
`[min–max]` in milliseconds:

| Measurement | Median | Min | Max |
| --- | ---: | ---: | ---: |
| `nono --version` | 25.7 | 25.4 | 27.2 |
| `nono setup --check-only` | 222.2 | 185.5 | 254.3 |
| `nono run -- true`, default profile | 383.4 | 353.0 | 386.5 |
| `nono run -- true`, agent profile | 3260.8 | 2248.8 | 10085.2 |
| agent profile without the `$WORKDIR/**` denies | 344.7 | 174.6 | 430.0 |
| example preflight + `true` | 1789.2 | 1604.8 | 1830.5 |

## What the numbers mean

The agent profile costs ~3.3 s median per wrapper launch versus 383 ms for `nono`'s default
profile. The difference is dominated by the recursive `$WORKDIR/**` denies: removing those entries
from a temporary variant drops the same launch to 344.7 ms, a median delta of ~2.9 s.

**Variance is large.** The three agent-profile samples were 2.25 s, 3.26 s, and 10.09 s (the first,
cold); an earlier ad-hoc 5-iteration run measured a 1.26 s delta that was not saved. Treat the delta
as a range, not a constant: the recursive globs are expanded over the checkout at sandbox setup, so
the cost scales with checkout size.

## Reproducing

There is no committed benchmark script: the measurements above were taken with direct `nono` and
example invocations on the stated host. To compare after a profile or `nono` change, time the same
commands on the host and checkout you care about (`nono --version`, `nono setup --check-only`,
`nono run --profile <profile> -- true`, and the example's preflight plus `true`), and compare
medians rather than single runs.

## Recommendation

The recursive checkout denies are correctness-critical: they cover nested instruction and
configuration files, so keep them. If wrapper latency matters, reduce the number of recursive
globs, deny only the configured instruction files' roots, or use a `nono` literal deny-list flag;
then re-measure after the change.

## Kill and reaping

A normal exit with a TERM-respecting background child completes in ~0.3 s and the leftover is
reaped; the `KILL_GRACE` path stays covered by the process test in `tests/process.rs`.

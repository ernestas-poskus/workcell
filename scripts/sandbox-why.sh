#!/usr/bin/env bash
#
# The sandbox policy assertions: ask `nono` what it would decide, and hold it to the
# answer the profiles are supposed to produce.
#
# This is evidence, not a gate, but unlike the escape drill it needs no sandbox: `nono why`
# and `nono run --dry-run` evaluate a profile without applying a Seatbelt profile, so this
# script runs inside another sandbox and on a host that cannot initialize one. That makes it
# the cheap regression check for a profile edit, a `nono` upgrade, or a platform change.
#
# Usage:
#   scripts/sandbox-why.sh [--bin PATH] [--out DIR] [--state-dir DIR] [--verbose]
#
# Environment:
#   SANDBOX_WHY_BIN   `nono` binary (default: `nono` on PATH)
#   SANDBOX_WHY_ROOT  report directory (default: $TMPDIR/workcell-sandbox-why-<ts>)
#
# Exit codes: 0 every assertion held, 1 an assertion failed, 2 a prerequisite is missing.
#
# One substitution point: the embedded profiles carry the token `__STATE_DIR__` in their six
# in-checkout state-path denials. `workcell` renders it with the caller's state directory before
# publishing; this script does the same substitution into a temporary copy and asks `nono` about
# the rendered bytes, so the assertions describe the policy a caller actually gets.
#
# What it pins, and why each one matters:
#   - the two credential denials (a deny is only useful if it beats the surrounding grant);
#   - the package caches that are read-only (a later unsandboxed build trusts the extracted tree);
#   - the harness state that is writable on purpose, and the config that is not;
#   - the checkout's instruction-file deny and the rendered state-path denies;
#   - egress: proxy mode, not "network allowed";
#   - loopback: granted only when a caller asks for it per run.
# A decision that changes here changes what the sandbox contains, so the script asserts the
# reason as well as the verdict: "DENIED because the path is not granted" and "DENIED because a
# deny rule covers it" are different guarantees, and only the second survives a broader grant.

set -u

BIN="${SANDBOX_WHY_BIN:-nono}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="${SANDBOX_WHY_ROOT:-${TMPDIR:-/tmp}/workcell-sandbox-why-$STAMP}"
STATE_DIR=".workcell"
VERBOSE="no"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILES="$ROOT_DIR/src/sandbox/profiles"
AGENT="$PROFILES/agent.profile.json"
REVIEW="$PROFILES/review.profile.json"
VERIFY="$PROFILES/verify.profile.json"
FAILURES=0
CHECKS=0

while [ $# -gt 0 ]; do
    case "$1" in
        --bin) [ $# -ge 2 ] || { echo "sandbox-why: --bin needs a value" >&2; exit 2; }; BIN="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || { echo "sandbox-why: --out needs a value" >&2; exit 2; }; OUT="$2"; shift 2 ;;
        --state-dir) [ $# -ge 2 ] || { echo "sandbox-why: --state-dir needs a value" >&2; exit 2; }; STATE_DIR="$2"; shift 2 ;;
        --verbose) VERBOSE="yes"; shift ;;
        -h|--help) sed -n '2,38p' "$0"; exit 0 ;;
        *) echo "sandbox-why: unknown argument: $1" >&2; exit 2 ;;
    esac
done

# The same shape `SandboxOptions::state_dir` accepts: a plain relative path with no token, quote,
# whitespace, or parent traversal.
case "$STATE_DIR" in
    ""|/*|*..*|*[!A-Za-z0-9._/-]*)
        echo "sandbox-why: invalid --state-dir: $STATE_DIR" >&2; exit 2 ;;
esac

command -v "$BIN" >/dev/null 2>&1 || { echo "sandbox-why: missing prerequisite: $BIN" >&2; exit 2; }
[ -f "$AGENT" ] || { echo "sandbox-why: missing agent profile: $AGENT" >&2; exit 2; }
[ -f "$REVIEW" ] || { echo "sandbox-why: missing review profile: $REVIEW" >&2; exit 2; }

mkdir -p "$OUT"
TSV="$OUT/results.tsv"
: >"$TSV"

# Render the embedded profiles exactly as `Sandbox::resolve_with` does before asking `nono` what
# they decide: the published policy has no token in it.
RENDERED="$OUT/profiles"
mkdir -p "$RENDERED"
sed "s|__STATE_DIR__|$STATE_DIR|g" "$AGENT" >"$RENDERED/agent.profile.json" \
    || { echo "sandbox-why: cannot render the agent profile" >&2; exit 2; }
sed "s|__STATE_DIR__|$STATE_DIR|g" "$REVIEW" >"$RENDERED/review.profile.json" \
    || { echo "sandbox-why: cannot render the review profile" >&2; exit 2; }
AGENT="$RENDERED/agent.profile.json"
REVIEW="$RENDERED/review.profile.json"

# A workdir the profiles can expand `$WORKDIR` against. It holds an instruction file and the
# state paths `nono why` will be asked about, because a deny is only meaningful if the path
# exists to be denied (a glob deny is not expanded against a missing directory).
WORKDIR="$ROOT_DIR/target/sandbox-why-workdir"
mkdir -p "$WORKDIR" "$WORKDIR/$STATE_DIR/runs"
printf '# assertion checkout\n' >"$WORKDIR/AGENTS.md"
printf 'assertion readme\n' >"$WORKDIR/README.md"
printf '{"claims":1}\n' >"$WORKDIR/$STATE_DIR/state.json"
printf 'assertion log\n' >"$WORKDIR/$STATE_DIR/runs/run-1.log"

say() { printf '%s\n' "$*"; }
record() { # id status expected observed
    printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >>"$TSV"
    printf '%-38s %-4s %s\n' "$1" "$2" "$4"
}
fail() { FAILURES=$((FAILURES + 1)); }
pass() { CHECKS=$((CHECKS + 1)); }

# A path decision must be DENIED for the named policy reason.
expect_deny() { # id profile path op reason-substring [extra args...]
    local id="$1" profile="$2" path="$3" op="$4" reason="$5" text
    shift 5
    text="$("$BIN" why -s -p "$profile" --workdir "$WORKDIR" --path "$path" --op "$op" "$@" 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    case "$text" in
        *DENIED*"$reason"*) record "$id" PASS "deny/$op" "$(printf '%s' "$text" | head -2 | tr '\n' ' ')"; pass ;;
        *) record "$id" FAIL "deny/$op ($reason)" "$(printf '%s' "$text" | head -3 | tr '\n' ' ')"; fail ;;
    esac
}

# A path decision must be ALLOWED with the named access.
expect_allow() { # id profile path op access-substring [extra args...]
    local id="$1" profile="$2" path="$3" op="$4" access="$5" text
    shift 5
    text="$("$BIN" why -s -p "$profile" --workdir "$WORKDIR" --path "$path" --op "$op" "$@" 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    case "$text" in
        *ALLOWED*"$access"*) record "$id" PASS "allow/$op" "$(printf '%s' "$text" | head -2 | tr '\n' ' ')"; pass ;;
        *) record "$id" FAIL "allow/$op ($access)" "$(printf '%s' "$text" | head -3 | tr '\n' ' ')"; fail ;;
    esac
}

# A host decision must be DENIED for the named policy reason.
expect_host_deny() { # id profile host port reason-substring
    local id="$1" profile="$2" host="$3" port="$4" reason="$5" text
    text="$("$BIN" why -s -p "$profile" --workdir "$WORKDIR" --host "$host" --port "$port" 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    case "$text" in
        *DENIED*"$reason"*) record "$id" PASS "deny/host" "$(printf '%s' "$text" | head -2 | tr '\n' ' ')"; pass ;;
        *) record "$id" FAIL "deny/host ($reason)" "$(printf '%s' "$text" | head -3 | tr '\n' ' ')"; fail ;;
    esac
}

# A host decision must be ALLOWED for the named reason.
expect_host_allow() { # id profile host port reason-substring
    local id="$1" profile="$2" host="$3" port="$4" reason="$5" text
    text="$("$BIN" why -s -p "$profile" --workdir "$WORKDIR" --host "$host" --port "$port" 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    case "$text" in
        *ALLOWED*"$reason"*) record "$id" PASS "allow/host" "$(printf '%s' "$text" | head -2 | tr '\n' ' ')"; pass ;;
        *) record "$id" FAIL "allow/host ($reason)" "$(printf '%s' "$text" | head -3 | tr '\n' ' ')"; fail ;;
    esac
}

# The resolved capability summary must contain the pattern. `--dry-run` prints the net and
# ipc lines the profiles actually resolve to; `why` alone answers only the proxy question.
expect_dry_run() { # id profile pattern [extra args...]
    local id="$1" profile="$2" pattern="$3" text
    shift 3
    text="$("$BIN" run --dry-run -p "$profile" --workdir "$WORKDIR" "$@" -- /bin/true 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    if printf '%s' "$text" | grep -qE "$pattern"; then
        record "$id" PASS "dry-run" "$(printf '%s' "$text" | grep -E "$pattern" | tr -s ' ' | tr '\n' ';')"
        pass
    else
        record "$id" FAIL "dry-run ($pattern)" "$(printf '%s' "$text" | tail -4 | tr '\n' ' ')"
        fail
    fi
}

say "sandbox-why: $(command -v "$BIN") $("$BIN" --version 2>/dev/null | head -1)"
say "sandbox-why: report $OUT"

# A resolved capability summary must NOT contain the pattern (a grant the profile no longer makes).
expect_dry_run_absent() { # id profile pattern
    local id="$1" profile="$2" pattern="$3" text
    text="$("$BIN" run --dry-run -p "$profile" --workdir "$WORKDIR" -- /bin/true 2>&1)"
    [ "$VERBOSE" = yes ] && say "--- $id: $text"
    if printf '%s' "$text" | grep -qE "$pattern"; then
        record "$id" FAIL "dry-run absent ($pattern)" "$(printf '%s' "$text" | grep -E "$pattern" | tr -s ' ' | tr '\n' ';')"
        fail
    else
        record "$id" PASS "dry-run absent" "the profile does not resolve the grant"
        pass
    fi
}

# ── credentials: the denies that must beat an enclosing read grant ────────────
expect_deny  agent_cargo_credentials  "$AGENT" "$HOME/.cargo/credentials.toml" read filesystem_deny
# A case variant resolves to the same vnode on a case-insensitive filesystem; a deny that
# only matched the literal spelling would be walkable on APFS.
expect_deny  agent_cargo_credentials_case "$AGENT" "$HOME/.cargo/CREDENTIALS.toml" read filesystem_deny
expect_deny  agent_gh_config          "$AGENT" "$HOME/.config/gh" read filesystem_deny
expect_deny  agent_launchagents       "$AGENT" "$HOME/Library/LaunchAgents/probe.plist" write path_not_granted
expect_deny  agent_gitconfig_write    "$AGENT" "$HOME/.gitconfig" write insufficient_access
expect_deny  agent_harness_config_write "$AGENT" "$HOME/.claude/settings.json" write path_not_granted

# ── the checkout's instruction files and rendered state paths ────────────────
expect_deny  agent_workdir_agents     "$AGENT" "$WORKDIR/AGENTS.md" read filesystem_deny
# The six state-path denials carry the caller's state directory, rendered above (default
# `.workcell`). A read and a write through the surrounding `$WORKDIR` grant must both hit the
# deny rule, not merely lack a grant.
expect_deny  agent_state_read         "$AGENT" "$WORKDIR/$STATE_DIR/state.json" read filesystem_deny
expect_deny  agent_state_logs_write   "$AGENT" "$WORKDIR/$STATE_DIR/logs" write filesystem_deny
expect_deny  agent_state_runs_write   "$AGENT" "$WORKDIR/$STATE_DIR/runs/run-1.log" write filesystem_deny

# ── grants that are deliberate, and each is a residual someone must own ──────
expect_allow agent_cargo_registry_read  "$AGENT" "$HOME/.cargo/registry/src/probe/crate-0.0.0/build.rs" read read
expect_deny  agent_cargo_registry_write "$AGENT" "$HOME/.cargo/registry/src/probe/crate-0.0.0/build.rs" write insufficient_access
expect_allow agent_cargo_git_read       "$AGENT" "$HOME/.cargo/git/checkouts/probe/abc/build.rs" read read
expect_deny  agent_cargo_git_write      "$AGENT" "$HOME/.cargo/git/checkouts/probe/abc/build.rs" write insufficient_access
expect_allow agent_shell_snapshots    "$AGENT" "$HOME/.claude/shell-snapshots/probe.sh" readwrite read+write
expect_allow agent_harness_state      "$AGENT" "$HOME/.claude/projects/probe.jsonl" readwrite read+write
expect_deny  agent_dsh_state          "$AGENT" "$HOME/.dsh/.credentials.yaml" read path_not_granted
expect_deny  agent_keychain_read      "$AGENT" "$HOME/Library/Keychains/login.keychain-db" read filesystem_deny
# The caller names its own harness path per run; the profile grants none of them.
expect_allow agent_caller_harness_read "$AGENT" "$HOME/.claude" read read --read "$HOME/.claude"
expect_allow agent_agy_token_write    "$AGENT" "$HOME/.gemini/antigravity-cli/antigravity-oauth-token" write read+write

# ── egress: the allowlist is a proxy, and the proxy is the only route ────────
expect_dry_run agent_net_proxy        "$AGENT" '^[[:space:]]*net[[:space:]]+proxy[[:space:]]*$'
expect_dry_run_absent agent_ipc_loopback     "$AGENT" '^[[:space:]]*ipc[[:space:]]+localhost:0[[:space:]]*$'
expect_dry_run agent_loopback_on_request "$AGENT" '^[[:space:]]*ipc[[:space:]]+localhost:0[[:space:]]*$' --open-port 0
expect_host_deny  agent_egress_offlist     "$AGENT" "example.com" 443 proxy_filtered
expect_host_deny  agent_egress_ip_literal  "$AGENT" "1.1.1.1" 443 proxy_filtered
expect_host_deny  agent_egress_loopback    "$AGENT" "127.0.0.1" 3080 proxy_filtered
expect_host_allow agent_egress_provider    "$AGENT" "api.anthropic.com" 443 proxy_allowed

# ── the review profile: read-only checkout, same egress shape ────────────────
expect_dry_run review_net_proxy       "$REVIEW" '^[[:space:]]*net[[:space:]]+proxy[[:space:]]*$'
expect_dry_run_absent review_ipc_loopback    "$REVIEW" '^[[:space:]]*ipc[[:space:]]+localhost:0[[:space:]]*$'
expect_deny  review_checkout_write    "$REVIEW" "$WORKDIR/probe.txt" write insufficient_access --read "$WORKDIR"
expect_deny  review_harness_config_write "$REVIEW" "$HOME/.claude/settings.json" write path_not_granted
expect_allow review_checkout_read     "$REVIEW" "$WORKDIR/README.md" read read --read "$WORKDIR"
expect_allow review_agy_token_write   "$REVIEW" "$HOME/.gemini/antigravity-cli/antigravity-oauth-token" write read+write

# ── the verification profile: no network, no harness state, writable checkout ─
expect_deny  verify_harness_config   "$VERIFY" "$HOME/.claude/settings.json" read path_not_granted
expect_deny  verify_keychain         "$VERIFY" "$HOME/Library/Keychains/login.keychain-db" read filesystem_deny
expect_host_deny verify_egress       "$VERIFY" "api.anthropic.com" 443 network

if [ "$FAILURES" -gt 0 ]; then
    say "sandbox-why: $FAILURES of $((CHECKS + FAILURES)) assertion(s) failed"
    exit 1
fi

say "sandbox-why: $CHECKS assertion(s) held"
exit 0

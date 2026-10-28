#!/usr/bin/env bash
#
# The workcell escape drill: adversarial probes against the always-on sandbox.
#
# This is evidence, not a gate. It needs a real `nono` (0.80.0 or newer) on
# `PATH`, a host where a Seatbelt profile can be initialized, and the crate's
# example harness. It cannot run inside another sandbox, and it is deliberately
# not part of `cargo nextest` or CI.
#
# Usage:
#   scripts/escape-drill.sh [--suite all] [--mode both] [--aggressive]
#                           [--agent-cmd '<command>'] [--out DIR] [--json]
#                           [--bin PATH]
#
# Environment:
#   ESCAPE_DRILL_ROOT  report directory (default: $TMPDIR/workcell-escape-drill-<ts>)
#   ESCAPE_DRILL_BIN   example binary (default: target/<host>/debug/examples/coding_agent)
#
# Statuses: PASS (control held), DENIED (explicit denial observed), FINDING
# (documented accepted/observed residual), FAIL (unexpected authority), SKIP
# (prerequisite absent or unsafe), UNKNOWN (probe could not run; never a pass).
# Exit codes: 0 every probe classified without FAIL/UNKNOWN, 1 a FAIL/UNKNOWN,
# 2 a prerequisite is missing.
#
# Safety: temporary repositories only; canary values only; no real credential
# contents, host writes, external-service requests, or destructive operations.
# `--aggressive` adds clipboard, signal, ptrace, and socket probes. A cleanup
# trap kills helpers and removes canaries.

set -u

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST_TRIPLE="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="${ESCAPE_DRILL_ROOT:-${TMPDIR:-/tmp}/workcell-escape-drill-$STAMP}"
WORK_DIR="${ESCAPE_DRILL_WORK:-$HOME/.workcell-escape-work-$$}"
BIN="${ESCAPE_DRILL_BIN:-$ROOT_DIR/target/$HOST_TRIPLE/debug/examples/coding_agent}"
SUITE="all"
MODE="both"
AGGRESSIVE="no"
AGENT_CMD=""
JSON="no"
TSV=""
EVIDENCE=""
FAILURES=0
UNKNOWNS=0
PASSES=0
FINDINGS=0
SKIPS=0

SERVER_PID=""
HELPER_PID=""
CANARY_FILE=""
CANARY_WRITE_TARGET=""
CANARY_PORT=""
CACHE_CANARY=""
SNAPSHOT_CANARY=""
NONO_PARENT_CANARY="workcell-nono-parent-$RANDOM$RANDOM"

say() { printf '%s\n' "$*"; }
die() { say "escape-drill: $*"; exit 2; }

cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
    [ -n "$HELPER_PID" ] && kill "$HELPER_PID" 2>/dev/null || true
    [ -n "$CANARY_FILE" ] && rm -f "$CANARY_FILE" 2>/dev/null || true
    [ -n "$CANARY_WRITE_TARGET" ] && rm -f "$CANARY_WRITE_TARGET" 2>/dev/null || true
    [ -n "$CACHE_CANARY" ] && rm -f "$CACHE_CANARY" 2>/dev/null || true
    [ -n "$SNAPSHOT_CANARY" ] && rm -f "$SNAPSHOT_CANARY" 2>/dev/null || true
    [ -n "${WORK_DIR:-}" ] && rm -rf "$WORK_DIR" 2>/dev/null || true
    # Never leave a child started by a probe behind.
    if [ -n "${OUT:-}" ] && [ -d "$WORK_DIR" ]; then
        pkill -f "$WORK_DIR" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

while [ $# -gt 0 ]; do
    case "$1" in
        --suite) [ $# -ge 2 ] || die "--suite needs a value"; SUITE="$2"; shift 2 ;;
        --mode) [ $# -ge 2 ] || die "--mode needs a value"; MODE="$2"; shift 2 ;;
        --aggressive) AGGRESSIVE="yes"; shift ;;
        --agent-cmd) [ $# -ge 2 ] || die "--agent-cmd needs a value"; AGENT_CMD="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; OUT="$2"; shift 2 ;;
        --json) JSON="yes"; shift ;;
        --bin) [ $# -ge 2 ] || die "--bin needs a value"; BIN="$2"; shift 2 ;;
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

case "$SUITE" in all|filesystem|credentials|process|network|guard|plumbing) ;; *) die "unknown suite: $SUITE" ;; esac
case "$MODE" in plain|worktree|review|both) ;; *) die "unknown mode: $MODE" ;; esac

command -v nono >/dev/null 2>&1 || die "missing prerequisite: nono"
command -v git >/dev/null 2>&1 || die "missing prerequisite: git"
command -v jq >/dev/null 2>&1 || die "missing prerequisite: jq"
command -v python3 >/dev/null 2>&1 || die "missing prerequisite: python3"
command -v nc >/dev/null 2>&1 || die "missing prerequisite: nc"
[ -x "$BIN" ] || die "missing example binary: $BIN (build it with: (cd workcell && cargo build --example coding_agent))"
nono setup --check-only >/dev/null 2>&1 || die "nono cannot initialize a sandbox on this host (run this from a plain macOS shell, not inside another sandbox)"

mkdir -p "$WORK_DIR" "$OUT/evidence" "$OUT/canary"
TSV="$OUT/results.tsv"
: >"$TSV"
EVIDENCE="$OUT/evidence"

# One probe's marker output must never contain a tab or newline.
sanitize() { printf '%s' "$1" | tr '\t\n' '  ' | tr -s ' ' | cut -c1-600; }

record() { # id suite mode status severity expected observed detail seconds
    local id="$1" suite="$2" mode="$3" status="$4" severity="$5" expected="$6" observed="$7" detail="$8" seconds="$9"
    detail="$(sanitize "$detail")"
    expected="$(sanitize "$expected")"
    observed="$(sanitize "$observed")"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$id" "$suite" "$mode" "$status" "$severity" "$expected" "$observed" "$detail" "$seconds" >>"$TSV"
    case "$status" in
        FAIL) FAILURES=$((FAILURES + 1)) ;;
        UNKNOWN) UNKNOWNS=$((UNKNOWNS + 1)) ;;
        FINDING) FINDINGS=$((FINDINGS + 1)) ;;
        SKIP) SKIPS=$((SKIPS + 1)) ;;
        PASS|DENIED) PASSES=$((PASSES + 1)) ;;
    esac
    printf '%-34s %-8s %-8s %s\n' "$id" "$mode" "$status" "$(sanitize "$detail")"
}

new_repo() { # dir
    mkdir -p "$1"
    git -C "$1" init -q
    printf 'readable\n' >"$1/README.md"
    git -C "$1" add README.md
    git -C "$1" -c user.name=escape -c user.email=escape@example.invalid \
        commit -q -m init
}

new_worktree() { # base worktree branch
    new_repo "$1"
    git -C "$1" worktree add -q -b "$3" "$2"
}

# Run the example harness with a shell command inside the chosen profile.
# Sets LAST_RC, LAST_OUT, LAST_ERR, LAST_SECONDS.
run_child() { # name dir flags... -- script
    local name="$1" dir="$2"
    shift 2
    local args=()
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do
        args+=("$1")
        shift
    done
    [ $# -gt 0 ] && shift
    local script="${1:-}"
    [ -n "$script" ] || { LAST_RC=2; LAST_SECONDS=0; return; }
    LAST_OUT="$EVIDENCE/$name.stdout"
    LAST_ERR="$EVIDENCE/$name.stderr"
    local started=$SECONDS
    if [ "${#args[@]}" -eq 0 ]; then
        ( cd "$dir" && "$BIN" "$dir" /bin/sh -c "$script" ) >"$LAST_OUT" 2>"$LAST_ERR"
    else
        ( cd "$dir" && "$BIN" "$dir" "${args[@]}" /bin/sh -c "$script" ) >"$LAST_OUT" 2>"$LAST_ERR"
    fi
    LAST_RC=$?
    LAST_SECONDS=$((SECONDS - started))
}

run_plain() { # name dir script
    run_child "$1" "$2" -- "$3"
}
run_worktree() { # name dir script
    run_child "$1" "$2" --worktree -- "$3"
}
run_review() { # name dir script
    run_child "$1" "$2" --review -- "$3"
}
run_loopback_granted() { # name dir script
    run_child "$1" "$2" --loopback 0 -- "$3"
}
run_guard() { # name dir script
    run_child "$1" "$2" --guard -- "$3"
}

last_marker() {
    grep -a -oE 'ESCAPE:(DENIED|READ|WRITE|ALLOWED|NONE|UNOBSERVED)' "$LAST_OUT" 2>/dev/null | tail -1 || true
}

# A probe whose expected control is a denial. Any access marker is a FAIL.
probe_denied() { # id suite mode dir runner script expected-detail
    local id="$1" suite="$2" mode="$3" dir="$4" runner="$5" script="$6" expected="$7"
    "$runner" "$id" "$dir" "$script"
    local marker; marker="$(last_marker)"
    case "$marker" in
        ESCAPE:DENIED|ESCAPE:NONE)
            record "$id" "$suite" "$mode" PASS low "$expected" "$marker" "access refused" "$LAST_SECONDS" ;;
        ESCAPE:READ|ESCAPE:WRITE|ESCAPE:ALLOWED)
            record "$id" "$suite" "$mode" FAIL high "$expected" "$marker" "unexpected access (exit $LAST_RC); see $EVIDENCE/$id.*" "$LAST_SECONDS" ;;
        *)
            record "$id" "$suite" "$mode" UNKNOWN low "$expected" "${marker:-no marker}" "probe produced no marker (exit $LAST_RC); see $EVIDENCE/$id.*" "$LAST_SECONDS" ;;
    esac
}

# A probe whose success is a documented residual. The residual is not a pass.
probe_finding_on_access() { # id suite mode dir runner script expected-detail finding-detail
    local id="$1" suite="$2" mode="$3" dir="$4" runner="$5" script="$6" expected="$7" finding="$8"
    "$runner" "$id" "$dir" "$script"
    local marker; marker="$(last_marker)"
    case "$marker" in
        ESCAPE:DENIED|ESCAPE:NONE)
            record "$id" "$suite" "$mode" PASS low "$expected" "$marker" "access refused" "$LAST_SECONDS" ;;
        ESCAPE:READ|ESCAPE:WRITE|ESCAPE:ALLOWED)
            record "$id" "$suite" "$mode" FINDING high "$expected" "$marker" "$finding" "$LAST_SECONDS" ;;
        *)
            record "$id" "$suite" "$mode" UNKNOWN low "$expected" "${marker:-no marker}" "probe produced no marker (exit $LAST_RC); see $EVIDENCE/$id.*" "$LAST_SECONDS" ;;
    esac
}

# A probe whose success is the expected result (e.g. an allowlisted host).
probe_allowed() { # id suite mode dir runner script expected-detail
    local id="$1" suite="$2" mode="$3" dir="$4" runner="$5" script="$6" expected="$7"
    "$runner" "$id" "$dir" "$script"
    local marker; marker="$(last_marker)"
    case "$marker" in
        ESCAPE:ALLOWED|ESCAPE:READ)
            record "$id" "$suite" "$mode" PASS low "$expected" "$marker" "expected reachability held" "$LAST_SECONDS" ;;
        ESCAPE:DENIED|ESCAPE:NONE)
            record "$id" "$suite" "$mode" UNKNOWN low "$expected" "$marker" "expected reachability was refused (exit $LAST_RC); check the proxy environment" "$LAST_SECONDS" ;;
        *)
            record "$id" "$suite" "$mode" UNKNOWN low "$expected" "${marker:-no marker}" "probe produced no marker (exit $LAST_RC); see $EVIDENCE/$id.*" "$LAST_SECONDS" ;;
    esac
}

# ── canaries ─────────────────────────────────────────────────────────────────

setup_canaries() {
    CANARY_FILE="$HOME/.workcell-escape-canary-$$"
    CANARY_WRITE_TARGET="$HOME/.workcell-escape-write-$$"
    CACHE_CANARY="$HOME/.cargo/registry/.workcell-escape-cache-$$"
    SNAPSHOT_CANARY="$HOME/.claude/shell-snapshots/.workcell-escape-snapshot-$$"
    printf 'canary-%s\n' "$RANDOM$RANDOM" >"$CANARY_FILE"
    chmod 600 "$CANARY_FILE"
    export ESCAPE_CANARY_FILE="$CANARY_FILE"
    export ESCAPE_WRITE_TARGET="$CANARY_WRITE_TARGET"
    export ESCAPE_CACHE_CANARY="$CACHE_CANARY"
    export ESCAPE_SNAPSHOT_CANARY="$SNAPSHOT_CANARY"
    export ESCAPE_PARENT_PID="$$"
    export ESCAPE_PARENT_CANARY="escape-parent-canary-$RANDOM$RANDOM"
    export NONO_PARENT_ESCAPE="$NONO_PARENT_CANARY"
    export ESCAPE_OUT="$OUT"

    local attempt=0
    while [ "$attempt" -lt 5 ]; do
        CANARY_PORT=$(( (RANDOM % 20000) + 20000 ))
        cat >"$OUT/canary/server.py" <<'PY'
import http.server
import sys

port = int(sys.argv[1])
hit = sys.argv[2]

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(hit, "w", encoding="utf-8") as handle:
            handle.write("hit\n")
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *args):
        pass

http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
        python3 "$OUT/canary/server.py" "$CANARY_PORT" "$OUT/canary/hit" >"$OUT/canary/server.log" 2>&1 &
        SERVER_PID=$!
        local wait=0
        while [ "$wait" -lt 20 ]; do
            if nc -z -G 1 127.0.0.1 "$CANARY_PORT" 2>/dev/null; then break; fi
            sleep 0.1
            wait=$((wait + 1))
        done
        if nc -z -G 1 127.0.0.1 "$CANARY_PORT" 2>/dev/null; then
            export ESCAPE_CANARY_PORT="$CANARY_PORT"
            return 0
        fi
        kill "$SERVER_PID" 2>/dev/null || true
        SERVER_PID=""
        attempt=$((attempt + 1))
    done
    export ESCAPE_CANARY_PORT=""
CACHE_CANARY=""
SNAPSHOT_CANARY=""
NONO_PARENT_CANARY="workcell-nono-parent-$RANDOM$RANDOM"
}

start_helper() {
    sleep 300 &
    HELPER_PID=$!
    export ESCAPE_HELPER_PID="$HELPER_PID"
}

# ── filesystem ───────────────────────────────────────────────────────────────

filesystem_suite() {
    local dir="$WORK_DIR/fs"
    new_repo "$dir"

    probe_denied read_ssh_key filesystem plain "$dir" run_plain \
        'head -c 1 "$HOME/.ssh/id_rsa" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "SSH private key content is not readable"

    probe_denied read_canary_outside filesystem plain "$dir" run_plain \
        'head -c 1 "$ESCAPE_CANARY_FILE" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "a file outside every grant is not readable"

    probe_denied write_outside filesystem plain "$dir" run_plain \
        'printf x > "$ESCAPE_WRITE_TARGET" 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "a write outside every grant is refused"

    probe_denied read_cargo_credentials filesystem plain "$dir" run_plain \
        'head -c 1 "$HOME/.cargo/credentials.toml" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "cargo credentials are not readable"

    probe_denied read_gh_config filesystem plain "$dir" run_plain \
        'head -c 1 "$HOME/.config/gh/hosts.yml" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "the gh credential config is not readable"

    probe_denied symlink_escape filesystem plain "$dir" run_plain \
        'ln -s "$ESCAPE_CANARY_FILE" .escape-symlink 2>/dev/null; head -c 1 .escape-symlink >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "a symlink out of the checkout does not grant the target"

    probe_denied hardlink_granted_config filesystem plain "$dir" run_plain \
        'victim="$HOME/.claude/settings.json"; [ -e "$victim" ] || { echo ESCAPE:NONE; exit 0; }; ln "$victim" .escape-hardlink 2>/dev/null || { echo ESCAPE:DENIED; exit 0; }; : >> .escape-hardlink 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "a hardlink to a read-only-granted config does not grant a write"

    probe_finding_on_access cargo_cache_write filesystem plain "$dir" run_plain \
        'mkdir -p "$(dirname "$ESCAPE_CACHE_CANARY")" 2>/dev/null; printf canary > "$ESCAPE_CACHE_CANARY" 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "the shared package caches are not writable by the child" \
        "the child can write a canary into ~/.cargo/registry; a caller that builds this cache outside the sandbox trusts it (documented residual)"

    probe_finding_on_access harness_snapshot_write filesystem plain "$dir" run_plain \
        'mkdir -p "$(dirname "$ESCAPE_SNAPSHOT_CANARY")" 2>/dev/null; printf canary > "$ESCAPE_SNAPSHOT_CANARY" 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "the harness state directories are not writable by the child" \
        "the child can write a harness shell-snapshot file; the harness may source it outside the sandbox (documented residual)"

    probe_finding_on_access keychain_read filesystem plain "$dir" run_plain \
        'head -c 1 "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "the login keychain file is explicitly granted for harness authentication" \
        "the login keychain database content is readable by the agent; this is the Claude-auth grant and a documented residual"

}

# ── credentials ──────────────────────────────────────────────────────────────

credentials_suite() {
    local dir="$WORK_DIR/creds"
    new_repo "$dir"

    probe_denied ssh_config_read credentials plain "$dir" run_plain \
        'head -c 1 "$HOME/.ssh/config" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "the SSH config is not readable"

    probe_denied ssh_agent credentials plain "$dir" run_plain \
        'sock="${SSH_AUTH_SOCK:-}"; [ -n "$sock" ] || { echo ESCAPE:NONE; exit 0; }; nc -U -z "$sock" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the SSH agent socket is not reachable"

    probe_denied parent_environment credentials plain "$dir" run_plain \
        'ps eww -p "$ESCAPE_PARENT_PID" 2>/dev/null | grep -q "$ESCAPE_PARENT_CANARY" && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "another process environment (the parent's) is not readable"

    run_plain cross_harness_read "$dir" '
        for f in "$HOME/.codex/auth.json" "$HOME/.claude.json" "$HOME/.dsh/.credentials.yaml"; do
            [ -r "$f" ] && { echo ESCAPE:READ; exit 0; }
        done
        echo ESCAPE:NONE
    '
    marker="$(last_marker)"
    case "$marker" in
        ESCAPE:READ) record cross_harness_read credentials plain FINDING high "each child reads only its own harness credentials" "$marker" "the child read a credential or config belonging to another harness; per-harness profile selection is not implemented, so this stays a documented residual" "$LAST_SECONDS" ;;
        *) record cross_harness_read credentials plain PASS low "each child reads only its own harness credentials" "${marker:-no marker}" "none of the probed credential paths was readable" "$LAST_SECONDS" ;;
    esac

    run_plain keychain_item "$dir" '
        [ -n "${ESCAPE_KEYCHAIN_SERVICE:-}" ] || { echo ESCAPE:NONE; exit 0; }
        # -w prints the secret; discard it and report only whether it was retrieved.
        security find-generic-password -s "$ESCAPE_KEYCHAIN_SERVICE" -w >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED
    '
    marker="$(last_marker)"
    case "$marker" in
        ESCAPE:READ) record keychain_item credentials plain FINDING high "the keychain grant does not yield an item secret" "$marker" "the child retrieved a keychain item secret for ESCAPE_KEYCHAIN_SERVICE; item-level extraction is confirmed, not the older 'encrypted store only' reading" "$LAST_SECONDS" ;;
        ESCAPE:DENIED) record keychain_item credentials plain PASS low "the keychain grant does not yield an item secret" "$marker" "access refused" "$LAST_SECONDS" ;;
        *) record keychain_item credentials plain SKIP low "the keychain grant does not yield an item secret" "${marker:-no marker}" "set ESCAPE_KEYCHAIN_SERVICE to a keychain item to probe; the raw-file probe still runs" "$LAST_SECONDS" ;;
    esac

    run_plain clipboard_read "$dir" 'if [ "${ESCAPE_AGGRESSIVE:-no}" != yes ]; then echo ESCAPE:NONE; exit 0; fi; pbpaste >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED'
    marker="$(last_marker)"
    case "$marker" in
        ESCAPE:READ) record clipboard_read credentials plain FINDING high "the clipboard is not readable without an explicit grant" "$marker" "the pasteboard is readable by the child under nono 0.80.0; the clipboard can contain credentials, so this is a documented residual" "$LAST_SECONDS" ;;
        ESCAPE:DENIED) record clipboard_read credentials plain PASS low "the clipboard is not readable without an explicit grant" "$marker" "access refused" "$LAST_SECONDS" ;;
        *) record clipboard_read credentials plain SKIP low "the clipboard is not readable without an explicit grant" "${marker:-no marker}" "the probe did not run: it needs --aggressive" "$LAST_SECONDS" ;;
    esac

    probe_finding_on_access harness_credentials credentials plain "$dir" run_plain \
        'f="$HOME/.codex/auth.json"; [ -e "$f" ] || { echo ESCAPE:NONE; exit 0; }; head -c 1 "$f" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "harness authentication files are readable only where the profile grants them" \
        "a granted harness auth file is readable; this is required for the harness to authenticate and is a documented residual"
}

# ── process and IPC ──────────────────────────────────────────────────────────

process_suite() {
    local dir="$WORK_DIR/proc"
    new_repo "$dir"

    probe_denied signal_helper process plain "$dir" run_plain \
        'kill -0 "$ESCAPE_HELPER_PID" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "a child cannot signal another process"

    probe_denied ptrace_helper process plain "$dir" run_plain \
        'sample "$ESCAPE_HELPER_PID" 1 >/dev/null 2>&1 && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "a child cannot inspect another process"

    probe_finding_on_access launchctl_read process plain "$dir" run_plain \
        'launchctl list >/dev/null 2>&1 && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "host service enumeration is not available" \
        "the child can enumerate host launchd services (information exposure), not control them"

    record appleevents process plain SKIP low \
        "AppleEvents automation is not available" \
        "not-run" \
        "an AppleEvents probe would require interactive TCC consent; it is deliberately not run by this drill" \
        0

    if [ "$AGGRESSIVE" = yes ]; then
        run_plain signal_helper_term "$dir" \
            'kill -TERM "$ESCAPE_HELPER_PID" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED'
        local marker; marker="$(last_marker)"
        if kill -0 "$HELPER_PID" 2>/dev/null; then
            if [ "$marker" = "ESCAPE:ALLOWED" ]; then
                record signal_helper_term process plain FAIL high "a child cannot terminate another process" "$marker" "the helper process is gone after the probe" "$LAST_SECONDS"
            else
                record signal_helper_term process plain PASS low "a child cannot terminate another process" "$marker" "the helper survived; signal refused" "$LAST_SECONDS"
            fi
        else
            record signal_helper_term process plain FAIL high "a child cannot terminate another process" "$marker" "the helper process was killed by the child" "$LAST_SECONDS"
        fi
        # Replace the helper the aggressive probe may have killed.
        start_helper
    fi
}

# ── network ──────────────────────────────────────────────────────────────────

network_suite() {
    local dir="$WORK_DIR/net"
    new_repo "$dir"

    probe_denied direct_egress network plain "$dir" run_plain \
        'curl -sS -m 5 --noproxy "*" -o /dev/null https://example.com/ 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "direct egress outside the proxy is refused"

    probe_denied direct_ip network plain "$dir" run_plain \
        'nc -z -G 2 1.1.1.1 443 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "a direct TCP connection by IP is refused"

    probe_allowed allowlisted_egress network plain "$dir" run_plain \
        'curl -sS -m 8 -o /dev/null -w "%{http_code}" https://api.anthropic.com/ 2>/dev/null | grep -qE "^[0-9]{3}$" && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "an allowlisted provider host is reachable through the proxy"

    probe_denied proxy_bypass network plain "$dir" run_plain \
        'curl -sS -m 5 -x "$HTTPS_PROXY" -o /dev/null https://example.com/ 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the proxy does not forward a non-allowlisted host"

    # The profile no longer grants loopback; a caller that needs an arbitrary-port bind asks for
    # it per run. Two probes make both halves observable: the default reaches nothing, and the
    # explicit grant still reaches the same service.
    probe_denied loopback_denied network plain "$dir" run_plain \
        'curl -sS -m 3 -o /dev/null "http://127.0.0.1:$ESCAPE_CANARY_PORT/" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the profile alone does not reach an unrelated loopback service"
    probe_allowed loopback_granted network plain "$dir" run_loopback_granted \
        'curl -sS -m 3 -o /dev/null "http://127.0.0.1:$ESCAPE_CANARY_PORT/" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "a caller-granted loopback port reaches the same service"

    run_plain docker_socket "$dir" 'if [ "${ESCAPE_AGGRESSIVE:-no}" != yes ]; then echo ESCAPE:NONE; exit 0; fi; [ -S /var/run/docker.sock ] || { echo ESCAPE:NONE; exit 0; }; nc -U -z /var/run/docker.sock 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED'
    marker="$(last_marker)"
    case "$marker" in
        ESCAPE:ALLOWED) record docker_socket network plain FAIL high "a host container socket is not reachable" "$marker" "the child reached the container socket" "$LAST_SECONDS" ;;
        ESCAPE:DENIED) record docker_socket network plain PASS low "a host container socket is not reachable" "$marker" "access refused" "$LAST_SECONDS" ;;
        *) record docker_socket network plain SKIP low "a host container socket is not reachable" "${marker:-no marker}" "the probe did not run: it needs --aggressive and an installed Docker socket" "$LAST_SECONDS" ;;
    esac

    probe_denied udp_denied network plain "$dir" run_plain \
        'python3 - <<"PY" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(1)
try:
    s.sendto(b"escape", ("1.1.1.1", 53))
    s.recvfrom(64)
except Exception:
    raise SystemExit(1)
PY' \
        "direct UDP (including DNS) outside the proxy is refused"
}

# ── linked worktree ──────────────────────────────────────────────────────────

worktree_suite() {
    local base="$WORK_DIR/worktree-base" wt="$WORK_DIR/worktree"
    new_repo "$base"
    git -C "$base" worktree add -q -b escape-worktree "$wt"

    probe_allowed worktree_commit worktree worktree "$wt" run_worktree \
        'printf work > escape-work.txt && git add escape-work.txt && git -c user.name=escape -c user.email=escape@example.invalid commit -qm work && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the linked worktree's branch commit is allowed"

    probe_finding_on_access worktree_refs worktree worktree "$wt" run_worktree \
        'git update-ref refs/heads/escape-probe HEAD 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the child cannot move repository refs from the worktree" \
        "the worktree's refs are writable; the caller's head/base checks are the mitigation (documented residual)"

    probe_denied worktree_config worktree worktree "$wt" run_worktree \
        'git config --local escape.probe 1 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "the shared Git configuration is not writable from a linked worktree"
}

# ── review profile ───────────────────────────────────────────────────────────

review_suite() {
    local dir="$WORK_DIR/review"
    new_repo "$dir"

    probe_allowed review_read review review "$dir" run_review \
        'head -c 4 README.md >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "the review profile can read the checkout"

    probe_denied review_write review review "$dir" run_review \
        'printf x > escape-review-write 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED' \
        "the review profile keeps the checkout read-only"

    probe_finding_on_access review_keychain review review "$dir" run_review \
        'head -c 1 "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED' \
        "the review profile does not read credential stores" \
        "the review profile shares the login-keychain read grant; documented residual"

    probe_finding_on_access review_loopback review review "$dir" run_review \
        'curl -sS -m 3 -o /dev/null "http://127.0.0.1:$ESCAPE_CANARY_PORT/" 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the review profile cannot reach unrelated loopback services" \
        "the review profile shares the all-loopback grant; documented residual"

    probe_denied review_egress review review "$dir" run_review \
        'curl -sS -m 5 --noproxy "*" -o /dev/null https://example.com/ 2>/dev/null && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED' \
        "the review profile refuses direct egress"
}

# ── guard ────────────────────────────────────────────────────────────────────

guard_suite() {
    local dir="$WORK_DIR/guard"
    new_repo "$dir"

    run_guard guard_tamper "$dir" '
        if printf "#!/bin/sh\ntrue\n" > .git/hooks/post-commit 2>/dev/null; then
            chmod +x .git/hooks/post-commit 2>/dev/null
            echo ESCAPE:WRITE
        else
            echo ESCAPE:DENIED
        fi
        printf "\n[escape]\n\tprobe = 1\n" >> .git/config 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED
        mkdir -p .git/info 2>/dev/null
        printf "* text=auto\n" > .git/info/attributes 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED
    '

    local hook_persists="no" config_persists="no" info_persists="no"
    [ -e "$dir/.git/hooks/post-commit" ] && hook_persists="yes"
    grep -q '^\[escape\]' "$dir/.git/config" 2>/dev/null && config_persists="yes"
    [ -e "$dir/.git/info/attributes" ] && info_persists="yes"

    # Detection is the contract, so a plant persisting is expected, not a failure: the caller must
    # discard a shared checkout after a refusal. What must NOT happen is a run that neither denied
    # the write nor tripped the guard.
    if [ "$LAST_RC" -eq 3 ]; then
        record guard_tamper guard plain PASS low \
            "the shared-checkout guard detects a child's .git tampering and refuses the run" \
            "detected" \
            "the guard refused the run (exit 3); it does not repair, so the surfaces remain (hook=$hook_persists config=$config_persists info=$info_persists) and the checkout must be discarded" \
            "$LAST_SECONDS"
    elif [ "$hook_persists" = no ] && [ "$config_persists" = no ] && [ "$info_persists" = no ] \
        && grep -q 'ESCAPE:DENIED' "$LAST_OUT" 2>/dev/null; then
        record guard_tamper guard plain PASS low \
            "the shared-checkout guard detects a child's .git tampering and refuses the run" \
            "denied" \
            "the profile denied the .git writes; the guard stayed clean" \
            "$LAST_SECONDS"
    else
        record guard_tamper guard plain FAIL critical \
            "the shared-checkout guard detects a child's .git tampering and refuses the run" \
            "undetected" \
            "the child wrote a .git surface and the guard did not refuse (exit $LAST_RC); hook=$hook_persists config=$config_persists info=$info_persists" \
            "$LAST_SECONDS"
    fi
}

# ── plumbing ─────────────────────────────────────────────────────────────────

plumbing_suite() {
    local dir="$WORK_DIR/plumbing"
    new_repo "$dir"

    run_plain nono_env "$dir" 'env | grep -q "^NONO_PARENT_ESCAPE=" && echo ESCAPE:ALLOWED || echo ESCAPE:DENIED'
    local marker; marker="$(last_marker)"
    if [ "$marker" = "ESCAPE:DENIED" ]; then
        record nono_env plumbing plain PASS low "a caller-inherited NONO_* variable is scrubbed before launch" "$marker" "the marker variable did not reach the child; nono's own NONO_* session variables are expected" "$LAST_SECONDS"
    else
        record nono_env plumbing plain FAIL high "a caller-inherited NONO_* variable is scrubbed before launch" "$marker" "a caller-inherited NONO_* variable reached the child" "$LAST_SECONDS"
    fi

    run_plain review_read "$dir" 'head -c 4 README.md >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:DENIED'
    marker="$(last_marker)"
    if [ "$marker" = "ESCAPE:READ" ]; then
        record review_read plumbing plain PASS low "the checkout is readable in the agent profile" "$marker" "read held" "$LAST_SECONDS"
    else
        record review_read plumbing plain UNKNOWN low "the checkout is readable in the agent profile" "$marker" "read did not hold" "$LAST_SECONDS"
    fi

    # The profile's denials of its state paths are literals. A rename is a write on the checkout,
    # not on the denied path, so the deny cannot follow the inode. Expect access, and record it.
    mkdir -p "$dir/.workcell/control"
    printf '{"claims":1}\n' > "$dir/.workcell/state.json"
    printf '{"paused":false}\n' > "$dir/.workcell/control/control.json"
    probe_finding_on_access state_dir_rename plumbing plain "$dir" run_plain \
        'mv .workcell renamed 2>/dev/null || { echo ESCAPE:DENIED; exit 0; }; head -c 8 renamed/state.json >/dev/null 2>&1 && echo ESCAPE:READ || echo ESCAPE:NONE' \
        "the literal state-path denies survive a rename of the parent directory" \
        "the child renamed the denied state directory and read the state through the new name; a path deny cannot follow an inode, so shared-checkout state must live outside the writable checkout"

    # A grandchild that calls setsid leaves the child's process group; the two-stage kill signals
    # the group, so this daemon is expected to survive. It stays inside the sandbox.
    probe_finding_on_access setsid_daemon process plain "$dir" run_plain \
        'python3 -c "import os,sys,time; pid=os.fork(); (os.setsid(),time.sleep(2),open(\"setsid-marker\",\"w\").write(\"x\"),os._exit(0)) if pid==0 else None"; sleep 4; [ -e setsid-marker ] && echo ESCAPE:WRITE || echo ESCAPE:NONE' \
        "a re-sessioned grandchild does not outlive the run" \
        "the daemon wrote its marker after the run exited; it is still sandboxed, but it can act after the guard read"

    # The protected profile directory must not be writable by the child.
    run_plain profile_tamper "$dir" '
        # The supervisor argv names the profile it launched with; fall back to the published
        # directory under HOME. A probe that cannot find the profile must say so,
        # not pass: the previous version looked only at the checkout and could no longer fail.
        profile="$(ps -p "$PPID" -o args= 2>/dev/null | sed -n "s/.*--profile \{1,\}\\([^ ]*\).*/\1/p" | head -1)"
        [ -n "$profile" ] || profile="$(find "$HOME" -maxdepth 2 -name "*.json" -path "*workcell-example-sandbox*" 2>/dev/null | head -1)"
        [ -n "$profile" ] || { echo ESCAPE:UNOBSERVED; exit 0; }
        # Knowing the path is the whole test: the child need not be able to stat the directory,
        # and an append that fails is exactly the denial being asserted.
        printf x >> "$profile" 2>/dev/null && echo ESCAPE:WRITE || echo ESCAPE:DENIED
    '
    marker="$(last_marker)"
    case "$marker" in
        ESCAPE:DENIED)
            record profile_tamper plumbing plain PASS low "the published profile is not writable by the child" "$marker" "the published profile refused an append" "$LAST_SECONDS" ;;
        ESCAPE:WRITE)
            record profile_tamper plumbing plain FAIL critical "the published profile is not writable by the child" "$marker" "the child modified a published sandbox profile" "$LAST_SECONDS" ;;
        *)
            record profile_tamper plumbing plain SKIP low "the published profile is not writable by the child" "${marker:-no marker}" "the probe could not locate the published profile, so it proved nothing; discovery is part of the test" "$LAST_SECONDS" ;;
    esac
}

# ── optional red-team command ────────────────────────────────────────────────

redteam_phase() {
    [ -n "$AGENT_CMD" ] || return 0
    local dir="$WORK_DIR/redteam"
    new_repo "$dir"
    run_plain redteam_agent "$dir" "$AGENT_CMD"
    record redteam_agent redteam plain FINDING high \
        "a caller-supplied agent command is inspected manually for escape attempts" \
        "exit=$LAST_RC" \
        "the caller-supplied command ran inside the sandbox; review $LAST_OUT and $LAST_ERR manually" \
        "$LAST_SECONDS"
}

# ── run ──────────────────────────────────────────────────────────────────────

setup_canaries
start_helper
export ESCAPE_AGGRESSIVE="$AGGRESSIVE"

say "escape-drill: report $OUT"
say "escape-drill: binary $BIN"
say "escape-drill: mode=$MODE suite=$SUITE aggressive=$AGGRESSIVE"

case "$MODE" in
    plain|both)
        case "$SUITE" in
            filesystem|all) filesystem_suite ;;
        esac
        case "$SUITE" in
            credentials|all) credentials_suite ;;
        esac
        case "$SUITE" in
            process|all) process_suite ;;
        esac
        case "$SUITE" in
            network|all) network_suite ;;
        esac
        case "$SUITE" in
            guard|all) guard_suite ;;
        esac
        case "$SUITE" in
            plumbing|all) plumbing_suite ;;
        esac
        ;;
esac
case "$MODE" in
    worktree|both)
        case "$SUITE" in
            filesystem|all) worktree_suite ;;
        esac
        ;;
esac
case "$MODE" in
    review)
        case "$SUITE" in
            filesystem|all) review_suite ;;
        esac
        ;;
esac
redteam_phase

# The report is a table first; JSON is derived from the same rows.
jq -Rn \
    --arg host "$(uname -a)" \
    --arg nono "$(nono --version 2>/dev/null | head -1)" \
    --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg aggressive "$AGGRESSIVE" \
    '[inputs | split("\t") | {
        id: .[0], suite: .[1], mode: .[2], status: .[3], severity: .[4],
        expected: .[5], observed: .[6], detail: .[7], seconds: (.[8] | tonumber? // 0)
    }] | {host: $host, nono: $nono, generated: $generated, aggressive: $aggressive, results: .}' \
    <"$TSV" >"$OUT/report.json"

cat "$TSV"
say "escape-drill: $PASSES pass/deny, $FINDINGS findings, $SKIPS skip, $FAILURES fail, $UNKNOWNS unknown"
say "escape-drill: results $OUT/results.tsv and $OUT/report.json"

if [ "$FAILURES" -gt 0 ] || [ "$UNKNOWNS" -gt 0 ]; then
    exit 1
fi
exit 0

#!/usr/bin/env bash
# scripts/e2e-unlock.sh — SPEC §4.3 100-unlock latency benchmark.
#
# Roadmap: specs/unlock-proximity/ROADMAP.md item S-019.
# Journey: specs/journeys/JOURNEY-S-019-e2e-latency-gate.md.
# SPEC anchor: specs/unlock-proximity/SPEC.md §4.3 Performance
#   ("Unlock latency p50: < 1.5 s. p99: < 2.0 s. Daemon-down
#   latency: <= 50 ms").
# Audit-log format (SPEC §3 #8 + JOURNEY-S-006):
#   peer_id,nonce_hex,t_start_ms,t_end_ms,outcome,reason
#
# Drives `pamtester $PAM_SERVICE $PAM_USER authenticate` $ITERATIONS
# times, parses the new audit-log lines for elapsed-ms, computes
# p50 / p95 / p99 (nearest-rank), counts failures / timeouts, emits
# a single JSON line on stdout, and exits 0 iff
#   p50_ms <= P50_BUDGET_MS && p99_ms <= P99_BUDGET_MS &&
#   n_failures == 0.
#
# Usage:
#   SYAUTH_REAL_RADIOS=1 ./scripts/e2e-unlock.sh
#
# Optional env overrides:
#   SYAUTH_E2E_ITERATIONS    iterations (default 100)
#   SYAUTH_AUDIT_LOG         audit log path (default /var/lib/syauth/last.log)
#   SYAUTH_PAM_SERVICE       PAM service (default syauth-test)
#   SYAUTH_PAM_USER          PAM user (default $USER)
#   SYAUTH_PAMTESTER_BIN     pamtester binary path (default `pamtester` on PATH)
#   SYAUTH_E2E_VERBOSE       set to 1 for `set -x` shell-trace
#   SYAUTH_E2E_PREPOPULATED  test-only: skip the loop and percentile the
#                            audit log's tail of ITERATIONS lines as-is
#
# Exit codes:
#   0 — gate green (JSON on stdout)
#   1 — gate red  (JSON on stdout, distinguishable by contents)
#   2 — pre-flight failure (stderr message, no JSON)

set -euo pipefail

if [[ "${SYAUTH_E2E_VERBOSE:-0}" == "1" ]]; then
    set -x
fi

# -----------------------------------------------------------------------------
# Named constants (SPEC §4.3 anchored)
# -----------------------------------------------------------------------------

readonly ITERATIONS="${SYAUTH_E2E_ITERATIONS:-100}"
readonly P50_BUDGET_MS=1500
readonly P99_BUDGET_MS=2000
readonly AUDIT_LOG_PATH="${SYAUTH_AUDIT_LOG:-/var/lib/syauth/last.log}"
readonly PAM_SERVICE="${SYAUTH_PAM_SERVICE:-syauth-test}"
readonly PAM_USER="${SYAUTH_PAM_USER:-${USER:-}}"
readonly PAMTESTER_BIN="${SYAUTH_PAMTESTER_BIN:-pamtester}"
readonly PREPOPULATED_MODE="${SYAUTH_E2E_PREPOPULATED:-0}"

# Exit codes.
readonly EX_OK=0
readonly EX_GATE_FAIL=1
readonly EX_PREFLIGHT=2

# Audit-column field separator (SPEC §3 #8).
readonly AUDIT_FIELD_SEPARATOR=","
# Audit column indices (1-based, as used by awk -F,). Layout:
#   1=peer_id 2=nonce_hex 3=t_start_ms 4=t_end_ms 5=outcome 6=reason
# Column 5 (outcome) is documented for grep-ability but the gate
# math does not consume it directly — the reason column (6) is the
# fine-grained tag we count (e.g., `response-timeout`).
readonly AUDIT_COL_T_START=3
readonly AUDIT_COL_T_END=4
readonly AUDIT_COL_REASON=6
# Audit reason string used for response-timeout outcomes
# (orchestrator.rs typed constant). Counted in `n_timeouts`.
readonly AUDIT_REASON_RESPONSE_TIMEOUT="response-timeout"

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------

log_stderr() {
    printf 'e2e-unlock: %s\n' "$*" >&2
}

fail_preflight() {
    log_stderr "preflight failed: $*"
    exit "$EX_PREFLIGHT"
}

# Resolve the pamtester binary into an absolute path or fail-fast.
resolve_pamtester() {
    if [[ "$PAMTESTER_BIN" = /* ]]; then
        if [[ -x "$PAMTESTER_BIN" ]]; then
            printf '%s' "$PAMTESTER_BIN"
            return 0
        fi
        fail_preflight "pamtester binary '$PAMTESTER_BIN' not executable (install pamtester or override SYAUTH_PAMTESTER_BIN)"
    fi
    local resolved
    if resolved="$(command -v "$PAMTESTER_BIN" 2>/dev/null)"; then
        printf '%s' "$resolved"
        return 0
    fi
    fail_preflight "pamtester binary '$PAMTESTER_BIN' not on PATH (install pamtester or set SYAUTH_PAMTESTER_BIN)"
}

# Nearest-rank percentile: given a sorted-ascending array passed via
# stdin (one integer per line) and a percentile (1..100), prints the
# value at rank `ceil(p/100 * n) - 1` (0-indexed).
percentile_from_sorted() {
    local p="$1"
    awk -v p="$p" '
        { v[NR] = $1 }
        END {
            if (NR == 0) { print 0; exit }
            rank = int((p / 100.0) * NR + 0.9999999)
            if (rank < 1) rank = 1
            if (rank > NR) rank = NR
            print v[rank]
        }
    '
}

# -----------------------------------------------------------------------------
# Pre-flight
# -----------------------------------------------------------------------------

if [[ "${SYAUTH_REAL_RADIOS:-0}" != "1" ]]; then
    fail_preflight "SYAUTH_REAL_RADIOS=1 not set (refusing to run benchmark without real radios)"
fi

if [[ -z "$PAM_USER" ]]; then
    fail_preflight "PAM_USER is empty (set USER or SYAUTH_PAM_USER)"
fi

if (( ITERATIONS < 1 )); then
    fail_preflight "ITERATIONS must be >= 1 (got $ITERATIONS)"
fi

if [[ ! -f "$AUDIT_LOG_PATH" ]]; then
    fail_preflight "audit log not found at '$AUDIT_LOG_PATH' (set SYAUTH_AUDIT_LOG or start syauth-presenced so it creates the file)"
fi

if [[ ! -r "$AUDIT_LOG_PATH" ]]; then
    fail_preflight "audit log '$AUDIT_LOG_PATH' not readable by $(id -un) (chgrp it to your user or set SYAUTH_AUDIT_LOG to a user-readable path)"
fi

PAMTESTER_RESOLVED=""
if [[ "$PREPOPULATED_MODE" != "1" ]]; then
    PAMTESTER_RESOLVED="$(resolve_pamtester)"
    log_stderr "using pamtester: $PAMTESTER_RESOLVED"
fi

log_stderr "iterations=$ITERATIONS p50_budget_ms=$P50_BUDGET_MS p99_budget_ms=$P99_BUDGET_MS"
log_stderr "audit_log=$AUDIT_LOG_PATH pam_service=$PAM_SERVICE pam_user=$PAM_USER"

# -----------------------------------------------------------------------------
# Drive pamtester and snapshot the audit log
# -----------------------------------------------------------------------------

N_FAILURES=0

START_LINES="$(wc -l < "$AUDIT_LOG_PATH" | awk '{print $1}')"

if [[ "$PREPOPULATED_MODE" == "1" ]]; then
    log_stderr "PREPOPULATED_MODE=1 — skipping pamtester loop, parsing existing audit log tail"
    END_LINES="$START_LINES"
    # Reframe START_LINES so we always read the last ITERATIONS lines.
    if (( END_LINES >= ITERATIONS )); then
        START_LINES=$((END_LINES - ITERATIONS))
    else
        START_LINES=0
    fi
else
    for (( i = 1; i <= ITERATIONS; i++ )); do
        if ! "$PAMTESTER_RESOLVED" "$PAM_SERVICE" "$PAM_USER" authenticate >/dev/null 2>&1; then
            rc=$?
            N_FAILURES=$((N_FAILURES + 1))
            log_stderr "[$i/$ITERATIONS] pamtester rc=$rc (failure $N_FAILURES so far)"
        else
            log_stderr "[$i/$ITERATIONS] pamtester rc=0"
        fi
    done
    END_LINES="$(wc -l < "$AUDIT_LOG_PATH" | awk '{print $1}')"
fi

NEW_LINES=$((END_LINES - START_LINES))
log_stderr "audit lines: start=$START_LINES end=$END_LINES new=$NEW_LINES"

# -----------------------------------------------------------------------------
# Parse the new audit lines and compute percentiles
# -----------------------------------------------------------------------------

ELAPSED_TMP="$(mktemp)"
trap 'rm -f "$ELAPSED_TMP"' EXIT

N_TIMEOUTS=0

if (( NEW_LINES > 0 )); then
    # Take exactly the new audit lines (`tail` is portable; `awk NR>=`
    # also works but `tail -n +K` reads from line K onward).
    # The daemon emits CSV (6 fields). The PAM module appends a
    # human-readable ISO-timestamp line to the same file on each
    # auth result. Skip those by requiring exactly 6 comma-separated
    # fields and a numeric t_end column.
    tail -n "+$((START_LINES + 1))" "$AUDIT_LOG_PATH" | head -n "$NEW_LINES" \
        | awk \
            -F"$AUDIT_FIELD_SEPARATOR" \
            -v ts_col="$AUDIT_COL_T_START" \
            -v te_col="$AUDIT_COL_T_END" \
            'NF == 6 && $te_col ~ /^[0-9]+$/ {
                ts = $ts_col + 0
                te = $te_col + 0
                d = te - ts
                if (d < 0) { d = 0 }
                print d
            }' > "$ELAPSED_TMP"
    # Count rows whose REASON column equals the response-timeout tag.
    N_TIMEOUTS="$(awk \
        -F"$AUDIT_FIELD_SEPARATOR" \
        -v reason_col="$AUDIT_COL_REASON" \
        -v tag="$AUDIT_REASON_RESPONSE_TIMEOUT" \
        'NF == 6 && $reason_col == tag { n++ } END { print n + 0 }' \
        <(tail -n "+$((START_LINES + 1))" "$AUDIT_LOG_PATH" | head -n "$NEW_LINES"))"
fi

# Sort ascending for nearest-rank.
SORTED_TMP="$(mktemp)"
trap 'rm -f "$ELAPSED_TMP" "$SORTED_TMP"' EXIT

if [[ -s "$ELAPSED_TMP" ]]; then
    sort -n "$ELAPSED_TMP" > "$SORTED_TMP"
else
    : > "$SORTED_TMP"
fi

N_SAMPLES="$(wc -l < "$SORTED_TMP" | awk '{print $1}')"

if (( N_SAMPLES > 0 )); then
    P50_MS="$(percentile_from_sorted 50 < "$SORTED_TMP")"
    P95_MS="$(percentile_from_sorted 95 < "$SORTED_TMP")"
    P99_MS="$(percentile_from_sorted 99 < "$SORTED_TMP")"
else
    P50_MS=0
    P95_MS=0
    P99_MS=0
fi

# -----------------------------------------------------------------------------
# Emit JSON summary (exactly one line, machine-readable)
# -----------------------------------------------------------------------------

printf '{"p50_ms":%d,"p95_ms":%d,"p99_ms":%d,"n_failures":%d,"n_timeouts":%d}\n' \
    "$P50_MS" "$P95_MS" "$P99_MS" "$N_FAILURES" "$N_TIMEOUTS"

# -----------------------------------------------------------------------------
# Gate evaluation
# -----------------------------------------------------------------------------

GATE_RC="$EX_OK"
GATE_REASONS=()

if (( P50_MS > P50_BUDGET_MS )); then
    GATE_RC="$EX_GATE_FAIL"
    GATE_REASONS+=("p50_ms=$P50_MS exceeds budget $P50_BUDGET_MS")
fi
if (( P99_MS > P99_BUDGET_MS )); then
    GATE_RC="$EX_GATE_FAIL"
    GATE_REASONS+=("p99_ms=$P99_MS exceeds budget $P99_BUDGET_MS")
fi
if (( N_FAILURES > 0 )); then
    GATE_RC="$EX_GATE_FAIL"
    GATE_REASONS+=("n_failures=$N_FAILURES > 0")
fi

if [[ "$GATE_RC" != "$EX_OK" ]]; then
    log_stderr "gate FAILED:"
    for r in "${GATE_REASONS[@]}"; do
        log_stderr "  - $r"
    done
    if (( N_SAMPLES > 0 )); then
        log_stderr "elapsed_ms head (10):"
        head -n 10 "$SORTED_TMP" | awk '{print "  ", $1}' >&2
        log_stderr "elapsed_ms tail (10):"
        tail -n 10 "$SORTED_TMP" | awk '{print "  ", $1}' >&2
    fi
else
    log_stderr "gate PASSED (p50_ms=$P50_MS p95_ms=$P95_MS p99_ms=$P99_MS n_failures=0)"
fi

exit "$GATE_RC"

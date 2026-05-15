#!/usr/bin/env bash
# scripts/e2e-emulator-down.sh — companion teardown for e2e-emulator-up.sh.
#
# Roadmap: specs/syauth/ROADMAP.md item S-019.
# Journey: specs/journeys/JOURNEY-S-019-e2e-real-radios.md §Phase 4.
#
# Idempotent. Safe to re-run by hand. Exit 0 even when nothing is running
# so a Make target that chains "up; test; down" stays unambiguous about
# the failure source: only test-suite failures and up-script failures
# break the chain.

set -euo pipefail

# -----------------------------------------------------------------------------
# Named constants
# -----------------------------------------------------------------------------
readonly AVD_NAME="syauth_e2e"
readonly ENV_FILE_NAME=".env.e2e"
readonly EMULATOR_PID_FILE="/tmp/syauth-e2e-emulator.pid"
readonly LOGCAT_PID_FILE="/tmp/syauth-e2e-logcat.pid"

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
log() {
    local ts
    ts="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
    printf '[%s] e2e-emulator-down: %s\n' "$ts" "$*" >&2
}

# -----------------------------------------------------------------------------
# Resolve repo root
# -----------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT
cd "$REPO_ROOT"

log "starting; repo root=$REPO_ROOT"

# -----------------------------------------------------------------------------
# Kill the logcat tail (best effort)
# -----------------------------------------------------------------------------
if [ -f "$LOGCAT_PID_FILE" ]; then
    LOGCAT_PID="$(cat "$LOGCAT_PID_FILE" 2>/dev/null || true)"
    if [ -n "$LOGCAT_PID" ]; then
        kill "$LOGCAT_PID" 2>/dev/null || true
        log "killed logcat pid=$LOGCAT_PID"
    fi
    rm -f "$LOGCAT_PID_FILE"
fi

# -----------------------------------------------------------------------------
# Kill the emulator (best effort)
# -----------------------------------------------------------------------------
if command -v adb >/dev/null 2>&1; then
    adb emu kill 2>/dev/null || true
    log "issued adb emu kill"
fi
if [ -f "$EMULATOR_PID_FILE" ]; then
    EMULATOR_PID="$(cat "$EMULATOR_PID_FILE" 2>/dev/null || true)"
    if [ -n "$EMULATOR_PID" ]; then
        kill "$EMULATOR_PID" 2>/dev/null || true
        log "killed emulator pid=$EMULATOR_PID"
    fi
    rm -f "$EMULATOR_PID_FILE"
fi

# -----------------------------------------------------------------------------
# Clear AVD lockfiles so the next up-run can reuse the AVD
# -----------------------------------------------------------------------------
if [ -n "${HOME:-}" ] && [ -d "$HOME/.android/avd/${AVD_NAME}.avd" ]; then
    find "$HOME/.android/avd/${AVD_NAME}.avd" -maxdepth 1 -name '*.lock' -type d -exec rm -rf {} + 2>/dev/null || true
    find "$HOME/.android/avd/${AVD_NAME}.avd" -maxdepth 1 -name '*.lock' -type f -exec rm -f {} + 2>/dev/null || true
    log "cleared AVD lockfiles for $AVD_NAME"
fi

# -----------------------------------------------------------------------------
# Remove .env.e2e
# -----------------------------------------------------------------------------
ENV_FILE="$REPO_ROOT/$ENV_FILE_NAME"
if [ -f "$ENV_FILE" ]; then
    rm -f "$ENV_FILE"
    log "removed $ENV_FILE"
fi

log "done"
exit 0

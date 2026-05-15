#!/usr/bin/env bash
# scripts/e2e-emulator-up.sh — boot the e2e Android emulator, install the
# syauth APK, run a scripted pair, and write .env.e2e at the repo root.
#
# Roadmap: specs/syauth/ROADMAP.md item S-019.
# Journey: specs/journeys/JOURNEY-S-019-e2e-real-radios.md §Phase 1.
#
# Usage:
#   ./scripts/e2e-emulator-up.sh
#
# Effect on success:
#   .env.e2e at the repo root contains:
#       SYAUTH_E2E_PEER_BOND_ID=<hex peer id>
#   The emulator is left running for the test suite. Tear it down with
#   scripts/e2e-emulator-down.sh.
#
# Exit code 0 only on full success. Every preflight failure exits non-zero
# with an actionable message on stderr.

set -euo pipefail

# -----------------------------------------------------------------------------
# Named constants
# -----------------------------------------------------------------------------
readonly AVD_NAME="syauth_e2e"
readonly APK_REL_PATH="syauth-android/app/build/outputs/apk/debug/app-debug.apk"
readonly OOB_LOGCAT_TAG="syauth-pair-oob"
readonly ENV_FILE_NAME=".env.e2e"
readonly ADB_BOOT_TIMEOUT_SECS="180"
readonly OOB_WAIT_TIMEOUT_SECS="60"
readonly PEER_BOND_ID_VAR="SYAUTH_E2E_PEER_BOND_ID"
readonly DEFAULT_ADAPTER="hci0"

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
log() {
    local ts
    ts="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
    printf '[%s] e2e-emulator-up: %s\n' "$ts" "$*" >&2
}

die() {
    log "ERROR: $*"
    exit 1
}

require_cmd() {
    local cmd="$1"
    local hint="${2:-}"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        if [ -n "$hint" ]; then
            die "$cmd not found on PATH; $hint"
        else
            die "$cmd not found on PATH"
        fi
    fi
}

# -----------------------------------------------------------------------------
# Resolve repo root
# -----------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
readonly SCRIPT_DIR REPO_ROOT
cd "$REPO_ROOT"

# -----------------------------------------------------------------------------
# Preflight
# -----------------------------------------------------------------------------
log "starting; repo root=$REPO_ROOT"

require_cmd adb "install Android SDK platform-tools and add platform-tools to PATH"
require_cmd emulator "install Android Studio command-line tools and add \$ANDROID_HOME/emulator to PATH"
require_cmd cargo "install rustup (https://rustup.rs)"

# -----------------------------------------------------------------------------
# APK check
# -----------------------------------------------------------------------------
APK_PATH="$REPO_ROOT/$APK_REL_PATH"
if [ ! -f "$APK_PATH" ]; then
    die "APK not found at $APK_PATH; run: ( cd syauth-android && ./gradlew :app:assembleDebug )"
fi
log "APK present at $APK_PATH"

# -----------------------------------------------------------------------------
# AVD check
# -----------------------------------------------------------------------------
if ! emulator -list-avds 2>/dev/null | grep -Fxq "$AVD_NAME"; then
    die "AVD '$AVD_NAME' not found; run: avdmanager create avd -n $AVD_NAME -k 'system-images;android-34;default;x86_64'"
fi
log "AVD '$AVD_NAME' present"

# -----------------------------------------------------------------------------
# Boot emulator headless in the background
# -----------------------------------------------------------------------------
log "starting emulator (headless)"
emulator -avd "$AVD_NAME" -no-window -no-audio -no-boot-anim -no-snapshot >/tmp/syauth-e2e-emulator.log 2>&1 &
EMULATOR_PID="$!"
echo "$EMULATOR_PID" > /tmp/syauth-e2e-emulator.pid
log "emulator started pid=$EMULATOR_PID; waiting for adb (timeout ${ADB_BOOT_TIMEOUT_SECS}s)"

# Wait for the emulator to register with adb.
timeout "${ADB_BOOT_TIMEOUT_SECS}" adb wait-for-device || die "adb wait-for-device timed out after ${ADB_BOOT_TIMEOUT_SECS}s"

# Then wait for sys.boot_completed.
boot_completed=""
SECONDS=0
while [ "$boot_completed" != "1" ] && [ "$SECONDS" -lt "$ADB_BOOT_TIMEOUT_SECS" ]; do
    boot_completed="$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')"
    sleep 2
done
if [ "$boot_completed" != "1" ]; then
    die "emulator did not finish booting within ${ADB_BOOT_TIMEOUT_SECS}s"
fi
log "emulator boot completed"

# -----------------------------------------------------------------------------
# Install the APK
# -----------------------------------------------------------------------------
log "installing APK"
adb install -r "$APK_PATH" >/dev/null 2>&1 || die "adb install failed; check /tmp/syauth-e2e-emulator.log"
log "APK installed"

# -----------------------------------------------------------------------------
# Capture the OOB code from logcat
#
# The Android app prints the OOB hex on the documented tag once the
# pairing screen lands. We tail logcat for at most $OOB_WAIT_TIMEOUT_SECS.
# -----------------------------------------------------------------------------
log "waiting for OOB code from logcat tag '$OOB_LOGCAT_TAG' (timeout ${OOB_WAIT_TIMEOUT_SECS}s)"
adb logcat -c
OOB_HEX=""
LOGCAT_LOG="/tmp/syauth-e2e-logcat.log"
adb logcat -s "$OOB_LOGCAT_TAG":I > "$LOGCAT_LOG" &
LOGCAT_PID="$!"
echo "$LOGCAT_PID" > /tmp/syauth-e2e-logcat.pid

start_ts="$SECONDS"
while [ -z "$OOB_HEX" ] && [ "$((SECONDS - start_ts))" -lt "$OOB_WAIT_TIMEOUT_SECS" ]; do
    # Match the conventional logcat row: "I/syauth-pair-oob(  1234): <hex>"
    OOB_HEX="$(grep -Eo '[0-9a-f]{8,64}' "$LOGCAT_LOG" 2>/dev/null | head -n1 || true)"
    if [ -z "$OOB_HEX" ]; then
        sleep 2
    fi
done

kill "$LOGCAT_PID" 2>/dev/null || true

if [ -z "$OOB_HEX" ]; then
    log "logcat dump (last 200 lines):"
    tail -n 200 "$LOGCAT_LOG" >&2 || true
    die "no OOB code seen on logcat tag '$OOB_LOGCAT_TAG' within ${OOB_WAIT_TIMEOUT_SECS}s"
fi
log "captured OOB hex (${#OOB_HEX} chars)"

# -----------------------------------------------------------------------------
# Drive the scripted pair via syauth-cli
#
# `--scripted-oob <hex>` bypasses the interactive y/N prompt and consumes
# the OOB code from the argument. The flag is gated on a stderr warning so
# a production operator cannot accidentally use it.
# -----------------------------------------------------------------------------
log "running scripted syauth pair against adapter $DEFAULT_ADAPTER"
PAIR_LOG="/tmp/syauth-e2e-pair.log"
if ! cargo run --quiet -p syauth-cli -- pair \
        --adapter "$DEFAULT_ADAPTER" \
        --yes \
        --scripted-oob "$OOB_HEX" \
        >"$PAIR_LOG" 2>&1; then
    log "pair log:"
    cat "$PAIR_LOG" >&2 || true
    die "syauth pair failed"
fi
log "pair complete"

# -----------------------------------------------------------------------------
# Extract the bonded peer id from the pair output. The CLI prints a
# trailing line `bonded <name> id=<peer_id>; run \`syauth list\` to verify`.
# -----------------------------------------------------------------------------
PEER_BOND_ID="$(grep -Eo 'id=[0-9a-f]+' "$PAIR_LOG" | head -n1 | cut -d= -f2 || true)"
if [ -z "$PEER_BOND_ID" ]; then
    log "pair log:"
    cat "$PAIR_LOG" >&2 || true
    die "could not extract peer bond id from syauth pair output"
fi
log "bonded peer id: $PEER_BOND_ID"

# -----------------------------------------------------------------------------
# Write .env.e2e at the repo root
# -----------------------------------------------------------------------------
ENV_FILE="$REPO_ROOT/$ENV_FILE_NAME"
{
    echo "# Generated by scripts/e2e-emulator-up.sh"
    echo "# DO NOT COMMIT. .env.e2e holds the bond id for the current emulator run."
    echo "$PEER_BOND_ID_VAR=$PEER_BOND_ID"
} > "$ENV_FILE"
chmod 0600 "$ENV_FILE" || true
log "wrote $ENV_FILE"

log "done; ready to run: cargo test -p syauth --test e2e_real"
exit 0

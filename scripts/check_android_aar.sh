#!/usr/bin/env bash
# Roadmap item S-015 — preflight check for the syauth_mobile.aar artifact.
#
# Returns 0 if the AAR exists at the documented path, otherwise prints a
# one-line skip message and returns a non-zero exit code so the caller
# (the `make android-test` target) can short-circuit to a clean skip.
#
# Mirrors the pattern in `scripts/build_aar.sh` where DRY_RUN=1 prints a
# build plan and exits 0 on hosts without the NDK; here the operational
# need is the opposite (do NOT build, just check), so we keep the script
# tiny and free of side effects.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
AAR_PATH="${PROJECT_ROOT}/crates/syauth-mobile/target/syauth_mobile.aar"

if [ -f "${AAR_PATH}" ]; then
    echo "==> syauth_mobile.aar present at ${AAR_PATH}"
    exit 0
fi

echo "==> syauth_mobile.aar not built — run 'make android-aar' on an NDK host first"
exit 1

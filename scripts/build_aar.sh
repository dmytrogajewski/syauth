#!/usr/bin/env bash
# syauth — Build the syauth_mobile AAR for Android.
#
# Mirrors `~/sources/prrr/scripts/build-android.sh` for the syauth project.
# Roadmap item S-014. See specs/journeys/JOURNEY-S-014-mobile-uniffi-surface.md.
#
# Usage:
#   NDK_HOME=/path/to/android-ndk ./scripts/build_aar.sh        # build release AAR
#   DEBUG=1 NDK_HOME=...           ./scripts/build_aar.sh       # debug profile
#   DRY_RUN=1                      ./scripts/build_aar.sh       # check toolchain only
#
# Outputs:
#   crates/syauth-mobile/target/syauth_mobile.aar
#   crates/syauth-mobile/bindings/kotlin/uniffi/syauth_mobile/syauth_mobile.kt

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MOBILE_DIR="${PROJECT_ROOT}/crates/syauth-mobile"
OUTPUT_DIR="${MOBILE_DIR}/target"
BINDINGS_DIR="${MOBILE_DIR}/bindings/kotlin"
UDL_FILE="${MOBILE_DIR}/src/mobile.udl"

PROFILE="${DEBUG:+debug}"
PROFILE="${PROFILE:-release}"
PROFILE_FLAG=""
if [ "${PROFILE}" = "release" ]; then
    PROFILE_FLAG="--release"
fi

DRY_RUN="${DRY_RUN:-0}"

# Android targets and their JNI architecture names. The two required-for-DoD
# targets are aarch64-linux-android (arm64-v8a) and armv7-linux-androideabi
# (armeabi-v7a); the x86 variants are useful for emulator testing.
declare -A ANDROID_TARGETS=(
    ["aarch64-linux-android"]="arm64-v8a"
    ["armv7-linux-androideabi"]="armeabi-v7a"
    ["x86_64-linux-android"]="x86_64"
    ["i686-linux-android"]="x86"
)

LIB_NAME="libsyauth_mobile.so"
AAR_NAME="syauth_mobile.aar"

echo "==> Building syauth-mobile AAR (${PROFILE} profile)"

# ---------------------------------------------------------------------------
# Preflight checks.
# ---------------------------------------------------------------------------

if [ ! -f "${UDL_FILE}" ]; then
    echo "Error: UDL file not found: ${UDL_FILE}" >&2
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo "Error: cargo not found in PATH" >&2
    exit 1
fi

if [ "${DRY_RUN}" = "1" ]; then
    echo "==> DRY_RUN=1: skipping NDK / cargo-ndk / uniffi-bindgen checks"
    echo "    Would build the following targets and package:"
    for target in "${!ANDROID_TARGETS[@]}"; do
        echo "      - ${target} (${ANDROID_TARGETS[$target]}) -> ${LIB_NAME}"
    done
    echo "    Would generate Kotlin bindings:"
    echo "      uniffi-bindgen generate ${UDL_FILE} --language kotlin --out-dir ${BINDINGS_DIR}"
    echo "    Would package:"
    echo "      ${OUTPUT_DIR}/${AAR_NAME}"
    echo "==> DRY_RUN complete (no artifacts produced)"
    exit 0
fi

if [ -z "${NDK_HOME:-}" ]; then
    echo "Error: NDK_HOME environment variable not set" >&2
    echo "" >&2
    echo "Install the Android NDK and set NDK_HOME, e.g.:" >&2
    echo "  export NDK_HOME=\${HOME}/Android/Sdk/ndk/26.1.10909125" >&2
    echo "" >&2
    echo "Download from: https://developer.android.com/ndk/downloads" >&2
    echo "Or use DRY_RUN=1 to verify the build pipeline without the NDK." >&2
    exit 1
fi

if [ ! -d "${NDK_HOME}" ]; then
    echo "Error: NDK_HOME directory does not exist: ${NDK_HOME}" >&2
    exit 1
fi

if ! command -v cargo-ndk &> /dev/null; then
    echo "Error: cargo-ndk not found" >&2
    echo "Install with: cargo install cargo-ndk" >&2
    exit 1
fi

if ! command -v uniffi-bindgen &> /dev/null; then
    echo "Error: uniffi-bindgen not found" >&2
    echo "Install with: cargo install uniffi-bindgen --version 0.29" >&2
    exit 1
fi

# Verify rust Android targets are installed.
for target in "${!ANDROID_TARGETS[@]}"; do
    if ! rustup target list --installed | grep -q "^${target}$"; then
        echo "Error: rust target ${target} not installed" >&2
        echo "Install with: rustup target add ${target}" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# Build per-target .so files.
# ---------------------------------------------------------------------------

echo "==> Building per-target shared libraries"
for target in "${!ANDROID_TARGETS[@]}"; do
    echo "  -> ${target}"
    (
        cd "${PROJECT_ROOT}"
        cargo ndk -t "${target}" build -p syauth-mobile ${PROFILE_FLAG}
    )
done

# ---------------------------------------------------------------------------
# Generate Kotlin bindings via uniffi-bindgen.
# ---------------------------------------------------------------------------

echo "==> Generating Kotlin bindings"
mkdir -p "${BINDINGS_DIR}"
(
    cd "${MOBILE_DIR}"
    uniffi-bindgen generate "${UDL_FILE}" --language kotlin --out-dir "${BINDINGS_DIR}"
)

# ---------------------------------------------------------------------------
# Package the AAR.
# ---------------------------------------------------------------------------

echo "==> Packaging AAR"
AAR_BUILD_DIR="${OUTPUT_DIR}/aar-build"
rm -rf "${AAR_BUILD_DIR}"
mkdir -p "${AAR_BUILD_DIR}"

# Copy native libraries into AAR jni/<abi>/ directory.
for target in "${!ANDROID_TARGETS[@]}"; do
    jni_arch="${ANDROID_TARGETS[$target]}"
    jni_dir="${AAR_BUILD_DIR}/jni/${jni_arch}"
    mkdir -p "${jni_dir}"
    src_so="${PROJECT_ROOT}/target/${target}/${PROFILE}/${LIB_NAME}"
    if [ ! -f "${src_so}" ]; then
        echo "Error: expected build output not found: ${src_so}" >&2
        exit 1
    fi
    cp "${src_so}" "${jni_dir}/"
done

# AndroidManifest.xml — minimal manifest for a native-only library.
cat > "${AAR_BUILD_DIR}/AndroidManifest.xml" << 'EOF'
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="com.sy.syauth.mobile">
    <uses-sdk android:minSdkVersion="26" />
</manifest>
EOF

# classes.jar: empty jar (this library has no Java/Kotlin classes —
# the Kotlin bindings live in `bindings/kotlin/` and the Android app
# brings them in as source).
mkdir -p "${AAR_BUILD_DIR}/classes"
(cd "${AAR_BUILD_DIR}/classes" && jar cf ../classes.jar . 2>/dev/null || zip -q ../classes.jar .)
rm -rf "${AAR_BUILD_DIR}/classes"

# R.txt: empty (no Android resources in this AAR).
touch "${AAR_BUILD_DIR}/R.txt"

# Package as AAR (just a zip with a specific layout).
AAR_PATH="${OUTPUT_DIR}/${AAR_NAME}"
rm -f "${AAR_PATH}"
(cd "${AAR_BUILD_DIR}" && zip -q -r "${AAR_PATH}" .)
rm -rf "${AAR_BUILD_DIR}"

echo "==> Build complete"
echo "    AAR:      ${AAR_PATH}"
echo "    Bindings: ${BINDINGS_DIR}"
echo ""
echo "To consume from Android Studio:"
echo "  1. Copy ${AAR_NAME} into syauth-android/app/libs/"
echo "  2. In app/build.gradle.kts: implementation(files(\"libs/${AAR_NAME}\"))"
echo "  3. Copy the generated Kotlin file(s) from ${BINDINGS_DIR} into"
echo "     syauth-android/app/src/main/kotlin/uniffi/syauth_mobile/."

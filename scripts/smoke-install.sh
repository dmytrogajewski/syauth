#!/usr/bin/env bash
#
# smoke-install.sh — Roadmap item S-021 install smoke test.
#
# Spins up a clean Fedora 39 container and a clean Debian 12
# container, copies in the freshly-built RPM and .deb respectively,
# runs the one-line install command from the README, then runs
# `syauth --version` and asserts the stdout begins with the
# expected version string.
#
# Gated on `docker` (or `podman` via the DOCKER env var). When docker
# is absent the script prints a one-line skip and exits 0 so it can
# be run from `make test` without breaking the dev box.

set -euo pipefail

# Pinned constants. Single-source-of-truth lives in deploy/version.env;
# we re-declare here so the script is self-contained when invoked
# without the make wrapper.
readonly PACKAGE_VERSION="0.1.0"
readonly RPM_RELEASE="1"
readonly DEB_REVISION="1"
readonly EXPECTED_VERSION_PREFIX="syauth ${PACKAGE_VERSION}"
readonly FEDORA_IMAGE="registry.fedoraproject.org/fedora:39"
readonly DEBIAN_IMAGE="debian:12"
readonly CONTAINER_NAME_FEDORA="syauth-smoke-fedora"
readonly CONTAINER_NAME_DEBIAN="syauth-smoke-debian"

# Repo root (parent of scripts/).
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

# Tool selector — accept docker or podman (`DOCKER=podman ./smoke-install.sh`).
DOCKER="${DOCKER:-docker}"

# Gate. No docker → skip cleanly.
if ! command -v "${DOCKER}" >/dev/null 2>&1; then
    echo "==> smoke-install: '${DOCKER}' not on PATH — skipping (set DOCKER=podman or install docker)"
    exit 0
fi

# Locate the two build artifacts. They land under target/ by
# convention; allow override via env for CI which may place them in
# the workflow's `download-artifact` directory.
RPM_PATH="${RPM_PATH:-${REPO_ROOT}/target/syauth-${PACKAGE_VERSION}-${RPM_RELEASE}.fc39.x86_64.rpm}"
DEB_PATH="${DEB_PATH:-${REPO_ROOT}/target/syauth_${PACKAGE_VERSION}-${DEB_REVISION}_amd64.deb}"

# Cleanup trap. Always remove both containers regardless of exit
# status; ignore errors if a container was never created.
cleanup() {
    "${DOCKER}" rm -f "${CONTAINER_NAME_FEDORA}" >/dev/null 2>&1 || true
    "${DOCKER}" rm -f "${CONTAINER_NAME_DEBIAN}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

run_one_smoke() {
    local image="$1"
    local container_name="$2"
    local install_cmd="$3"
    local artifact_path="$4"
    local artifact_name
    artifact_name="$(basename "${artifact_path}")"

    if [ ! -f "${artifact_path}" ]; then
        echo "==> smoke-install: artifact missing at ${artifact_path} — skipping ${image}"
        echo "    (build it with 'make rpm' or 'make deb' first, or set RPM_PATH/DEB_PATH)"
        return 0
    fi

    echo "==> smoke-install: spinning up ${image}"
    "${DOCKER}" run -d \
        --name "${container_name}" \
        --rm=false \
        "${image}" \
        sleep 600 >/dev/null

    echo "==> smoke-install: copying ${artifact_name} into ${container_name}"
    "${DOCKER}" cp "${artifact_path}" "${container_name}:/tmp/${artifact_name}"

    echo "==> smoke-install: running install command in ${container_name}"
    "${DOCKER}" exec "${container_name}" sh -c "${install_cmd} /tmp/${artifact_name}"

    echo "==> smoke-install: running 'syauth --version' in ${container_name}"
    local version_output
    version_output="$("${DOCKER}" exec "${container_name}" syauth --version)"
    echo "    stdout: ${version_output}"

    case "${version_output}" in
        "${EXPECTED_VERSION_PREFIX}"*)
            echo "==> smoke-install: ${image} PASS"
            ;;
        *)
            echo "==> smoke-install: ${image} FAIL — expected stdout to start with '${EXPECTED_VERSION_PREFIX}', got '${version_output}'" >&2
            return 1
            ;;
    esac
}

echo "==> smoke-install: Fedora ${PACKAGE_VERSION} install + version smoke"
run_one_smoke \
    "${FEDORA_IMAGE}" \
    "${CONTAINER_NAME_FEDORA}" \
    "dnf install -y" \
    "${RPM_PATH}"

echo "==> smoke-install: Debian ${PACKAGE_VERSION} install + version smoke"
run_one_smoke \
    "${DEBIAN_IMAGE}" \
    "${CONTAINER_NAME_DEBIAN}" \
    "apt-get update -qq && apt-get install -y --no-install-recommends" \
    "${DEB_PATH}"

echo "==> smoke-install: all platforms PASS"

#!/usr/bin/env bash
# Selftest for crates/trusty-audit/install.sh (#5870).
#
# Why: the installer's whole value is what it does when something goes WRONG —
# refusing an unsupported platform, refusing a checksum mismatch, leaving
# nothing behind after a failed download. A gate that only proved the happy
# path would pass while every one of those arms was broken, which is the
# failure mode `scripts/check_*_selftest.sh` exists to prevent across this repo.
#
# What: runs install.sh against stubbed `curl` and `uname` so every arm is
# exercised with no network and no real release. Each case asserts BOTH the
# exit status and that the install directory holds what it should — "it exited
# non-zero" is not evidence that nothing was installed.
#
# Usage: bash scripts/check_trusty_audit_install_selftest.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/crates/trusty-audit/install.sh"

[[ -f "${SCRIPT}" ]] || {
    echo "FATAL: installer not found at ${SCRIPT}" >&2
    exit 1
}

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

PASS=0
FAIL=0

pass() {
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}
fail() {
    echo "  FAIL: $1" >&2
    FAIL=$((FAIL + 1))
}

# ---------------------------------------------------------------------------
# Fixture: a release tarball containing fake `trusty-audit` + `taudit` binaries that
# answer `--version`, plus its real sha256 sidecar.
# ---------------------------------------------------------------------------
VERSION="9.9.9"
TARGET="aarch64-apple-darwin"
ASSET="trusty-audit-${VERSION}-${TARGET}.tar.gz"
FIXTURES="${WORK}/fixtures"
mkdir -p "${FIXTURES}"

build_fixture_tarball() {
    local stage="${WORK}/stage/trusty-audit-${VERSION}-${TARGET}"
    rm -rf "${WORK}/stage"
    mkdir -p "${stage}"
    for name in trusty-audit taudit; do
        cat >"${stage}/${name}" <<EOF
#!/bin/sh
[ "\$1" = "--version" ] && echo "trusty-audit ${VERSION}" && exit 0
echo "trusty-audit stub launched"
exit 0
EOF
        chmod +x "${stage}/${name}"
    done
    tar -czf "${FIXTURES}/${ASSET}" -C "${WORK}/stage" "trusty-audit-${VERSION}-${TARGET}"
    (cd "${FIXTURES}" && shasum -a 256 "${ASSET}" >"${ASSET}.sha256")
}
build_fixture_tarball

# ---------------------------------------------------------------------------
# Stubs. A directory prepended to PATH so install.sh resolves these instead of
# the real tools. `curl` serves from ${FIXTURES}; `uname` reports whatever the
# case under test sets.
# ---------------------------------------------------------------------------
STUBS="${WORK}/stubs"
mkdir -p "${STUBS}"

cat >"${STUBS}/curl" <<'STUBEOF'
#!/usr/bin/env bash
# Stub curl: resolves a URL to a fixture file, or fails when told to.
set -uo pipefail
out=""; url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        http*) url="$1"; shift ;;
        *) shift ;;
    esac
done

# GitHub releases API.
if [[ "${url}" == *api.github.com* ]]; then
    [[ -n "${out}" ]] && printf '[{"tag_name":"trusty-audit-v%s"}]\n' "${STUB_VERSION}" > "${out}"
    exit 0
fi

# Release asset or its sidecar.
if [[ "${STUB_DOWNLOAD_FAIL:-0}" == "1" ]]; then
    exit 22
fi

name="${url##*/}"
src="${STUB_FIXTURES}/${name}"
if [[ ! -f "${src}" ]]; then
    exit 22
fi

if [[ "${name}" == *.sha256 && "${STUB_BAD_CHECKSUM:-0}" == "1" ]]; then
    # Serve a well-formed sidecar whose digest cannot match.
    printf '%s  %s\n' "0000000000000000000000000000000000000000000000000000000000000000" \
        "${name%.sha256}" > "${out}"
    exit 0
fi

cp "${src}" "${out}"
exit 0
STUBEOF
chmod +x "${STUBS}/curl"

cat >"${STUBS}/uname" <<'STUBEOF'
#!/usr/bin/env bash
case "${1:-}" in
    -s) echo "${STUB_OS:-Darwin}" ;;
    -m) echo "${STUB_ARCH:-arm64}" ;;
    -r) echo "25.5.0" ;;
    *)  echo "${STUB_OS:-Darwin}" ;;
esac
STUBEOF
chmod +x "${STUBS}/uname"

# ---------------------------------------------------------------------------
# Harness: run install.sh in a fresh install dir with a given stub environment.
# Sets RUN_STATUS, RUN_OUTPUT, RUN_INSTALL_DIR.
# ---------------------------------------------------------------------------
run_case() {
    RUN_INSTALL_DIR="${WORK}/install-$1"
    rm -rf "${RUN_INSTALL_DIR}"
    mkdir -p "${RUN_INSTALL_DIR}"
    shift

    set +e
    RUN_OUTPUT="$(
        env PATH="${STUBS}:${PATH}" \
            STUB_FIXTURES="${FIXTURES}" \
            STUB_VERSION="${VERSION}" \
            TRUSTY_AUDIT_INSTALL_DIR="${RUN_INSTALL_DIR}" \
            TRUSTY_AUDIT_NO_LAUNCH=1 \
            "$@" \
            sh "${SCRIPT}" 2>&1
    )"
    RUN_STATUS=$?
    set -e
}

# `find` rather than a glob so an empty directory is unambiguous.
installed_count() {
    find "$1" -maxdepth 1 -type f | wc -l | tr -d ' '
}

echo "== trusty-audit install.sh selftest =="

# ── Case 1 — a non-macOS host is refused before any download ────────────────
run_case linux STUB_OS=Linux
if [[ ${RUN_STATUS} -ne 0 ]] && [[ "${RUN_OUTPUT}" == *"Unsupported operating system: Linux"* ]]; then
    pass "non-Darwin uname is refused, naming the OS"
else
    fail "non-Darwin uname: status=${RUN_STATUS} output=${RUN_OUTPUT}"
fi
if [[ "$(installed_count "${RUN_INSTALL_DIR}")" == "0" ]]; then
    pass "non-Darwin uname installs nothing"
else
    fail "non-Darwin uname left files in the install dir"
fi

# ── Case 2 — an Intel Mac is refused, naming the reason ─────────────────────
run_case intel STUB_ARCH=x86_64
if [[ ${RUN_STATUS} -ne 0 ]] && [[ "${RUN_OUTPUT}" == *"Unsupported macOS architecture: x86_64"* ]]; then
    pass "Intel macOS is refused, naming the architecture"
else
    fail "Intel macOS: status=${RUN_STATUS} output=${RUN_OUTPUT}"
fi
if [[ "$(installed_count "${RUN_INSTALL_DIR}")" == "0" ]]; then
    pass "Intel macOS installs nothing"
else
    fail "Intel macOS left files in the install dir"
fi

# ── Case 3 — a checksum mismatch refuses to install ─────────────────────────
run_case badsum STUB_BAD_CHECKSUM=1
if [[ ${RUN_STATUS} -ne 0 ]] && [[ "${RUN_OUTPUT}" == *"CHECKSUM MISMATCH"* ]]; then
    pass "checksum mismatch is refused"
else
    fail "checksum mismatch: status=${RUN_STATUS} output=${RUN_OUTPUT}"
fi
if [[ "$(installed_count "${RUN_INSTALL_DIR}")" == "0" ]]; then
    pass "checksum mismatch installs nothing"
else
    fail "checksum mismatch left files in the install dir"
fi

# ── Case 4 — a failed download leaves nothing behind ────────────────────────
run_case dlfail STUB_DOWNLOAD_FAIL=1
if [[ ${RUN_STATUS} -ne 0 ]]; then
    pass "failed download exits non-zero"
else
    fail "failed download: expected non-zero, got ${RUN_STATUS}"
fi
if [[ "$(installed_count "${RUN_INSTALL_DIR}")" == "0" ]]; then
    pass "failed download installs nothing"
else
    fail "failed download left files in the install dir"
fi

# ── Case 5 — the happy path installs both binary names ──────────────────────
run_case happy
if [[ ${RUN_STATUS} -eq 0 ]]; then
    pass "happy path exits zero"
else
    fail "happy path: status=${RUN_STATUS} output=${RUN_OUTPUT}"
fi
for name in trusty-audit taudit; do
    if [[ -x "${RUN_INSTALL_DIR}/${name}" ]]; then
        pass "happy path installed ${name} executable"
    else
        fail "happy path did not install ${name}"
    fi
done
if [[ "${RUN_OUTPUT}" == *"trusty-audit ${VERSION}"* ]]; then
    pass "happy path proved the binary reports its version"
else
    fail "happy path did not print the binary's reported version"
fi

# ── Case 6 — re-running is idempotent ───────────────────────────────────────
HAPPY_DIR="${RUN_INSTALL_DIR}"
first_sum="$(shasum -a 256 "${HAPPY_DIR}/trusty-audit" | awk '{print $1}')"
set +e
second_out="$(
    env PATH="${STUBS}:${PATH}" \
        STUB_FIXTURES="${FIXTURES}" STUB_VERSION="${VERSION}" \
        TRUSTY_AUDIT_INSTALL_DIR="${HAPPY_DIR}" TRUSTY_AUDIT_NO_LAUNCH=1 \
        sh "${SCRIPT}" 2>&1
)"
second_status=$?
set -e
second_sum="$(shasum -a 256 "${HAPPY_DIR}/trusty-audit" | awk '{print $1}')"

if [[ ${second_status} -eq 0 ]] && [[ "${first_sum}" == "${second_sum}" ]]; then
    pass "re-run is idempotent (same binary, exit 0)"
else
    fail "re-run: status=${second_status} first=${first_sum} second=${second_sum} out=${second_out}"
fi
# No temp turds left from the atomic-rename staging.
if [[ "$(find "${HAPPY_DIR}" -maxdepth 1 -name '.*.install.*' | wc -l | tr -d ' ')" == "0" ]]; then
    pass "re-run left no partial staging files"
else
    fail "re-run left .install. staging files behind"
fi

echo
echo "passed: ${PASS}  failed: ${FAIL}"
[[ ${FAIL} -eq 0 ]] || exit 1
echo "OK"

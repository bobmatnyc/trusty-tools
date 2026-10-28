#!/usr/bin/env bash
#
# check-crate-size.sh — the packaged .crate must fit under the crates.io upload
# limit. Called by scripts/preflight-publish.sh as CHECK 10.
#
# Why: the trusty-mpm 1.9.0 publish failed at upload with crates.io
#   "413 Payload Too Large: max upload size is: 10485760". preflight-publish.sh,
#   check-publish-ready.sh and pre-publish.yml all passed, because none of them
#   measures the packaged .crate. The limit is a property of the packaged
#   archive, so only packaging the crate can see it.
#
# What: runs `cargo package -p <package> --no-verify`, finds
#   `<target-dir>/package/<package>-<version>.crate`, and compares its byte size
#   with the limit (10485760 B, the figure crates.io reports).
#     [FAIL] size >= limit, OR the .crate could not be packaged / found /
#            measured. Fail-closed: an unmeasurable crate never passes.
#     [WARN] size >= 90% of the limit (9437184 B). Exit 0. 90% leaves about
#            1 MiB, which is under one release of growth for the crates that
#            ship a UI bundle, so the WARN lands a release before the FAIL.
#     [PASS] below the margin.
#   The target dir is `${CARGO_TARGET_DIR:-<repo>/target}`, the same resolution
#   cargo uses for its own output (#10372: a hard-coded <repo>/target/ misses
#   the .envrc-exported CARGO_TARGET_DIR). A stale .crate from an earlier run is
#   deleted before packaging so it cannot stand in for this run's archive.
#
# Usage:
#   scripts/check-crate-size.sh <package-name> <version>
#   scripts/check-crate-size.sh --crate-file <path> <package-name> <version>
#     --crate-file measures an existing archive and skips `cargo package`.
#
# Exit: 0 = PASS or WARN, 1 = FAIL, 2 = usage.
#
# Test: scripts/check-crate-size-selftest.sh
#
# Portability: bash 3.2 (macOS) and bash 5; `wc -c` for size, no stat flags.

set -euo pipefail

LIMIT_BYTES=10485760
WARN_BYTES=$((LIMIT_BYTES * 90 / 100))

usage() {
  echo "usage: scripts/check-crate-size.sh [--crate-file <path>] <package-name> <version>" >&2
  exit 2
}

CRATE_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --crate-file)
      [ $# -ge 2 ] || usage
      CRATE_FILE="$2"; shift 2 ;;
    -h|--help) usage ;;
    --) shift; break ;;
    -*) usage ;;
    *) break ;;
  esac
done
[ $# -eq 2 ] || usage
PKG="$1"
VERSION="$2"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${CRATE_SIZE_REPO_ROOT:-$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)}"

fail() { echo "[FAIL] crate-size: $*" >&2; exit 1; }

if [ -z "$CRATE_FILE" ]; then
  TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
  CRATE_FILE="${TARGET_DIR}/package/${PKG}-${VERSION}.crate"
  rm -f "$CRATE_FILE"
  pkg_log="$(mktemp "${TMPDIR:-/tmp}/check-crate-size.XXXXXX")"
  trap 'rm -f "$pkg_log"' EXIT
  rc=0
  (cd "$REPO_ROOT" && cargo package -p "$PKG" --no-verify) > "$pkg_log" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    tail -n 15 "$pkg_log" | sed 's/^/       /' >&2
    fail "'cargo package -p ${PKG} --no-verify' exited ${rc}; the archive size is unknown."
  fi
fi

[ -f "$CRATE_FILE" ] || fail "no .crate at ${CRATE_FILE} after packaging ${PKG} ${VERSION}; the archive size is unknown (CARGO_TARGET_DIR='${CARGO_TARGET_DIR:-<unset>}')."

SIZE=""
SIZE="$(wc -c < "$CRATE_FILE" | tr -d '[:space:]')" || SIZE=""
case "$SIZE" in
  ''|*[!0-9]*) fail "could not measure ${CRATE_FILE} (wc -c gave '${SIZE}')." ;;
esac

DETAIL="${PKG} ${VERSION}: ${SIZE} B packaged (${CRATE_FILE}); crates.io limit ${LIMIT_BYTES} B"

if [ "$SIZE" -ge "$LIMIT_BYTES" ]; then
  fail "${DETAIL}. crates.io would reject the upload with 413. Shrink the package (check 'cargo package --list' and Cargo.toml include/exclude)."
fi
if [ "$SIZE" -ge "$WARN_BYTES" ]; then
  echo "[WARN] crate-size: ${DETAIL}; at or above 90% (${WARN_BYTES} B). The next release may not fit." >&2
  exit 0
fi
echo "[PASS] crate-size: ${DETAIL}; below the 90% margin (${WARN_BYTES} B)." >&2

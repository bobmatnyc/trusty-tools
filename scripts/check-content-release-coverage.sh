#!/usr/bin/env bash
#
# check-content-release-coverage.sh — a trusty-mpm release needs content that
# is published (#11470).
#
# Why: trusty-mpm 1.14.8 required `instructions/output-styles/owner-plain.md`,
#   which no published content-v* release carried. Its bundle tests read the
#   repository's own content/ tree, so they passed; the binary failed on every
#   host pinned to the newest release, content-v2.4.0.
# What: reads MIN_CONTENT_TAG (crates/trusty-mpm/src/content/min_content.rs)
#   and the files tm requires (REQUIRED_INSTRUCTIONS in
#   core/framework_content.rs, every `content_path` in core/bundle.rs). It
#   finds the newest published, non-draft, non-pre-release content-v* release
#   (the one `tm content update` pins) and fails unless that release is at or
#   above MIN_CONTENT_TAG and its bundle carries every required file.
# Usage:
#   scripts/check-content-release-coverage.sh
#   scripts/check-content-release-coverage.sh --source-root DIR --releases-dir DIR
#     --releases-dir: a fixture of the published set, one <tag>.tar.gz per
#     release, every one counted as published (the selftest uses it).
# Exit: 0 covered; 1 not covered; 2 the check could not run.
# Test: scripts/check-content-release-coverage-selftest.sh.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_ROOT="${REPO_ROOT}"
RELEASES_DIR=""
REPO="bobmatnyc/trusty-tools"

while [ $# -gt 0 ]; do
  case "$1" in
    --source-root) SOURCE_ROOT="$2"; shift 2 ;;
    --releases-dir) RELEASES_DIR="$2"; shift 2 ;;
    *) echo "check-content-release-coverage: unknown argument $1" >&2; exit 2 ;;
  esac
done

SRC="${SOURCE_ROOT}/crates/trusty-mpm/src"
MIN_TAG="$(sed -n 's/^pub const MIN_CONTENT_TAG: &str = "\(content-v[0-9.]*\)";/\1/p' \
  "${SRC}/content/min_content.rs")"
if [ -z "${MIN_TAG}" ]; then
  echo "[FAIL] content-coverage: no MIN_CONTENT_TAG in ${SRC}/content/min_content.rs" >&2
  exit 2
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/content-coverage.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT

# The required files, as bundle paths.
{
  sed -n '/^pub const REQUIRED_INSTRUCTIONS: &\[&str\] = &\[/,/^\];/p' \
    "${SRC}/core/framework_content.rs" | sed -n 's/^ *"\([^"]*\)",$/\1/p'
  sed -n 's/^ *content_path: "\([^"]*\)",$/\1/p' "${SRC}/core/bundle.rs"
} | sed 's|^|instructions/|' | sort -u > "${WORK}/required"
if [ ! -s "${WORK}/required" ]; then
  echo "[FAIL] content-coverage: read no required content paths from ${SRC}" >&2
  exit 2
fi

# version_key <tag> — a sortable key for content-vX.Y.Z.
version_key() {
  local v="${1#content-v}"
  IFS=. read -r a b c <<<"${v}"
  printf '%05d.%05d.%05d\n' "${a}" "${b}" "${c}"
}

# The published stable tags, one per line.
if [ -n "${RELEASES_DIR}" ]; then
  find "${RELEASES_DIR}" -maxdepth 1 -name 'content-v*.tar.gz' -exec basename {} .tar.gz \; \
    > "${WORK}/tags"
else
  if ! gh api --paginate "repos/${REPO}/releases?per_page=100" \
      --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name' \
      > "${WORK}/all" 2> "${WORK}/gh.err"; then
    echo "[FAIL] content-coverage: could not list ${REPO} releases: $(cat "${WORK}/gh.err")" >&2
    exit 2
  fi
  grep -E '^content-v[0-9]+\.[0-9]+\.[0-9]+$' "${WORK}/all" > "${WORK}/tags" || true
fi

NEWEST=""
NEWEST_KEY=""
while read -r tag; do
  [ -n "${tag}" ] || continue
  key="$(version_key "${tag}")"
  if [ -z "${NEWEST_KEY}" ] || [[ "${key}" > "${NEWEST_KEY}" ]]; then
    NEWEST="${tag}"; NEWEST_KEY="${key}"
  fi
done < "${WORK}/tags"

if [ -z "${NEWEST}" ]; then
  echo "[FAIL] content-coverage: no published content-v* release exists" >&2
  exit 1
fi
if [[ "${NEWEST_KEY}" < "$(version_key "${MIN_TAG}")" ]]; then
  echo "[FAIL] content-coverage: trusty-mpm needs ${MIN_TAG} (MIN_CONTENT_TAG), but the newest" >&2
  echo "       published content release is ${NEWEST}; publish ${MIN_TAG} first." >&2
  exit 1
fi

if [ -n "${RELEASES_DIR}" ]; then
  BUNDLE="${RELEASES_DIR}/${NEWEST}.tar.gz"
else
  if ! gh release download "${NEWEST}" -R "${REPO}" -p "${NEWEST}.tar.gz" -D "${WORK}" \
      2> "${WORK}/gh.err"; then
    echo "[FAIL] content-coverage: could not download ${NEWEST}: $(cat "${WORK}/gh.err")" >&2
    exit 2
  fi
  BUNDLE="${WORK}/${NEWEST}.tar.gz"
fi
tar tzf "${BUNDLE}" | sed 's|^\./||' | sort -u > "${WORK}/carried"

# A required file may still be served from its pre-D07 path
# (architect_compat::legacy_instruction_path): architect/<f> from
# supervisor/<f>, and the Architect style from the supervisor style.
legacy_path() {
  case "$1" in
    instructions/architect/*) echo "instructions/supervisor/${1#instructions/architect/}" ;;
    instructions/output-styles/trusty-mpm-architect.md)
      echo "instructions/output-styles/trusty-mpm-supervisor.md" ;;
  esac
}
MISSING=""
while read -r path; do
  grep -qxF "${path}" "${WORK}/carried" && continue
  legacy="$(legacy_path "${path}")"
  [ -n "${legacy}" ] && grep -qxF "${legacy}" "${WORK}/carried" && continue
  MISSING="${MISSING}${path}"$'\n'
done < "${WORK}/required"
MISSING="${MISSING%$'\n'}"
if [ -n "${MISSING}" ]; then
  echo "[FAIL] content-coverage: ${NEWEST}, the newest published content release, lacks files" >&2
  echo "       trusty-mpm requires; publish a content release that carries them and raise" >&2
  echo "       MIN_CONTENT_TAG to it:" >&2
  printf '%s\n' "${MISSING}" | sed 's/^/         /' >&2
  exit 1
fi

echo "[PASS] content-coverage: ${NEWEST} (>= ${MIN_TAG}) carries all $(wc -l < "${WORK}/required" | tr -d ' ') required files" >&2

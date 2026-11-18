#!/usr/bin/env bash
#
# check-content-release-coverage-selftest.sh — fixtures for the content
# release coverage gate (#11470).
#
# Why: the gate blocks a trusty-mpm release; a gate that cannot fail protects
#   nothing, so each verdict is driven here against a synthetic source tree
#   and a synthetic published set, with no network.
# What: case 1 replays 1.14.8 (requires owner-plain.md, newest published
#   lacks it) and must fail; case 2 a minimum above every published release
#   must fail; case 3 a covering release passes; case 4 a file served from
#   its pre-D07 supervisor path counts; case 5 a source with no
#   MIN_CONTENT_TAG cannot run (exit 2); case 6 a newest release that covers
#   everything over a minimum that lacks a file must fail and name the fix.
# Usage: bash scripts/check-content-release-coverage-selftest.sh
# Test: this IS the test; ci.yml runs it.

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${REPO_ROOT}/scripts/check-content-release-coverage.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/coverage-selftest.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT
FAIL=0

# source <dir> <min-tag> — a minimal crates/trusty-mpm/src tree.
source_tree() {
  local src="$1/crates/trusty-mpm/src"
  mkdir -p "${src}/content" "${src}/core"
  [ -n "$2" ] && printf 'pub const MIN_CONTENT_TAG: &str = "%s";\n' "$2" > "${src}/content/min_content.rs"
  [ -n "$2" ] || : > "${src}/content/min_content.rs"
  printf '%s\n' 'pub const REQUIRED_INSTRUCTIONS: &[&str] = &[' '    "sections/core.md",' \
    '    "architect/identity.md",' '];' > "${src}/core/framework_content.rs"
  printf '%s\n' '        content_path: "output-styles/owner-plain.md",' > "${src}/core/bundle.rs"
}

# release <dir> <tag> <file>... — one published bundle carrying the files.
release() {
  local dir="$1" tag="$2"; shift 2
  local stage="${WORK}/stage-${tag}"
  mkdir -p "${dir}" "${stage}"
  for f in "$@"; do mkdir -p "${stage}/$(dirname "$f")"; : > "${stage}/$f"; done
  (cd "${stage}" && tar czf "${dir}/${tag}.tar.gz" "$@")
}

expect() {
  local name="$1" want="$2" rc=0; shift 2
  bash "${GATE}" "$@" > "${WORK}/out" 2>&1 || rc=$?
  if [ "${rc}" = "${want}" ]; then echo "ok   ${name} (exit ${rc})"
  else echo "FAIL ${name}: exit ${rc}, want ${want}"; sed 's/^/     /' "${WORK}/out"; FAIL=1; fi
}

source_tree "${WORK}/s1" content-v2.4.0
release "${WORK}/r1" content-v2.4.0 instructions/sections/core.md instructions/architect/identity.md
expect "1.14.8 replay: newest release lacks a required file" 1 \
  --source-root "${WORK}/s1" --releases-dir "${WORK}/r1"
grep -q "instructions/output-styles/owner-plain.md" "${WORK}/out" \
  || { echo "FAIL case 1 does not name the missing file"; FAIL=1; }

source_tree "${WORK}/s2" content-v2.5.0
expect "minimum above every published release" 1 \
  --source-root "${WORK}/s2" --releases-dir "${WORK}/r1"

release "${WORK}/r3" content-v2.5.0 instructions/sections/core.md \
  instructions/architect/identity.md instructions/output-styles/owner-plain.md
cp "${WORK}/r1/content-v2.4.0.tar.gz" "${WORK}/r3/"
expect "a covering release at the minimum" 0 \
  --source-root "${WORK}/s2" --releases-dir "${WORK}/r3"

release "${WORK}/r4" content-v2.5.0 instructions/sections/core.md \
  instructions/supervisor/identity.md instructions/output-styles/owner-plain.md
expect "a pre-D07 supervisor path counts" 0 \
  --source-root "${WORK}/s2" --releases-dir "${WORK}/r4"

source_tree "${WORK}/s5" ""
expect "no MIN_CONTENT_TAG" 2 --source-root "${WORK}/s5" --releases-dir "${WORK}/r3"

# #11470 F1: a host pinned to the minimum gets the minimum's files, so the
# minimum must carry them too, whatever the newest release carries.
expect "the minimum lacks a file the newest carries" 1 \
  --source-root "${WORK}/s1" --releases-dir "${WORK}/r3"
grep -q "raise MIN_CONTENT_TAG" "${WORK}/out" \
  || { echo "FAIL case 6 does not say to raise MIN_CONTENT_TAG"; FAIL=1; }

[ "${FAIL}" -eq 0 ] && echo "check-content-release-coverage-selftest: all cases pass"
exit "${FAIL}"

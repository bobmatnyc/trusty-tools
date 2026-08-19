#!/usr/bin/env bash
#
# check_source_class_selftest.sh — fixtures for the shared source/test path
# classification in scripts/lib/source_class.sh (issue #5765).
#
# Why: this library exists because two gates classified the same path two ways
#   and neither carried a test that said what the right answer was. A shared
#   definition with no fixtures would be the same defect with one fewer copy —
#   the next edit to `is_test_path` silently changes what both gates do. It also
#   asserts the WIRING: a gate that quietly grows a private copy again puts the
#   repo back where #5765 found it, and only a grep can see that.
#
# What: three groups.
#   is_test_path       every shape that qualifies, and the near-misses that must
#                      not (`tests_helper.rs`, `contests/`, `src/testdata.rs`)
#   is_crate_src_path  the depth-1 shape, and the nested/adjacent paths that are
#                      deliberately outside it
#   wiring             both gates source the library; neither redefines it
#
# Usage: bash scripts/check_source_class_selftest.sh
# Exit: 0 when every case matches; 1 listing each mismatch.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI), same as the
#   library under test.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

# shellcheck source=lib/source_class.sh
. "${REPO_ROOT}/scripts/lib/source_class.sh"

FAILURES=0
CASES=0

# assert <label> <expected: yes|no> <actual-exit-status>
assert() {
  local label="$1" want="$2" rc="$3" got
  CASES=$((CASES + 1))
  if [ "$rc" -eq 0 ]; then got="yes"; else got="no"; fi
  if [ "$got" = "$want" ]; then
    printf '  ok   %-58s -> %s\n' "$label" "$got"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $label: expected '$want', got '$got'"
  fi
}

test_path() {
  local path="$1" want="$2" rc=0
  is_test_path "$path" || rc=$?
  assert "is_test_path $path" "$want" "$rc"
}

src_path() {
  local path="$1" want="$2" rc=0
  is_crate_src_path "$path" || rc=$?
  assert "is_crate_src_path $path" "$want" "$rc"
}

echo "is_test_path:"
test_path "crates/trusty-review/src/report/reporter_tests.rs" yes
test_path "crates/trusty-common/src/embedder/mod_test.rs" yes
test_path "crates/trusty-mpm/src/session/tests.rs" yes
test_path "crates/trusty-search/src/index/tests/fixture.rs" yes
test_path "crates/trusty-search/benches/query.rs" yes
test_path "crates/trusty-audit/src/testdata/report.json" yes
# The near-misses. Each is production code whose NAME merely resembles a test
# file; classifying one as a test would exempt a real change from the changelog
# gate, which is the direction that loses coverage.
test_path "crates/trusty-mpm/src/session/tests_helper.rs" no
test_path "crates/trusty-mpm/src/testdata.rs" no
test_path "crates/trusty-mpm/src/contests/mod.rs" no
test_path "crates/trusty-mpm/src/lib.rs" no
test_path "crates/trusty-mpm/src/attest.rs" no

echo "is_crate_src_path:"
src_path "crates/trusty-mpm/src/lib.rs" yes
src_path "crates/trusty-mpm/src/a/b/c.rs" yes
src_path "crates/trusty-review/src/report/reporter_tests.rs" yes
# NESTED members are deliberately outside this predicate — `cargo package`
# excludes a nested package from its parent's tarball, so a nested edit is not
# drift against the parent's published version. scripts/check_changelog_fragment.sh
# reaches them by structural attribution instead (#4576).
src_path "crates/trusty-audit/ui/src-tauri/src/main.rs" no
src_path "crates/trusty-agents/ui/src/App.svelte" no
# Adjacent shapes that must not read as crate source.
src_path "crates/trusty-mpm/Cargo.toml" no
src_path "crates/trusty-mpm/changelog.d/1-x.md" no
src_path "crates/trusty-mpm/srcx/a.rs" no
src_path "crates/trusty-mpm/src" no
src_path "crates/trusty-mpm" no
src_path "docs/reference/crate-map.md" no
src_path "scripts/bump-version.sh" no
src_path "" no

echo "crate_of_src_path:"
CASES=$((CASES + 1))
got="$(crate_of_src_path "crates/trusty-review/src/report/reporter_tests.rs" 2>/dev/null)"
if [ "$got" = "trusty-review" ]; then
  printf '  ok   %-58s -> %s\n' "names the top-level crate" "$got"
else
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: crate_of_src_path: expected 'trusty-review', got '$got'"
fi

CASES=$((CASES + 1))
if crate_of_src_path "docs/x.md" >/dev/null 2>&1; then
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: crate_of_src_path accepted a non-source path"
else
  printf '  ok   %-58s -> %s\n' "refuses a non-source path" "no"
fi

# ---------------------------------------------------------------------------
# Wiring. The library is only a fix while both gates actually read it.
# ---------------------------------------------------------------------------
echo "wiring:"
wiring() {
  local label="$1" want="$2" got="$3"
  CASES=$((CASES + 1))
  if [ "$got" = "$want" ]; then
    printf '  ok   %-58s -> %s\n' "$label" "$got"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $label: expected '$want', got '$got'"
  fi
}

# The `.` line itself, not a mention of the filename in a comment.
wiring "check-pr-version-bump.sh sources the library" "1" \
  "$(grep -c '^\. .*lib/source_class\.sh"$' scripts/check-pr-version-bump.sh || true)"
wiring "check_changelog_fragment.sh sources the library" "1" \
  "$(grep -c '^\. .*lib/source_class\.sh"$' scripts/check_changelog_fragment.sh || true)"
# A private redefinition is how the two definitions drifted apart in the first
# place, so neither gate may declare one of its own again.
wiring "check-pr-version-bump.sh defines no is_test_path" "0" \
  "$(grep -c '^is_test_path()' scripts/check-pr-version-bump.sh || true)"
wiring "check_changelog_fragment.sh defines no is_test_path" "0" \
  "$(grep -c '^is_test_path()' scripts/check_changelog_fragment.sh || true)"
wiring "check-pr-version-bump.sh keeps no private src/ regex" "0" \
  "$(grep -c 'crates/\[\^/\]+/src/' scripts/check-pr-version-bump.sh || true)"

echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo "check_source_class_selftest: ${CASES} case(s), all pass"
  exit 0
fi
echo "check_source_class_selftest: ${FAILURES} of ${CASES} case(s) FAILED" >&2
exit 1

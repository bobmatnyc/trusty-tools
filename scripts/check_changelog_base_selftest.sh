#!/usr/bin/env bash
#
# check_changelog_base_selftest.sh — base-attribution regressions for
# scripts/check_changelog_fragment.sh (issue #5018).
#
# Why: the gate blames whatever `git merge-base BASE HEAD`..HEAD contains, so
#   the base ref alone decides which commits count as "this PR's". Its workflow
#   passed `github.event.pull_request.base.sha`, a snapshot frozen when the PR
#   event fired, while `actions/checkout` resolved the same event to
#   `refs/pull/N/merge` — a merge commit whose first parent is the base
#   branch's CURRENT tip. The stale SHA is an ancestor of that merge ref, so
#   `git merge-base` returns it unchanged and the diff spans every commit main
#   took in between. A release inside that window assembles a crate's fragments
#   into CHANGELOG.md and DELETES `changelog.d/*.md`, and the gate excludes
#   deletions from evidence, so an untouched crate reads as "source changed, no
#   fragment". PR #5018 changed one crate and was failed for three others.
#
#   Neither the base handling nor the fix had a test. This is it.
#
# What: builds a throwaway git repo carrying the exact shape — crate A touched
#   by the branch, crate B released on main since the fork point — checks out a
#   GitHub-shaped merge ref, and asserts the gate's verdict under each base.
#
#   Cases:
#     stale-base-refused        a bare fork-point SHA under a merge ref is
#                               REFUSED naming the base, and crate B is never
#                               blamed. This is the case that goes red against
#                               the pre-#5018 script, which instead reported
#                               "crate-b: src/** changed with no changelog
#                               record" for a crate the branch never touched.
#     live-base-passes          the base BRANCH passes, scanning only the
#                               branch's own paths and naming only crate A.
#     missing-fragment-fails    crate A's src/** changed with no fragment still
#                               FAILS. The fix must not weaken the gate.
#     exact-base-parent-allowed a bare SHA that IS the merge ref's base parent
#                               is exact, not stale, and still runs.
#     plain-head-sha-allowed    a bare SHA base against a NON-merge HEAD is the
#                               true fork point and still runs. The guard is
#                               scoped to the merge-ref shape it was written
#                               for, and must not fire outside it.
#
# Test: this IS the test. Run directly:
#   bash scripts/check_changelog_base_selftest.sh
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only. Same constraints as the script under test.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/changelog-base-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

REPO="$TMP_ROOT/repo"
GATE="scripts/check_changelog_fragment.sh"
fail=0

g() { git -C "$REPO" "$@"; }

# ---------------------------------------------------------------------------
# Fixture: two crates, a branch that touches only crate-a, and a release on
# main that changes crate-b's source and CONSUMES its fragments.
# ---------------------------------------------------------------------------
mkdir -p "$REPO/scripts/lib"
cp "$SCRIPT_DIR/check_changelog_fragment.sh" "$REPO/scripts/"
cp "$SCRIPT_DIR/assemble-changelog.sh" "$REPO/scripts/"
# #5765: the gate sources its path classification from `scripts/lib/` beside
# itself, so the library travels with it into the synthetic repo.
cp "$SCRIPT_DIR/lib/source_class.sh" "$REPO/scripts/lib/"

new_crate() {
  local name="$1" dir="$REPO/crates/$1"
  mkdir -p "$dir/src" "$dir/changelog.d"
  printf 'pub fn v() -> u32 { 1 }\n' >"$dir/src/lib.rs"
  printf '[package]\nname = "%s"\nversion = "0.1.0"\n' "$name" >"$dir/Cargo.toml"
  printf '# Changelog\n\n---\n' >"$dir/CHANGELOG.md"
  printf 'Placeholder keeping changelog.d/ tracked between releases.\n' \
    >"$dir/changelog.d/README.md"
}

fragment() {
  # crate, filename, category
  printf '%s\n\n- a user-visible change in %s\n' "$3" "$1" \
    >"$REPO/crates/$1/changelog.d/$2"
}

g init -q -b main
g config user.email selftest@example.invalid
g config user.name "changelog base self-test"

new_crate crate-a
new_crate crate-b
# crate-b enters the window with unreleased fragments, exactly as a real crate
# does between releases.
fragment crate-b 4100-b-earlier.md Fixed
fragment crate-b 4200-b-later.md Added
g add -A
g commit -qm "M0: two crates, crate-b carrying unreleased fragments"
FORK_POINT="$(g rev-parse HEAD)"

# The branch. Touches crate-a's source and records it — a correct, complete PR.
g checkout -q -b pr
printf 'pub fn v() -> u32 { 2 }\n' >"$REPO/crates/crate-a/src/lib.rs"
fragment crate-a 5018-a-change.md Fixed
g add -A
g commit -qm "PR: change crate-a source, add crate-a fragment"
PR_HEAD="$(g rev-parse HEAD)"

# Main moves on without the branch: crate-b's source changes, and a release
# folds its fragments into CHANGELOG.md and deletes them. This is assembly, not
# omission — and it is what a stale base misreads.
g checkout -q main
printf 'pub fn v() -> u32 { 7 }\n' >"$REPO/crates/crate-b/src/lib.rs"
g rm -q "crates/crate-b/changelog.d/4100-b-earlier.md" \
  "crates/crate-b/changelog.d/4200-b-later.md"
printf '# Changelog\n\n---\n\n## [0.1.1]\n\n### Fixed\n\n- released bullet\n' \
  >"$REPO/crates/crate-b/CHANGELOG.md"
g add -A
g commit -qm "M1: crate-b source change + release consuming crate-b fragments"
MAIN_TIP="$(g rev-parse HEAD)"

# The merge ref GitHub builds for a pull_request event: first parent is the
# CURRENT base tip, second is the PR head. `actions/checkout` checks this out,
# which is what lets a stale base outrank the fork point.
g checkout -q --detach main
g merge -q --no-ff -m "Merge ${PR_HEAD} into ${MAIN_TIP}" pr
MERGE_REF="$(g rev-parse HEAD)"

# ---------------------------------------------------------------------------
# assert_case: name, base ref, expected exit status, ERE the output must match,
# ERE the output must NOT match ("" to skip either match).
# ---------------------------------------------------------------------------
assert_case() {
  local name="$1" base="$2" want_rc="$3" want_re="$4" deny_re="$5" out rc=0
  # Capture, then match. NOT `... | grep -q`: under `set -o pipefail` the gate's
  # own (expected) non-zero exit becomes the pipeline's status even when grep
  # matched, so every rejection assertion would read as a miss.
  out="$(cd "$REPO" && CHANGELOG_GATE_BASE="$base" bash "$GATE" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_rc" ]; then
    echo "FAIL: $name -> exit $rc (expected $want_rc)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  if [ -n "$want_re" ] && ! grep -qE "$want_re" <<<"$out"; then
    echo "FAIL: $name -> exit $rc as expected, but output does not match /$want_re/" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  if [ -n "$deny_re" ] && grep -qE "$deny_re" <<<"$out"; then
    echo "FAIL: $name -> exit $rc as expected, but output MATCHES forbidden /$deny_re/" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  echo "PASS: $name -> exit $rc${want_re:+, matched /$want_re/}"
}

# 1. THE #5018 DEFECT. A pinned fork-point SHA under the merge ref must be
#    refused, and crate-b — which this branch never touched — must never be
#    named. The pre-fix script fails here by blaming crate-b instead.
assert_case stale-base-refused "$FORK_POINT" 1 \
  'STALE BASE' \
  'crate-b'

# 2. THE FIX. Against the base BRANCH the merge base is the merge ref's own
#    first parent, so the diff is exactly the branch's contribution.
assert_case live-base-passes main 0 \
  'OK   crate-a: changelog.d fragment present and valid' \
  'crate-b'

# 3. STILL A GATE. Drop crate-a's fragment and the same live base must fail.
g checkout -q pr
g rm -q crates/crate-a/changelog.d/5018-a-change.md
g commit -qm "PR: drop the crate-a fragment"
NOFRAG_HEAD="$(g rev-parse HEAD)"
g checkout -q --detach main
g merge -q --no-ff -m "Merge ${NOFRAG_HEAD} into ${MAIN_TIP}" pr
assert_case missing-fragment-fails main 1 \
  'FAIL crate-a: crates/crate-a/src/\*\* changed with no changelog record' \
  ''

# 4. NOT OVER-BROAD. A bare SHA that IS the merge ref's base parent is exact,
#    not stale, so the guard must let it through and reach the real verdict.
assert_case exact-base-parent-allowed "$MAIN_TIP" 1 \
  'FAIL crate-a: crates/crate-a/src/\*\* changed with no changelog record' \
  'STALE BASE'

# 5. NOT OVER-BROAD. Against a plain (non-merge) HEAD the same fork-point SHA
#    IS the fork point, so it stays correct and must not be refused.
g checkout -q --detach "$PR_HEAD"
assert_case plain-head-sha-allowed "$FORK_POINT" 0 \
  'OK   crate-a: changelog.d fragment present and valid' \
  'STALE BASE'

echo
if [ "$fail" -ne 0 ]; then
  echo "check_changelog_base_selftest: FAILED — the gate mis-attributes changes." >&2
  echo "  Merge ref under test: ${MERGE_REF}" >&2
  exit 1
fi
echo "check_changelog_base_selftest: all base-attribution cases passed."

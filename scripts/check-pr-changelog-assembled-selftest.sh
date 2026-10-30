#!/usr/bin/env bash
#
# check-pr-changelog-assembled-selftest.sh — synthetic-repo fixtures for
# scripts/check-pr-changelog-assembled.sh (issue #7439).
#
# Why: the gate's header cited this file for months while it did not exist, so
#   the coverage it claimed never ran. That is how the #7359 pipefail bug
#   shipped: a PR that DELETES a crate's CHANGELOG.md produces a pure-deletion
#   diff with no `+## [` lines, grep exits 1, pipefail propagates it, and
#   `set -e` killed the assignment before the guard below could skip the crate
#   — the job failed with no output at all. A gate whose failing AND passing
#   branches are never exercised is indistinguishable from one that checks
#   nothing, which is the same shape #6406 exists to close.
#
# What: builds a throwaway git repo per case, each with a base commit and a
#   second commit standing in for the PR, then runs the gate INSIDE that repo
#   with `--base <base-sha>` and asserts the exit status and the finding on
#   stdout/stderr. The gate resolves its own REPO_ROOT from its location and
#   shells out to `${REPO_ROOT}/scripts/check-changelog-assembled.sh`, so both
#   scripts are copied into each fixture's `scripts/` — the same copy-the-gate
#   -into-a-checkout pattern scripts/check_rustdoc_links_selftest.sh documents
#   for its own `--gate`.
#
# Usage:
#   ./scripts/check-pr-changelog-assembled-selftest.sh
#   ./scripts/check-pr-changelog-assembled-selftest.sh --gate <path-to-a-copy>
#
#   --gate swaps in a different copy of the gate under test. It exists so a
#   case can be proven RED against a pre-fix gate before the fix is trusted.
#
# Exit: 0 when every case matches its expectation, 1 otherwise, 2 on usage.
#
# Test: this IS the test. Run directly:
#   ./scripts/check-pr-changelog-assembled-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check-pr-changelog-assembled.sh"
HELPER="${SCRIPT_DIR}/check-changelog-assembled.sh"

while [ $# -gt 0 ]; do
  case "$1" in
    --gate)
      [ $# -lt 2 ] && {
        echo "ERROR: --gate needs a path" >&2
        exit 2
      }
      GATE="$2"
      shift 2
      ;;
    -h | --help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

[ -f "$GATE" ] || {
  echo "ERROR: gate not found: $GATE" >&2
  exit 2
}

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pr-changelog-assembled-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

# mkrepo <name> <crate-dir> <version> — prints the repo path. Creates a git repo
# holding the two gate scripts, a crate manifest, and a CHANGELOG.md with one
# already-released section, then commits it as the merge base.
mkrepo() {
  local name="$1" dir="$2" version="$3"
  local repo="${WORK}/${name}"
  mkdir -p "${repo}/scripts" "${repo}/crates/${dir}/changelog.d"
  cp "$GATE" "${repo}/scripts/check-pr-changelog-assembled.sh"
  cp "$HELPER" "${repo}/scripts/check-changelog-assembled.sh"
  printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n' \
    "$dir" "$version" > "${repo}/crates/${dir}/Cargo.toml"
  echo "placeholder" > "${repo}/crates/${dir}/changelog.d/README.md"
  printf '# Changelog — %s\n\n---\n\n## [0.1.0] — 2026-01-01\n\n### Fixed\n\n- an older, already-released fix\n' \
    "$dir" > "${repo}/crates/${dir}/CHANGELOG.md"
  git -C "$repo" init -q
  git -C "$repo" add -A
  git -C "$repo" -c user.email=t@t -c user.name=t commit -qm base
  echo "$repo"
}

# commit_pr <repo> — commits the working tree as the simulated PR head.
commit_pr() {
  git -C "$1" add -A
  git -C "$1" -c user.email=t@t -c user.name=t commit -qm pr
}

# run_case <label> <expect-exit> <expect-substring|-> <repo> [base-ref]
# The base defaults to HEAD~1, the commit mkrepo made.
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  local base out rc=0
  base="${5:-$(git -C "$repo" rev-parse HEAD~1)}"
  out="$(cd "$repo" && bash scripts/check-pr-changelog-assembled.sh --base "$base" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_exit" ]; then
    fail_case "${label}: expected exit ${want_exit}, got ${rc}" "$out"
    return
  fi
  if [ "$want_sub" != "-" ] && ! printf '%s\n' "$out" | grep -qF -- "$want_sub"; then
    fail_case "${label}: exit ${rc} but output never said '${want_sub}'" "$out"
    return
  fi
  if [ "$want_sub" = "-" ]; then
    pass_case "${label} -> exit ${rc} (clean)"
  else
    pass_case "${label} -> exit ${rc}, reported ${want_sub}"
  fi
}

# ===========================================================================
# 1. THE #7359 REGRESSION. The PR DELETES the crate's CHANGELOG.md. The diff
#    is pure deletion, so it carries no `+## [` line and grep exits 1. The gate
#    must read that as "no new headings here" and pass, not die on pipefail
#    with no output. This is the case whose absence let the bug ship.
# ===========================================================================
repo="$(mkrepo deleted deleted-crate 0.1.0)"
rm "${repo}/crates/deleted-crate/CHANGELOG.md"
commit_pr "$repo"
run_case "PR deletes CHANGELOG.md" 0 "-" "$repo"

# ===========================================================================
# 2. THE #6406 BYPASS. The PR hand-writes a `## [99.0.0]` section while
#    changelog.d/ still holds the fragment a real assemble run would have
#    consumed and deleted. Exit 1, STRANDED-FRAGMENTS.
# ===========================================================================
repo="$(mkrepo stranded stranded-crate 99.0.0)"
printf 'Fixed\n\n- a fix nobody ever folded in\n' \
  > "${repo}/crates/stranded-crate/changelog.d/6406-stranded.md"
printf '# Changelog — stranded-crate\n\n---\n\n## [99.0.0] — 2026-01-02\n\n### Fixed\n\n- hand-written, never assembled\n\n## [0.1.0] — 2026-01-01\n\n### Fixed\n\n- an older, already-released fix\n' \
  > "${repo}/crates/stranded-crate/CHANGELOG.md"
commit_pr "$repo"
run_case "PR hand-writes a section, fragment stranded" 1 "STRANDED-FRAGMENTS" "$repo"

# ===========================================================================
# 3. The honest release cut, for contrast. The same new section, but the
#    fragment is gone in the same commit — what scripts/assemble-changelog.sh
#    always leaves behind. Without this case, a gate that failed on EVERY new
#    section would still pass cases 1 and 2.
# ===========================================================================
repo="$(mkrepo assembled assembled-crate 1.0.0)"
printf '# Changelog — assembled-crate\n\n---\n\n## [1.0.0] — 2026-01-02\n\n### Fixed\n\n- assembled for real\n\n## [0.1.0] — 2026-01-01\n\n### Fixed\n\n- an older, already-released fix\n' \
  > "${repo}/crates/assembled-crate/CHANGELOG.md"
commit_pr "$repo"
run_case "PR assembles cleanly" 0 "OK   assembled-crate 1.0.0" "$repo"

# ===========================================================================
# 4-12. A RELEASE BRANCH THAT MERGED MAIN (#10497). mkrelease builds the PR
#    #10497 topology: main holds fragment 100-cut.md at the cut; the release
#    branch assembles `## [1.0.0]` from it and deletes it; main then gains
#    200-post.md, the NEXT release's; the release branch merges main (its own
#    head as first parent, as GitHub's update-branch does). Each case then
#    edits the release branch before the final commit.
# ===========================================================================
gitc() {
  local repo="$1"
  shift
  git -C "$repo" -c user.email=t@t -c user.name=t -c commit.gpgsign=false "$@"
}

REL_SECTION='## [1.0.0] — 2026-01-02\n\n### Fixed\n\n- fix at the cut\n\n'

# mkrelease <name> — prints the repo path, checked out on `release` after the
# merge of main, with the merge committed.
mkrelease() {
  local repo dir=rel-crate
  repo="$(mkrepo "$1" "$dir" 1.0.0)"
  local cl="${repo}/crates/${dir}/CHANGELOG.md" fd="${repo}/crates/${dir}/changelog.d"
  gitc "$repo" branch -M main
  printf 'Fixed\n\n- fix at the cut\n' > "${fd}/100-cut.md"
  gitc "$repo" add -A
  gitc "$repo" commit -qm cut-point
  gitc "$repo" checkout -q -b release
  # shellcheck disable=SC2059  # REL_SECTION is a format string by design
  printf "# Changelog — ${dir}\n\n---\n\n${REL_SECTION}## [0.1.0] — 2026-01-01\n\n### Fixed\n\n- an older, already-released fix\n" > "$cl"
  rm "${fd}/100-cut.md"
  gitc "$repo" add -A
  gitc "$repo" commit -qm "release 1.0.0"
  gitc "$repo" checkout -q main
  printf 'Added\n\n- post-cut feature for the next release\n' > "${fd}/200-post.md"
  gitc "$repo" add -A
  gitc "$repo" commit -qm post-cut
  gitc "$repo" checkout -q release
  gitc "$repo" merge -q --no-ff -m "Merge main into release" main
  echo "$repo"
}

# 4. THE #10497 REGRESSION: the merge alone. 200-post.md is the next release's.
repo="$(mkrelease rel-merged)"
run_case "release branch merged main: post-cut fragment ignored" 0 "changelog.d/200-post.md" "$repo" main

# 5. CI's refs/pull/N/merge shape: main is the FIRST parent of HEAD.
repo="$(mkrelease rel-ci-merge)"
gitc "$repo" checkout -q main
gitc "$repo" merge -q --no-ff -m "ci merge ref" release
run_case "CI merge ref (main first parent): post-cut fragment ignored" 0 "OK   rel-crate 1.0.0" "$repo" main~1

# 6. Real strand: the release consumed 100-cut.md into the section but left
#    the file. Present at the cut, and its text sits in the section.
repo="$(mkrelease rel-consumed-kept)"
gitc "$repo" checkout -q main~1 -- crates/rel-crate/changelog.d/100-cut.md
commit_pr "$repo"
run_case "release kept a fragment it consumed" 1 "changelog.d/100-cut.md" "$repo" main

# 7. Real strand: a fragment present at the cut that the release neither
#    folded in nor deleted (its text is NOT in the section).
repo="$(mkrepo rel-cut-left rel-crate 1.0.0)"
gitc "$repo" branch -M main
printf 'Fixed\n\n- fix at the cut\n' > "${repo}/crates/rel-crate/changelog.d/100-cut.md"
printf 'Fixed\n\n- left behind at the cut\n' > "${repo}/crates/rel-crate/changelog.d/101-left.md"
gitc "$repo" add -A
gitc "$repo" commit -qm cut-point
gitc "$repo" checkout -q -b release
# shellcheck disable=SC2059
printf "# Changelog — rel-crate\n\n---\n\n${REL_SECTION}" > "${repo}/crates/rel-crate/CHANGELOG.md"
rm "${repo}/crates/rel-crate/changelog.d/100-cut.md"
commit_pr "$repo"
gitc "$repo" checkout -q main
printf 'Added\n\n- post-cut feature for the next release\n' > "${repo}/crates/rel-crate/changelog.d/200-post.md"
commit_pr "$repo"
gitc "$repo" checkout -q release
gitc "$repo" merge -q --no-ff -m "Merge main into release" main
run_case "fragment present at the cut, not consumed, not deleted" 1 "changelog.d/101-left.md" "$repo" main

# 8. Real strand: main's post-cut fragment folded into the section by hand
#    after the merge, file left in place.
repo="$(mkrelease rel-post-folded)"
cl="${repo}/crates/rel-crate/CHANGELOG.md"
awk '{ print } /^- fix at the cut$/ { print "- post-cut feature for the next release" }' \
  "$cl" > "${cl}.new"
mv "${cl}.new" "$cl"
commit_pr "$repo"
run_case "post-cut fragment folded into the section but kept" 1 "changelog.d/200-post.md" "$repo" main

# 9. Criterion of the Release Window rule: a fragment the release branch ADDS
#    itself must be folded in with --merge; left unconsumed it is stranded.
repo="$(mkrelease rel-own-added)"
printf 'Fixed\n\n- release-branch fix\n' > "${repo}/crates/rel-crate/changelog.d/300-own.md"
commit_pr "$repo"
run_case "fragment the release branch added, not folded in" 1 "changelog.d/300-own.md" "$repo" main

# 10. A main fragment the release branch EDITED is the branch's own: stranded.
repo="$(mkrelease rel-edited)"
printf 'Added\n\n- post-cut feature, reworded on the release branch\n' \
  > "${repo}/crates/rel-crate/changelog.d/200-post.md"
commit_pr "$repo"
run_case "post-cut fragment edited on the release branch" 1 "changelog.d/200-post.md" "$repo" main

# 11. Empty changelog.d/ (removed entirely) and README-only, after a merge.
repo="$(mkrelease rel-no-dir)"
gitc "$repo" rm -q -r crates/rel-crate/changelog.d
gitc "$repo" commit -qm "drop changelog.d"
gitc "$repo" checkout -q main
gitc "$repo" rm -q crates/rel-crate/changelog.d/200-post.md
gitc "$repo" commit -qm "main releases 200-post"
gitc "$repo" checkout -q release
gitc "$repo" merge -q --no-ff -m "Merge main again" main
run_case "no changelog.d/ at all after merging main" 0 "OK   rel-crate 1.0.0" "$repo" main
repo="$(mkrelease rel-readme-only)"
gitc "$repo" rm -q crates/rel-crate/changelog.d/200-post.md
gitc "$repo" commit -qm "drop post-cut fragment"
run_case "README-only changelog.d/ after merging main" 0 "OK   rel-crate 1.0.0" "$repo" main

# 12. Failure paths: a base ref that does not exist fails the PR gate (exit
#     1, never a silent pass), and the helper refuses an unresolvable --base
#     with exit 2. Without --base the helper keeps its tag-time verdict on the
#     #10497 tree: STRANDED-FRAGMENTS.
repo="$(mkrelease rel-bad-base)"
run_case "base ref missing" 1 "cannot find a merge base" "$repo" no-such-ref
rc=0
out="$(cd "$repo" && bash scripts/check-changelog-assembled.sh --base no-such-ref rel-crate 1.0.0 2>&1)" || rc=$?
if [ "$rc" -eq 2 ] && printf '%s\n' "$out" | grep -qF "does not resolve"; then
  pass_case "helper: unresolvable --base -> exit 2"
else
  fail_case "helper: unresolvable --base: expected exit 2, got ${rc}" "$out"
fi
rc=0
out="$(cd "$repo" && bash scripts/check-changelog-assembled.sh rel-crate 1.0.0 2>&1)" || rc=$?
if [ "$rc" -eq 1 ] && printf '%s\n' "$out" | grep -qF "STRANDED-FRAGMENTS"; then
  pass_case "helper without --base keeps the tag-time verdict -> exit 1, STRANDED-FRAGMENTS"
else
  fail_case "helper without --base: expected exit 1 STRANDED-FRAGMENTS, got ${rc}" "$out"
fi
rc=0
out="$(cd "$repo" && gitc "$repo" checkout -q main && bash scripts/check-changelog-assembled.sh --base main rel-crate 1.0.0 2>&1)" || rc=$?
if [ "$rc" -eq 1 ] && printf '%s\n' "$out" | grep -qF "STRANDED-FRAGMENTS"; then
  pass_case "helper --base at the base's own tip excludes nothing -> exit 1"
else
  fail_case "helper --base at the base tip: expected exit 1, got ${rc}" "$out"
fi

echo
echo "check-pr-changelog-assembled-selftest: ${PASSED} passed, ${FAILED} failed."
[ "$FAILED" -eq 0 ]

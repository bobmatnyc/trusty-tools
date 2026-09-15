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

# run_case <label> <expect-exit> <expect-substring|-> <repo>
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  local base out rc=0
  base="$(git -C "$repo" rev-parse HEAD~1)"
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

echo
echo "check-pr-changelog-assembled-selftest: ${PASSED} passed, ${FAILED} failed."
[ "$FAILED" -eq 0 ]

#!/usr/bin/env bash
#
# is-branch-caused.sh — whose red is it? (#6653)
#
# Why: root CLAUDE.md ("Baseline failures") and tm-workflow.md's
#   "Baseline-Failure Protocol" both say the same thing in prose: before
#   reporting a red gate, establish whether YOUR branch caused it, and prove
#   the "not mine" claim rather than asserting it. The proof named there is a
#   two-step one — an empty `git diff --name-only origin/main...HEAD --
#   <crate>/` when the crate is untouched, and a reproduction on the base
#   branch when it is not. Every engineer re-derives that from prose, and the
#   second half is the half that gets skipped, because standing up a clean base
#   checkout by hand is tedious enough to talk yourself out of.
#
# What: one verdict on stdout and a fixed exit code.
#
#     PRE-EXISTING   exit 0   the crate's diff vs the base is empty, OR the
#                             same gate fails at the base too
#     BRANCH-CAUSED  exit 1   the gate passes at the base and fails here
#     INCONCLUSIVE   exit 2   the base checkout would not build, so the two
#                             runs are not comparable
#
#   The crate name comes from <crate-dir>/Cargo.toml's own `name` field, never
#   from the directory name — they differ in this workspace (see CLAUDE.md's
#   Abbreviations & Aliases table).
#
#   The base run happens in a THROWAWAY git worktree checked out at the base
#   ref, removed by an EXIT trap. The caller's checkout is never touched: no
#   checkout, no stash, no reset. Cargo artifacts for the base run go to a
#   dedicated CARGO_TARGET_DIR so the base build neither collides with nor
#   invalidates the caller's own `target/`.
#
# Usage:
#   bash scripts/is-branch-caused.sh crates/trusty-mpm
#   bash scripts/is-branch-caused.sh crates/trusty-mpm --base origin/main
#
# Note: the base run is a real `cargo test -p <crate> --no-fail-fast` in a cold
#   worktree. It is slow the first time and incremental afterwards.
#
# Test: scripts/is-branch-caused-selftest.sh

set -uo pipefail

VERDICT_PRE_EXISTING=0
VERDICT_BRANCH_CAUSED=1
VERDICT_INCONCLUSIVE=2

CRATE_DIR=""
BASE="origin/main"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      BASE="${2:-}"
      if [[ -z "$BASE" ]]; then
        echo "is-branch-caused.sh: --base needs a ref argument" >&2
        exit "$VERDICT_INCONCLUSIVE"
      fi
      shift 2
      ;;
    -h|--help)
      sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "is-branch-caused.sh: unknown option '$1'" >&2
      exit "$VERDICT_INCONCLUSIVE"
      ;;
    *)
      CRATE_DIR="${1%/}"
      shift
      ;;
  esac
done

if [[ -z "$CRATE_DIR" ]]; then
  echo "is-branch-caused.sh: usage: is-branch-caused.sh <crate-dir> [--base <ref>]" >&2
  exit "$VERDICT_INCONCLUSIVE"
fi

if ! REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  echo "is-branch-caused.sh: not inside a git repository." >&2
  exit "$VERDICT_INCONCLUSIVE"
fi
cd "$REPO_ROOT"

MANIFEST="$CRATE_DIR/Cargo.toml"
if [[ ! -f "$MANIFEST" ]]; then
  echo "is-branch-caused.sh: no Cargo.toml at $MANIFEST" >&2
  exit "$VERDICT_INCONCLUSIVE"
fi

# The crate NAME, from the manifest's [package] name. Directory names differ
# from crate names in this workspace, so reading the manifest is the only
# correct resolution. First `name = "..."` line wins; [package] precedes every
# other table in every manifest here.
CRATE="$(sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)"
if [[ -z "$CRATE" ]]; then
  echo "is-branch-caused.sh: cannot read a package name out of $MANIFEST" >&2
  exit "$VERDICT_INCONCLUSIVE"
fi

if ! git rev-parse --verify --quiet "$BASE" >/dev/null; then
  echo "is-branch-caused.sh: base ref '$BASE' does not resolve. Fetch first." >&2
  exit "$VERDICT_INCONCLUSIVE"
fi

# STEP 1 — the emptiness proof. A crate this branch never touched cannot be the
# cause of its own red, and that is provable without running anything.
CHANGED="$(git diff --name-only "${BASE}...HEAD" -- "$CRATE_DIR/" 2>/dev/null)"
if [[ -z "$CHANGED" ]]; then
  echo "PRE-EXISTING"
  echo "  ${CRATE}: no change under ${CRATE_DIR}/ in ${BASE}...HEAD" >&2
  exit "$VERDICT_PRE_EXISTING"
fi

echo "is-branch-caused.sh: ${CRATE} has $(printf '%s\n' "$CHANGED" | wc -l | tr -d ' ')" \
     "changed path(s) vs ${BASE}; reproducing the gate at the base." >&2

# STEP 2 — run the same gate at the base, in a throwaway worktree.
#
# The tree goes under <repo-root>/.claude/worktrees/, never $TMPDIR: a worktree
# provisioned in a temp directory can be reaped mid-run and silently fails
# unrelated tests (#3955). The `is-branch-caused.` prefix marks it ephemeral so
# nobody mistakes it for a workstream tree.
WORKTREE="$REPO_ROOT/.claude/worktrees/is-branch-caused.$$"
mkdir -p "$REPO_ROOT/.claude/worktrees"

cleanup() {
  # Remove the throwaway tree. `prune` is the fallback for an environment that
  # denies `worktree remove`; it also clears the registry entry either way.
  git -C "$REPO_ROOT" worktree remove --force "$WORKTREE" >/dev/null 2>&1 ||
    git -C "$REPO_ROOT" worktree prune >/dev/null 2>&1 || true
}
trap cleanup EXIT

if ! git worktree add --detach "$WORKTREE" "$BASE" >/dev/null 2>&1; then
  echo "is-branch-caused.sh: cannot create a worktree at $BASE" >&2
  echo "INCONCLUSIVE"
  exit "$VERDICT_INCONCLUSIVE"
fi

# git records the PHYSICAL path; a repo reached through a symlink would
# otherwise leave `worktree remove` matching nothing and the tree leaking.
WORKTREE="$(cd "$WORKTREE" && pwd -P)"

# A dedicated target dir: the base build must not collide with, or invalidate,
# the caller's own `target/`, and reusing one across runs keeps step 2 warm.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR_BASELINE:-$REPO_ROOT/target/branch-caused-baseline}"
export SKIP_UI_BUILD=1

BUILD_LOG="$(mktemp "${TMPDIR:-/tmp}/is-branch-caused-build.XXXXXX")"
TEST_LOG="$(mktemp "${TMPDIR:-/tmp}/is-branch-caused-test.XXXXXX")"

if ! (cd "$WORKTREE" && cargo test -p "$CRATE" --no-run) >"$BUILD_LOG" 2>&1; then
  echo "is-branch-caused.sh: the base checkout does not build ${CRATE}." >&2
  tail -30 "$BUILD_LOG" >&2
  echo "INCONCLUSIVE"
  exit "$VERDICT_INCONCLUSIVE"
fi

(cd "$WORKTREE" && cargo test -p "$CRATE" --no-fail-fast) >"$TEST_LOG" 2>&1
BASE_STATUS=$?

if [[ "$BASE_STATUS" -eq 0 ]]; then
  echo "BRANCH-CAUSED"
  echo "  ${CRATE} passes at ${BASE}; the failure is this branch's." >&2
  exit "$VERDICT_BRANCH_CAUSED"
fi

echo "PRE-EXISTING"
echo "  ${CRATE} fails at ${BASE} too (exit ${BASE_STATUS}); base log: ${TEST_LOG}" >&2
grep -E '^(test result|error|failures:)' "$TEST_LOG" | tail -20 >&2
exit "$VERDICT_PRE_EXISTING"

#!/usr/bin/env bash
#
# is-branch-caused-selftest.sh — fixtures for scripts/is-branch-caused.sh (#6653).
#
# Why: the verdict that matters most is the CHEAP one — "this crate's diff
# against the base is empty, so its red is not mine" — because it is the one a
# reader will act on without re-checking, and the one whose failure mode is
# silent: a script that printed PRE-EXISTING for a crate the branch DID touch
# would launder a real regression into someone else's problem. A gate whose
# pass cannot be shown to be earned is worth nothing, so the empty-diff path is
# pinned here against a synthetic repository, along with the three refusals
# that must never be mistaken for a verdict.
#
# What: four cases in throwaway git repositories under $TMPDIR.
#   1. EMPTY DIFF     — a crate the branch never touched: PRE-EXISTING, exit 0,
#                       and the report names the crate by its Cargo.toml
#                       `name`, not by its directory (they differ here on
#                       purpose — `crates/alpha` ships `alpha-crate`).
#   2. NON-EMPTY DIFF — a crate the branch DID touch must NOT take the
#                       exit-0 shortcut; it must reach the base reproduction.
#   3. NO MANIFEST    — a directory with no Cargo.toml: INCONCLUSIVE, exit 2.
#   4. BAD BASE       — an unresolvable base ref: INCONCLUSIVE, exit 2.
#
# Test: this IS the test. Run directly:
#   bash scripts/is-branch-caused-selftest.sh
#
# Portability: POSIX tools plus git; bash 3.2 (macOS) and bash 5 (Linux CI).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNDER_TEST="$SCRIPT_DIR/is-branch-caused.sh"

FAILURES=0

fail() {
  echo "FAIL: $*" >&2
  FAILURES=$((FAILURES + 1))
}

pass() {
  echo "ok: $*"
}

# Build a synthetic repository:
#   main branch  — crates/alpha (package `alpha-crate`), crates/beta (`beta-crate`)
#   work branch  — edits crates/beta only
# Echoes the repo path.
make_repo() {
  local root
  root="$(mktemp -d "${TMPDIR:-/tmp}/ibc-selftest.XXXXXX")"
  (
    cd "$root" || exit 1
    git init --quiet --initial-branch=main .
    git config user.email selftest@example.com
    git config user.name selftest
    git config commit.gpgsign false

    mkdir -p crates/alpha/src crates/beta/src
    # Directory name and package name deliberately differ.
    printf '[package]\nname = "alpha-crate"\nversion = "0.1.0"\nedition = "2021"\n' \
      >crates/alpha/Cargo.toml
    printf 'pub fn a() {}\n' >crates/alpha/src/lib.rs
    printf '[package]\nname = "beta-crate"\nversion = "0.1.0"\nedition = "2021"\n' \
      >crates/beta/Cargo.toml
    printf 'pub fn b() {}\n' >crates/beta/src/lib.rs

    git add -A
    git commit --quiet -m "base"

    git checkout --quiet -b work
    printf 'pub fn b() { let _ = 1; }\n' >crates/beta/src/lib.rs
    git add -A
    git commit --quiet -m "touch beta only"
  ) || return 1
  printf '%s\n' "$root"
}

REPO="$(make_repo)"
if [[ -z "$REPO" || ! -d "$REPO" ]]; then
  echo "FAIL: could not build the synthetic repository" >&2
  exit 1
fi
trap 'rm -rf "$REPO"' EXIT

# ── 1. EMPTY DIFF ────────────────────────────────────────────────────────
OUT="$(cd "$REPO" && bash "$UNDER_TEST" crates/alpha --base main 2>/tmp/ibc-err.$$)"
CODE=$?
ERR="$(cat "/tmp/ibc-err.$$" 2>/dev/null)"
rm -f "/tmp/ibc-err.$$"

if [[ "$CODE" -ne 0 ]]; then
  fail "empty diff: expected exit 0, got $CODE (stderr: $ERR)"
elif [[ "$OUT" != "PRE-EXISTING" ]]; then
  fail "empty diff: expected 'PRE-EXISTING' on stdout, got '$OUT'"
elif ! printf '%s' "$ERR" | grep -q 'alpha-crate'; then
  fail "empty diff: report must name the Cargo.toml package 'alpha-crate', got: $ERR"
elif printf '%s' "$ERR" | grep -q 'no change under crates/alpha/'; then
  pass "empty diff → PRE-EXISTING (exit 0), crate resolved from Cargo.toml"
else
  fail "empty diff: report must name the crate DIRECTORY it proved empty, got: $ERR"
fi

# ── 2. NON-EMPTY DIFF must not take the shortcut ─────────────────────────
# `crates/beta` DID change, so the script must reach step 2 rather than
# printing PRE-EXISTING off the diff. Step 2 cannot build in this synthetic
# repo (no workspace root, no cargo registry), so the run ends INCONCLUSIVE —
# what is pinned here is that it did NOT stop at step 1.
if command -v git >/dev/null 2>&1; then
  OUT="$(cd "$REPO" && bash "$UNDER_TEST" crates/beta --base main 2>/tmp/ibc-err2.$$)"
  CODE=$?
  ERR="$(cat "/tmp/ibc-err2.$$" 2>/dev/null)"
  rm -f "/tmp/ibc-err2.$$"

  if [[ "$CODE" -eq 0 && "$OUT" == "PRE-EXISTING" ]]; then
    fail "non-empty diff: took the exit-0 shortcut for a crate the branch touched"
  elif ! printf '%s' "$ERR" | grep -q 'changed path'; then
    fail "non-empty diff: expected the step-2 notice on stderr, got: $ERR"
  else
    pass "non-empty diff → reached the base reproduction (verdict '$OUT', exit $CODE)"
  fi

  # The throwaway worktree must be gone, whichever way the run ended. Match the
  # throwaway's own mktemp prefix only — the synthetic repo itself lives under
  # $TMPDIR too, and its main-worktree line would otherwise count as a leak.
  LEFTOVER="$(cd "$REPO" && git worktree list | grep -c 'is-branch-caused\.' || true)"
  if [[ "$LEFTOVER" -gt 0 ]]; then
    fail "non-empty diff: left $LEFTOVER throwaway worktree(s) registered"
  else
    pass "throwaway worktree removed on exit"
  fi
fi

# ── 3. NO MANIFEST ───────────────────────────────────────────────────────
OUT="$(cd "$REPO" && bash "$UNDER_TEST" crates/nonexistent --base main 2>/dev/null)"
CODE=$?
if [[ "$CODE" -ne 2 ]]; then
  fail "missing manifest: expected exit 2, got $CODE (stdout: $OUT)"
else
  pass "missing Cargo.toml → exit 2"
fi

# ── 4. BAD BASE ──────────────────────────────────────────────────────────
OUT="$(cd "$REPO" && bash "$UNDER_TEST" crates/alpha --base no/such/ref 2>/dev/null)"
CODE=$?
if [[ "$CODE" -ne 2 ]]; then
  fail "bad base ref: expected exit 2, got $CODE (stdout: $OUT)"
else
  pass "unresolvable base ref → exit 2"
fi

# ── verdict ──────────────────────────────────────────────────────────────
if [[ "$FAILURES" -gt 0 ]]; then
  echo "is-branch-caused-selftest: $FAILURES case(s) failed." >&2
  exit 1
fi
echo "is-branch-caused-selftest: all cases passed."

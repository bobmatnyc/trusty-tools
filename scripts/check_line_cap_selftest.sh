#!/usr/bin/env bash
#
# check_line_cap_selftest.sh — regression fixtures for the SLOC counter
# shared by scripts/check_line_cap.sh (issues #2489 / #2509).
#
# Why: the SLOC awk heuristic in scripts/lib/sloc_awk.sh has one specific,
#   previously-broken failure mode: a `/*` appearing inside `//`/`///`/`//!`
#   line-comment PROSE (e.g. a doc comment mentioning a path glob like
#   `/api/v1/sessions/*`) was mistaken for a block-comment opener, which
#   silently swallowed the rest of the file as an "unterminated" block
#   comment. There is no bats/shell-test convention elsewhere in scripts/, so
#   this is a minimal, dependency-free self-test runnable directly with bash.
#
# What: runs the shared $SLOC_AWK program (scripts/lib/sloc_awk.sh) against
#   each fixture in scripts/test-data/ and asserts the exact expected SLOC
#   count. Exits non-zero and prints a diff-style report on any mismatch.
#
# Test: this IS the test. Run directly: bash scripts/check_line_cap_selftest.sh
#
# Portability: same constraints as check_line_cap.sh — POSIX tools only,
#   bash 3.2 (macOS) and bash 5 (Linux CI) compatible.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/sloc_awk.sh
. "$SCRIPT_DIR/lib/sloc_awk.sh"

FIXTURE_DIR="$SCRIPT_DIR/test-data"

# fixture<TAB>expected_sloc
# - sloc-normal.rs:            baseline sanity check, no comment tricks.
# - sloc-pathglob-doc.rs:      regression fixture for #2489/#2509 — a `/*`
#                              inside `///`/`//!` doc-comment prose (a path
#                              glob) must NOT swallow the rest of the file.
# - sloc-real-block-comment.rs: genuine /* ... */ block comments (including
#                              multi-line spans) must still be fully excluded.
# - sloc-trailing-comment.rs:  code followed by a trailing `//` comment must
#                              still count the code portion of the line.
CASES="sloc-normal.rs	7
sloc-pathglob-doc.rs	6
sloc-real-block-comment.rs	2
sloc-trailing-comment.rs	4"

fail=0
while IFS="$(printf '\t')" read -r fixture expected; do
  [ -n "$fixture" ] || continue
  path="$FIXTURE_DIR/$fixture"
  if [ ! -f "$path" ]; then
    echo "FAIL: fixture missing: $path" >&2
    fail=1
    continue
  fi
  actual="$(awk "$SLOC_AWK" "$path")"
  if [ "$actual" -eq "$expected" ]; then
    echo "PASS: $fixture -> $actual SLOC (expected $expected)"
  else
    echo "FAIL: $fixture -> $actual SLOC (expected $expected)" >&2
    fail=1
  fi
done <<< "$CASES"

if [ "$fail" -ne 0 ]; then
  echo "check_line_cap_selftest: one or more SLOC-counter regression cases FAILED." >&2
  exit 1
fi

echo "check_line_cap_selftest: all SLOC-counter regression cases passed."
exit 0

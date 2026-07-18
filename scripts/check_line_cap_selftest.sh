#!/usr/bin/env bash
#
# check_line_cap_selftest.sh — regression fixtures for the SLOC counter
# shared by scripts/check_line_cap.sh (issues #2489 / #2509).
#
# Why: the SLOC awk heuristic in scripts/lib/sloc_awk.sh has had two specific,
#   previously-broken failure modes, both pinned here as regressions:
#     - issue #2489/#2509: a `/*` appearing inside `//`/`///`/`//!`
#       line-comment PROSE (e.g. a doc comment mentioning a path glob like
#       `/api/v1/sessions/*`) was mistaken for a block-comment opener, which
#       silently swallowed the rest of the file as an "unterminated" block
#       comment.
#     - issue #2563: a boolean `in_block` flag (rather than a nesting-depth
#       counter) mis-tracked genuine NESTED `/* ... */` block comments — a
#       `*/` that only closed an inner nested comment was mistaken for
#       closing the outer one, causing an OVERCOUNT (comment prose counted
#       as code), which violates the documented never-overcount invariant.
#   There is also one INTENTIONAL leniency case pinned as a fixture (not a
#   bug — see scripts/lib/sloc_awk.sh's header comment): a `/*` inside a
#   string literal with no matching `*/` swallows the rest of the file to
#   EOF, which UNDERCOUNTS real code lines. This is accepted because the
#   counter is designed to never overcount, only (rarely) undercount.
#   There is no bats/shell-test convention elsewhere in scripts/, so this is
#   a minimal, dependency-free self-test runnable directly with bash.
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
# - sloc-nested-block-comment.rs: regression fixture for #2563 — a genuine
#                              NESTED /* /* */ */ block comment (6 comment
#                              lines) must be fully excluded; only the trailing
#                              real code line counts. Pre-fix this overcounted
#                              (the inner `*/` was mistaken for the outer
#                              closer, exposing comment prose as "code").
# - sloc-trailing-comment.rs:  code followed by a trailing `//` comment must
#                              still count the code portion of the line.
# - sloc-string-literal-slash-star.rs: INTENTIONAL leniency case (#2563 item
#                              2, not a bug) — an unmatched `/*` inside a
#                              string literal swallows the rest of the file to
#                              EOF, undercounting 2 genuine code lines. Pinned
#                              here so this known trade-off cannot silently
#                              change (e.g. into an overcount) without a
#                              deliberate selftest update.
CASES="sloc-normal.rs	7
sloc-pathglob-doc.rs	6
sloc-real-block-comment.rs	2
sloc-nested-block-comment.rs	1
sloc-trailing-comment.rs	4
sloc-string-literal-slash-star.rs	2"

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

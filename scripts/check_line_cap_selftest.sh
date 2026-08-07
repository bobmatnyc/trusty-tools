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
#   Issue #5153 added `#[cfg(test)] mod` exclusion to the counter. That change
#   can only fail in one direction that matters — excluding too much, so real
#   production bloat sails through a gate reporting OK — so its fixtures come
#   in two halves: three that must be EXCLUDED, and four that must be COUNTED
#   because the construct is ambiguous or out of scope. Part 2 below then
#   proves the whole gate end to end, because a counter fixture alone cannot
#   show that a 600-SLOC production file still FAILS.
#
# What: two parts.
#   1. Runs the shared $SLOC_AWK program (scripts/lib/sloc_awk.sh) against
#      each fixture in scripts/test-data/ and asserts the exact expected SLOC
#      count.
#   2. Builds throwaway git repos and runs the real scripts/check_line_cap.sh
#      against a 600-SLOC decoy (must FAIL) and a 400-production + 400-inline-
#      test file (must PASS, and the assertion pins that its raw non-blank line
#      count is over the cap, so the pass is the exclusion's doing and not a
#      shrunken fixture).
#   Exits non-zero and prints a diff-style report on any mismatch.
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
#
# #[cfg(test)] exclusion (issue #5153) — MUST BE EXCLUDED:
# - sloc-cfg-test-mod.rs:      the canonical inline `#[cfg(test)] mod tests
#                              { … }`; only the 6 production lines count.
# - sloc-cfg-test-nested-mod.rs: the same, INDENTED inside another module —
#                              the closer is matched at the attribute's own
#                              indent, not at column 0.
# - sloc-cfg-test-brace-skew.rs and -unterminated.rs below are the fail-closed
#                              half of the same matcher.
#
# #[cfg(test)] exclusion — MUST STILL BE COUNTED (fail-closed half):
# - sloc-cfg-test-decl.rs:     `#[cfg(test)] mod tests;` declares a sibling
#                              file, not an inline body. Nothing to exclude;
#                              behaviour unchanged from before #5153.
# - sloc-cfg-test-other-predicate.rs: `all(test, …)`, `any(test, …)` and
#                              `not(test)` are NOT the literal `#[cfg(test)]`
#                              and are counted. `any(test, …)` in particular
#                              really does compile into non-test builds.
# - sloc-cfg-test-non-mod-item.rs: `#[cfg(test)]` on an `fn` or a `use` is
#                              counted — only `mod` bodies are in scope.
# - sloc-cfg-test-brace-skew.rs: an unmatched `{` inside a raw string skews
#                              the module's brace balance. The gate must
#                              COUNT the module rather than guess where it
#                              ends. This is THE fail-open regression: if a
#                              future edit makes this fixture drop to 3, the
#                              counter has started trusting a balance it
#                              cannot verify.
# - sloc-cfg-test-unterminated.rs: a `mod tests {` with no closer at all —
#                              no closer found, nothing excluded.
CASES="sloc-normal.rs	7
sloc-pathglob-doc.rs	6
sloc-real-block-comment.rs	2
sloc-nested-block-comment.rs	1
sloc-trailing-comment.rs	4
sloc-string-literal-slash-star.rs	2
sloc-cfg-test-mod.rs	6
sloc-cfg-test-nested-mod.rs	5
sloc-cfg-test-decl.rs	8
sloc-cfg-test-other-predicate.rs	17
sloc-cfg-test-non-mod-item.rs	9
sloc-cfg-test-brace-skew.rs	11
sloc-cfg-test-unterminated.rs	9"

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

# ===========================================================================
# Part 2 — end-to-end gate cases for the #[cfg(test)] exclusion (issue #5153).
#
# A counter fixture proves what a number is; it cannot prove the GATE still
# rejects a fat production file. These two cases do, against the real
# scripts/check_line_cap.sh in a throwaway repo:
#
#   decoy   600 production SLOC + a small #[cfg(test)] block  -> must FAIL
#   inverse 400 production SLOC + 400 inline test SLOC        -> must PASS,
#           with the raw non-blank line count asserted to be over the cap so
#           the pass is provably the exclusion's doing.
#
# The repo is padded to clear check_line_cap.sh's MIN_RS_FILES scan floor
# (#4618), which otherwise rejects a small fixture before it counts anything.
# ===========================================================================
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
PAD_FILES=520

# new_gate_fixture — print the path to a throwaway git repo holding a copy of
# the gate (so its SCRIPT_DIR-derived repo root resolves inside the fixture)
# and enough filler .rs files to clear the scan floor.
new_gate_fixture() {
  local tmp i
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/linecap.e2e.XXXXXX")"
  git -C "$tmp" init -q
  git -C "$tmp" config user.email selftest@example.invalid
  git -C "$tmp" config user.name "line-cap self-test"
  mkdir -p "$tmp/scripts" "$tmp/pad"
  cp "$REPO_ROOT/scripts/check_line_cap.sh" "$tmp/scripts/"
  cp -R "$REPO_ROOT/scripts/lib" "$tmp/scripts/"
  i=0
  while [ "$i" -lt "$PAD_FILES" ]; do
    printf 'pub fn pad%s() -> u32 {\n    %s\n}\n' "$i" "$i" > "$tmp/pad/pad$i.rs"
    i=$((i + 1))
  done
  echo "$tmp"
}

# emit_prod <count> — <count> lines of one-SLOC-each production code.
emit_prod() {
  awk -v n="$1" 'BEGIN { for (i = 0; i < n; i++) printf "pub fn p%d() -> u32 { %d }\n", i, i }'
}

e2e_fail=0

# ---- decoy: genuine production bloat must still be rejected ----------------
f="$(new_gate_fixture)"
{
  emit_prod 600
  printf '\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(p0(), 0);\n    }\n}\n'
} > "$f/decoy.rs"
git -C "$f" add -A >/dev/null && git -C "$f" commit -qm fixture
decoy_out="$(cd "$f" && bash scripts/check_line_cap.sh 2>&1)" && decoy_rc=0 || decoy_rc=$?
case "$decoy_rc:$decoy_out" in
  0:*)
    echo "SELF-TEST FAIL: 600-SLOC production decoy PASSED the gate — the #[cfg(test)]" >&2
    echo "       stripper is over-matching and the cap is no longer enforced." >&2
    e2e_fail=1 ;;
  *"decoy.rs is 600 SLOC"*)
    echo "  ok  decoy 600 production SLOC + small #[cfg(test)] block -> FAILED as required" ;;
  *)
    echo "SELF-TEST FAIL: decoy was rejected, but not for being 600 SLOC:" >&2
    printf '%s\n' "$decoy_out" | sed 's/^/       /' >&2
    e2e_fail=1 ;;
esac
rm -rf "$f"

# ---- inverse: production under cap, inline tests over it ------------------
f="$(new_gate_fixture)"
{
  emit_prod 400
  printf '\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn big() {\n'
  awk 'BEGIN { for (i = 0; i < 396; i++) printf "        let _v%d = %d;\n", i, i }'
  printf '    }\n}\n'
} > "$f/inverse.rs"
raw_lines="$(grep -c . "$f/inverse.rs")"
counted="$(awk "$SLOC_AWK" "$f/inverse.rs")"
git -C "$f" add -A >/dev/null && git -C "$f" commit -qm fixture
inv_out="$(cd "$f" && bash scripts/check_line_cap.sh 2>&1)" && inv_rc=0 || inv_rc=$?
if [ "$raw_lines" -le 500 ]; then
  echo "SELF-TEST FAIL: the inverse fixture has only ${raw_lines} non-blank lines, so it" >&2
  echo "       would pass the cap even WITHOUT the exclusion. It proves nothing." >&2
  e2e_fail=1
elif [ "$counted" -ne 400 ]; then
  echo "SELF-TEST FAIL: inverse fixture counted ${counted} SLOC, expected 400." >&2
  e2e_fail=1
elif [ "$inv_rc" -ne 0 ]; then
  echo "SELF-TEST FAIL: 400-production + 400-inline-test file was REJECTED:" >&2
  printf '%s\n' "$inv_out" | sed 's/^/       /' >&2
  e2e_fail=1
else
  echo "  ok  inverse ${raw_lines} raw non-blank lines, ${counted} counted -> PASSED as required"
fi
rm -rf "$f"

if [ "$e2e_fail" -ne 0 ]; then
  echo "check_line_cap_selftest: end-to-end gate cases FAILED." >&2
  exit 1
fi

echo "check_line_cap_selftest: end-to-end gate cases passed."
exit 0

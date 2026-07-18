# scripts/lib/sloc_awk.sh — shared SLOC-counting awk program.
#
# Why: scripts/check_line_cap.sh and its self-test (scripts/check_line_cap_selftest.sh)
#   must count lines identically, or the self-test can pass while the real gate
#   still has drifted logic. Extracting the awk program to one sourced file
#   means there is exactly one place this algorithm lives (issues #2489/#2509).
#
# What: defines the `SLOC_AWK` shell variable — an awk program that counts
#   SLOC (source lines of code) in a single file, excluding blank lines,
#   `//`/`///`/`//!` line comments, and `/* ... */` block comments (including
#   multi-line spans AND nested block comments — Rust supports nesting them,
#   e.g. `/* outer /* inner */ still outer */`). A line with code followed by
#   a trailing `//` comment still counts.
#
# Algorithm (issue #2489/#2509 fix, extended by issue #2563 for nesting): the
#   awk program walks each line left-to-right with a single cursor `pos` and a
#   block-comment NESTING DEPTH counter `depth` (carried across lines):
#     - While `depth > 0` (we are inside one or more nested block comments),
#       scan forward from `pos` for whichever of `/*` or `*/` occurs first.
#       A `*/` decrements `depth`; a `/*` increments it (issue #2563 — a plain
#       boolean `in_block` flag cannot tell a nested `/*` from a coincidental
#       one deeper inside prose, so a `*/` that only closes an INNER nested
#       comment was previously mistaken for closing the outer one, causing
#       genuine comment prose after it to be miscounted as code, i.e. an
#       OVERCOUNT — a violation of the documented never-overcount invariant).
#       If neither marker is found, the rest of the line is consumed by the
#       open comment and contributes nothing to this line's code text.
#     - While `depth == 0`, scan forward from `pos` for whichever of `//` or
#       `/*` occurs first:
#         - `//` first (or `/*` absent): the remainder of the line is a line
#           comment; keep only the text before it and stop scanning this
#           line. This is the issue #2489/#2509 fix — a `/*` appearing later
#           in `///`/`//!` doc-comment PROSE (e.g. a path glob like
#           `/api/v1/sessions/*`) is never reached, because the line-comment
#           truncation happens first and removes it along with the rest of
#           the line.
#         - `/*` first: keep the text before it as code, set `depth = 1`, and
#           continue the walk (now in the `depth > 0` branch above) so any
#           further nesting or a same-line close is still handled correctly.
#   Whatever code text survives the walk (i.e. was never inside a comment) is
#   whitespace-stripped; the line counts as 1 SLOC iff any characters remain.
#   This preserves the pre-existing behavior for non-nested block comments and
#   trailing `//` comments while fixing the nested-comment overcount.
#
# Intentional leniency (unchanged from the original heuristic, issue #2563
#   item 2): this awk program has no notion of string/char literals or raw
#   strings, so a `//` or `/*` INSIDE a string literal is still treated as a
#   real comment marker. An unmatched `/*` inside a string literal (i.e. one
#   with no `*/` anywhere later in the string or file) sets `depth = 1` and,
#   because no real `*/` ever appears to close it, SWALLOWS EVERY SUBSEQUENT
#   LINE TO EOF as if it were still inside that block comment — the same
#   failure class as #2489/#2509 via a different trigger. This is a known,
#   accepted trade-off: the counter is designed to err TOWARD LENIENCY (it may
#   UNDERCOUNT real code, e.g. missing genuine code lines after such a string),
#   but per the invariant above it will NEVER overcount. See
#   scripts/test-data/sloc-string-literal-slash-star.rs for a pinned fixture
#   of this exact swallow-to-EOF behavior, and scripts/check_line_cap.sh's
#   header comment for the full leniency note.
#
# Test: scripts/check_line_cap_selftest.sh exercises this against fixtures in
#   scripts/test-data/ (normal file, `/*` inside doc-comment prose, real block
#   comments, nested block comments, trailing `//` after code, `/*` inside a
#   string literal).
SLOC_AWK='
BEGIN { depth = 0; sloc = 0 }
{
  line = $0
  n = length(line)
  pos = 1
  out = ""
  while (pos <= n) {
    if (depth > 0) {
      # Inside one or more nested block comments: find whichever of /* or */
      # occurs first from the current cursor and adjust nesting depth.
      rest = substr(line, pos)
      o = index(rest, "/*")
      c = index(rest, "*/")
      if (c > 0 && (o == 0 || c < o)) {
        depth--
        pos = pos + (c - 1) + 2
      } else if (o > 0) {
        depth++
        pos = pos + (o - 1) + 2
      } else {
        # Neither marker found: the rest of the line stays inside the
        # comment and contributes no code text.
        pos = n + 1
      }
    } else {
      # Not inside a block comment: find whichever of // or /* occurs first.
      rest = substr(line, pos)
      lc = index(rest, "//")
      bc = index(rest, "/*")
      if (lc > 0 && (bc == 0 || lc < bc)) {
        # Line comment starts first (or is the only marker present): keep
        # the code before it and stop scanning this line.
        out = out substr(line, pos, lc - 1)
        pos = n + 1
      } else if (bc > 0) {
        # Block comment starts first: keep the code before it, open one
        # level of nesting, and keep walking from just past the "/*".
        out = out substr(line, pos, bc - 1)
        depth = 1
        pos = pos + (bc - 1) + 2
      } else {
        # No comment markers left on this line: the remainder is all code.
        out = out substr(line, pos)
        pos = n + 1
      }
    }
  }
  gsub(/[[:space:]]/, "", out)
  if (length(out) > 0) sloc++
}
END { print sloc }
'

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
#   multi-line spans). A line with code followed by a trailing `//` comment
#   still counts.
#
# Algorithm (issue #2489/#2509 fix): unlike a naive scanner that looks for the
#   next `/*` unconditionally, this walks the line left-to-right and, at each
#   step, asks "which comment marker comes first: `//` or `/*`?"
#     - If `//` comes first (or `/*` is absent), the rest of the line is a
#       line comment: truncate there and stop. This is the critical fix — a
#       `/*` appearing later in a `///`/`//!` doc-comment's PROSE (e.g. a path
#       glob like `/api/v1/sessions/*`) is never reached, because the line
#       comment truncation happens first and removes it along with the rest
#       of the line.
#     - If `/*` comes first, scan for its matching `*/` as before (handling
#       multiple same-line block comments), then loop again on what remains
#       so a `//` appearing AFTER a same-line block comment is still honored.
#   This preserves the pre-existing behavior for real, code-context block
#   comments (`/* ... */` spanning one or many lines) while fixing the false
#   trap when `/*` only ever appeared inside comment prose.
#
# Intentional leniency (unchanged from the original heuristic): `//` or `/*`
#   inside a string/char literal or raw string can still suppress real code
#   on that line (undercounts). The counter is designed to err TOWARD
#   LENIENCY — it may undercount, but it will never over-count. See
#   scripts/check_line_cap.sh's header comment for the full leniency note.
#
# Test: scripts/check_line_cap_selftest.sh exercises this against fixtures in
#   scripts/test-data/ (normal file, `/*` inside doc-comment prose, real block
#   comments, trailing `//` after code).
SLOC_AWK='
BEGIN { in_block = 0; sloc = 0 }
{
  line = $0
  if (in_block) {
    # Inside a block comment: look for the closing */
    pos = index(line, "*/")
    if (pos > 0) {
      in_block = 0
      line = substr(line, pos + 2)
    } else {
      # Entire line is inside block comment
      next
    }
  }
  # Walk the line, resolving whichever comment marker (// or /*) occurs
  # first at each step. A // that occurs before any /* on the remaining
  # line ALWAYS wins and truncates the line immediately — this is what
  # stops a /* inside line-comment prose from being mistaken for a block
  # opener (issue #2489/#2509).
  while (1) {
    lc = index(line, "//")
    bc = index(line, "/*")
    if (lc == 0 && bc == 0) break
    if (lc > 0 && (bc == 0 || lc < bc)) {
      # Line comment starts first (or is the only marker present): the
      # remainder of the line is comment prose. Truncate and stop scanning.
      line = substr(line, 1, lc - 1)
      break
    }
    # Block comment starts first. Search for its closer only in the portion
    # after the opener, so a stray */ earlier in the line cannot be
    # mistaken for the closer of this opener.
    after_open = substr(line, bc + 2)
    rel_close = index(after_open, "*/")
    if (rel_close > 0) {
      blk_close = bc + 1 + rel_close   # +1 for the two chars of /*
      line = substr(line, 1, bc - 1) substr(line, blk_close + 2)
      # Loop again: there may be more // or /* markers in what remains.
    } else {
      # /* opened but no */ on this line — remainder is a block comment.
      line = substr(line, 1, bc - 1)
      in_block = 1
      break
    }
  }
  # Count the line if anything non-whitespace remains
  gsub(/[[:space:]]/, "", line)
  if (length(line) > 0) sloc++
}
END { print sloc }
'

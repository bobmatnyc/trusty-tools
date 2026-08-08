# scripts/lib/sloc_awk.sh — shared SLOC-counting awk program.
#
# Why: scripts/check_line_cap.sh and its self-test (scripts/check_line_cap_selftest.sh)
#   must count lines identically, or the self-test can pass while the real gate
#   still has drifted logic. Extracting the awk program to one sourced file
#   means there is exactly one place this algorithm lives (issues #2489/#2509).
#
# What: defines the `SLOC_AWK` shell variable — an awk program that counts
#   SLOC (source lines of code) in a single file. It excludes, in two passes:
#     1. blank lines, `//`/`///`/`//!` line comments, and `/* ... */` block
#        comments (including multi-line spans AND nested block comments —
#        Rust supports nesting them, e.g. `/* outer /* inner */ still outer */`).
#        A line with code followed by a trailing `//` comment still counts.
#     2. `#[cfg(test)] mod <name> { … }` regions — inline unit-test modules
#        (issue #5153). See "Pass 2" below for the exact matcher and its
#        deliberately narrow scope.
#
# Pass 1 algorithm (issue #2489/#2509 fix, extended by issue #2563 for
#   nesting): the awk program walks each line left-to-right with a single
#   cursor `pos` and a block-comment NESTING DEPTH counter `depth` (carried
#   across lines):
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
#   The surviving code text is stored per line in `code[]` (original leading
#   whitespace preserved) for pass 2 to inspect. A line counts as 1 SLOC iff
#   any non-whitespace character remains after pass 2 has had its say.
#
# Pass 2 algorithm — `#[cfg(test)]` module exclusion (issue #5153).
#
#   Why: the 500-SLOC production cap was counting inline `#[cfg(test)] mod
#   tests { … }` blocks as production code, so adding tests to a 460-SLOC
#   module forced a mechanical split of its test module into a sibling
#   `_tests.rs` file. That inflates diffs and buys nothing — the production
#   code did not grow. Test code is already capped separately at 3000 SLOC.
#
#   THE FAIL-OPEN RISK IS THE WHOLE DIFFICULTY. An over-matching stripper
#   silently under-counts, and real production bloat then sails through a gate
#   that reports OK. So this matcher is built to FAIL CLOSED: every ambiguous
#   or unrecognised construct is COUNTED AS PRODUCTION. A false cap violation
#   is annoying; a gate that lies is a defect.
#
#   Two INDEPENDENT methods must agree before a region is excluded:
#     (a) Brace balance. Starting at the `mod … {` line, sum `{` minus `}`
#         per line over the comment-stripped text. The region ends at the
#         first line where the running total returns to 0.
#     (b) Indentation anchor. That same line must be exactly the anchor's
#         leading-whitespace prefix followed by `}` and nothing else.
#   If (a) and (b) disagree, if the balance never returns to 0 before EOF, or
#   if the balance goes negative, NOTHING is excluded. Requiring agreement is
#   what makes brace counting safe without a real Rust lexer: the brace
#   counter has no notion of string, char, or raw-string literals, so a `{`
#   or `}` inside `r#"…"#` JSON fixture text or a `'{'` char literal skews
#   the balance — which breaks the equality check and falls back to counting.
#
#   Recognised shape (and ONLY this shape):
#     <indent>#[cfg(test)]
#     <indent>[further #[…] attribute lines]
#     <indent>[pub |pub(crate) |pub(super) |…]mod <ident> {
#     …
#     <indent>}
#   Blank lines between the attribute and the item are tolerated.
#
#   EXPLICITLY NOT EXCLUDED — each of these is counted as production:
#     - `#[cfg(test)] mod tests;` and `#[cfg(test)] #[path = "x.rs"] mod y;`
#       — sibling-file declarations. They are one or two lines and the file
#       they name is classified as a test file by its own path, so there is
#       nothing to gain. Behaviour is unchanged for them.
#     - `#[cfg(test)]` on an `fn`, `impl`, `static`, or `use` item. In this
#       workspace those are ~20 occurrences totalling a few dozen lines,
#       against 1261 `mod` blocks. Matching them means parsing multi-line
#       signatures, `where` clauses, and expression bodies whose opening
#       brace is not on the item's first line — a large fail-open surface for
#       negligible gain. Put test helpers inside the `#[cfg(test)] mod`.
#     - Any `cfg` predicate other than the exact literal `#[cfg(test)]`:
#       `#[cfg(all(test, unix))]`, `#[cfg(any(test, feature = "x"))]`, and
#       `#[cfg(not(test))]` are all counted. `any(test, …)` genuinely compiles
#       into non-test builds; `all(test, …)` does not, but recognising it
#       would mean parsing nested cfg predicates, and there are 10 such
#       blocks in the workspace.
#     - A `mod … {` whose leading whitespace differs from the `#[cfg(test)]`
#       attribute's, or whose line does not end in `{`.
#
#   KNOWN LIMITATION — the counter is line-based, not a parser. A test module
#   whose brace balance is skewed by braces inside string/char/raw-string
#   literals is not excluded at all (it stays counted as production). That is
#   the intended failure direction. There is no construct that causes the
#   opposite — production code silently dropped — because exclusion requires
#   the brace balance to return to exactly 0 on a line that is exactly the
#   anchor indent plus `}`.
#
# Intentional pass-1 leniency (unchanged, issue #2563 item 2): this awk
#   program has no notion of string/char literals or raw strings, so a `//` or
#   `/*` INSIDE a string literal is still treated as a real comment marker. An
#   unmatched `/*` inside a string literal (i.e. one with no `*/` anywhere
#   later in the string or file) sets `depth = 1` and, because no real `*/`
#   ever appears to close it, SWALLOWS EVERY SUBSEQUENT LINE TO EOF as if it
#   were still inside that block comment — the same failure class as
#   #2489/#2509 via a different trigger. This is a known, accepted trade-off
#   for COMMENT stripping specifically: it may UNDERCOUNT real code, but it
#   will never treat comment prose as code. See
#   scripts/test-data/sloc-string-literal-slash-star.rs for a pinned fixture,
#   and scripts/check_line_cap.sh's header for the full note.
#
# Test: scripts/check_line_cap_selftest.sh exercises this against fixtures in
#   scripts/test-data/ (normal file, `/*` inside doc-comment prose, real block
#   comments, nested block comments, trailing `//` after code, `/*` inside a
#   string literal, and the six `#[cfg(test)]` cases from #5153: excluded
#   inline mod, nested/indented mod, sibling-file `mod tests;` declaration,
#   non-literal cfg predicate, brace-skewing raw string, and an unterminated
#   region).
SLOC_AWK='
function rtrim(s) { sub(/[ \t\r]+$/, "", s); return s }
function ltrim(s) { sub(/^[ \t]+/, "", s); return s }
# Leading-whitespace prefix of s ("" when s starts with a non-blank).
function lead_ws(s,   t) {
  t = s
  if (t ~ /^[ \t]*$/) return ""
  sub(/[^ \t].*$/, "", t)
  return t
}
# Net "{" minus "}" on a line. gsub returns the substitution count, which is
# far faster in awk than a per-character walk over ~500k lines.
function brace_delta(s,   a, b, t) {
  t = s; a = gsub(/\{/, "", t)
  t = s; b = gsub(/\}/, "", t)
  return a - b
}
BEGIN { depth = 0; sloc = 0; nlines = 0 }
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
  nlines++
  code[nlines] = out
}
END {
  # ---- Pass 2: mark #[cfg(test)] mod regions for exclusion (issue #5153).
  # Every branch that cannot prove a region falls through WITHOUT marking,
  # so the lines stay counted as production. Fail closed, always.
  i = 1
  while (i <= nlines) {
    if (rtrim(ltrim(code[i])) != "#[cfg(test)]") { i++; continue }
    lead = lead_ws(code[i])

    # Walk to the attributed item, tolerating blank lines and further
    # attribute lines at the SAME indent.
    j = i + 1
    is_mod = 0
    while (j <= nlines) {
      t = rtrim(ltrim(code[j]))
      if (t == "") { j++; continue }
      if (lead_ws(code[j]) != lead) break
      if (t ~ /^#\[.*\]$/) { j++; continue }
      if (t ~ /^(pub([(][a-z]+[)])?[ ]+)?mod[ ]+[A-Za-z_][A-Za-z0-9_]*[ ]*\{$/) is_mod = 1
      break
    }
    if (!is_mod) { i++; continue }

    # Method (a): brace balance from the `mod … {` line.
    # Method (b): the closing line must be exactly <lead>} and nothing else.
    d = 0
    close_at = 0
    for (k = j; k <= nlines; k++) {
      d += brace_delta(code[k])
      if (d < 0) break                                  # malformed -> count it
      if (d == 0) {
        if (rtrim(code[k]) == lead "}") close_at = k    # both methods agree
        break                                           # disagree -> count it
      }
    }
    if (close_at == 0) { i++; continue }                # unterminated -> count it

    for (m = i; m <= close_at; m++) skip[m] = 1
    i = close_at + 1
  }

  # Audit hook: `awk -v emit_skip=1 "$SLOC_AWK" file.rs` prints the excluded
  # line ranges instead of the count, so the pass-2 matcher can be diffed
  # against an independent implementation without duplicating this program.
  if (emit_skip) {
    for (i = 1; i <= nlines; i++) if (i in skip) print i
    exit 0
  }

  for (i = 1; i <= nlines; i++) {
    if (i in skip) continue
    t = code[i]
    gsub(/[[:space:]]/, "", t)
    if (length(t) > 0) sloc++
  }
  print sloc
}
'

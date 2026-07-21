#!/usr/bin/env bash
#
# check_test_pointers.sh — lint `Test:` doc-comment pointers against real
# test names (issue #2458).
#
# Why: the Why/What/Test doc pattern (CLAUDE.md) requires every public item
#   to carry a `/// Test:` (or `//! Test:` at module level) pointer naming the
#   test(s) that exercise it. Those pointers rot silently — a test gets
#   renamed or deleted and the doc comment is never updated, because nothing
#   mechanically checks the cross-reference. Three dangling pointers were
#   caught by human reviewers in a single 48h window (PR #2499: cli.rs cited
#   a `cli_url_unset_is_none` test that was never named that; PR #2513: two
#   pointers in poll/mod.rs cited a test name variant that didn't match the
#   real one). This script turns that into a mechanical CI gate, mirroring
#   scripts/check_line_cap.sh's ratcheted-allowlist shape.
#
# What: scans every tracked `.rs` file (`git ls-files '*.rs'`, minus
#   EXCLUDE_PREFIXES below) for `Test:` doc-comment annotations (a `///`/`//!`
#   line whose stripped content starts with the literal `Test:`, plus any
#   immediately-following `///`/`//!` continuation lines up to the next blank
#   comment line, a new `Why:`/`What:` tag, or the end of the doc block).
#   Within that text, ONLY the LEADING run of backtick-quoted spans
#   (`` `name` ``, separated by whitespace/`,`/`;`/`&`/"and"/"plus", or a
#   `(parenthetical)` remark immediately attached to a citation, e.g. "`name`
#   (macOS-only);") is scanned — extraction stops dead at the first token
#   that is none of those (see candidate_names' doc comment for why: real
#   annotations routinely continue past the test-name list into prose that
#   ALSO backtick-quotes unrelated symbols). Each surviving span is then
#   classified:
#     - module-path-qualified names (`foo::bar::baz_test`) are matched on
#       their FINAL segment (`baz_test`);
#     - a citation whose final segment is a lowercase snake_case identifier
#       (`^[a-z][a-z0-9_]*$`) containing at least one `_` is an EXACT
#       candidate;
#     - a citation whose final segment is otherwise the same shape but
#       contains one or more `*` glob wildcards (`disclaimed_stderr_*`) is a
#       GLOB candidate — resolved by pattern match against the crate's name
#       cache (see name_glob_exists_in_crate) rather than dropped. A glob
#       that matches nothing is exactly as dangling as an exact citation
#       that matches nothing, and is reported the same way.
#     - anything else (bare module/type references (`` `tests` ``,
#       `` `ActivityInfo` `` — no underscore, or CamelCase)) is not a
#       candidate at all and is silently skipped, same as before — this is
#       the deliberate leniency for field/type mentions inside the citation
#       list, not the bug this script was rewritten to close.
#   An UNTERMINATED backtick or parenthetical inside the leading run (a
#   stray `` ` `` or `(` with no matching close) is not "prose" — it is
#   malformed annotation syntax the parser cannot interpret, and is reported
#   as a hard parse-error violation (never allowlist-suppressible) rather
#   than silently truncating the scan.
#   Each surviving EXACT/GLOB candidate is checked with `git grep` (cached
#   per crate — see the name cache section) for a `fn <name>(` (or
#   `fn <name><...>(` for a generic fn) OR a `mod <name>;` / `mod <name> {`
#   definition anywhere under the citing file's own crate (the nearest
#   ancestor directory containing a Cargo.toml; falls back to the whole tree
#   if none is found) — citing a whole test MODULE (e.g.
#   `` `ctrl::tests::pm_task_tests` ``) is an established convention in this
#   codebase, just as valid as citing a single `fn`. No match = a dangling
#   pointer, reported as `file:line: cites <name> (crate <dir>)`. If the
#   final segment DOES resolve but the citation is `::`-qualified with a
#   named path segment before it (e.g. the `piped_native_tests` in
#   `super::super::piped_native_tests::foo`), that segment must ALSO match a
#   real `mod` declared somewhere in the crate (issue #3581 blind spot 3) —
#   otherwise the citation is flagged as a bad-module-path dangling pointer
#   even though its final name happens to exist somewhere unrelated.
#
# PRECISION LIMITS (documented per issue #2458, "pragmatic grep is
#   acceptable"; blind spots closed by issue #3581):
#   - citations must be backtick-quoted (this codebase's universal doc
#     convention for code identifiers); a bare-word citation with no
#     backticks is never linted.
#   - only the LEADING run is linted: a citation list, each entry optionally
#     followed by its own `(parenthetical)` aside, joined by `,`/`;`/`&`/
#     "and"/"plus". A second real test name mentioned after ordinary prose
#     (rather than as part of that joined list) is a false NEGATIVE —
#     missed, not flagged. This trades missed rot for not blocking merges on
#     misparsed prose, the same leans-lenient tradeoff check_line_cap.sh's
#     SLOC counter documents. Genuinely malformed syntax (unterminated
#     backtick/paren) is NOT treated as "prose" — see above, it hard-fails.
#   - glob citations (`foo_*`) are resolved against the crate's name cache
#     (does at least one `fn`/`mod` match the pattern?), not silently
#     dropped — issue #3581's primary finding was that dropping them let two
#     dangling glob-shaped pointers pass as "0 dangling pointers".
#   - the existence check is `fn <name>(` anywhere in the crate — it does
#     NOT verify the function is under `#[test]` / `#[cfg(test)]`. A
#     dangling pointer that happens to collide with a same-named production
#     function will be missed (false negative).
#   - the module-path check (blind spot 3) only verifies that the stated
#     parent module NAME exists somewhere in the crate, not that the cited
#     item is actually nested inside it — true containment checking would
#     require a real Rust module-tree resolver (mod-to-file resolution,
#     `#[path]` attributes, cfg-gated mods), out of reach for a
#     pragmatic-grep gate. A citation whose final name AND whose (wrong,
#     unrelated) parent module both happen to exist somewhere in the crate
#     will be missed (false negative) — narrower than the original bug
#     (which had zero path checking at all), but still a known gap.
#   - "same crate" is filesystem-nearest-Cargo.toml, not `cargo metadata`
#     dependency resolution — a citation naming a test in a *different*
#     crate than the one containing the doc comment will be flagged even if
#     the test genuinely exists elsewhere. This is intentional: Test:
#     pointers are meant to name in-crate tests; cross-crate citations
#     should name the crate in prose (outside the leading backtick run, so
#     it is never scanned) rather than as a bare backtick span.
#
# ALLOWLIST (ratchet, can only shrink — mirrors .line-cap-allowlist.tsv):
#   `.test-pointer-allowlist.tsv`, one `<path><TAB><name>` row per
#   grandfathered dangling pointer — keyed on (path, cited-name), NOT line
#   number. Line numbers drift on every unrelated edit above a grandfathered
#   row, which would otherwise turn "someone touched an unrelated function
#   above this doc comment" into a spurious gate failure (this happened
#   twice in the PR that introduced this gate). A name cited more than once
#   in the same file collapses to a single row. A listed (path, name) that
#   is STILL dangling anywhere in that file is suppressed (OK); a listed
#   (path, name) with NO matching dangling citation left in that file
#   (fixed, renamed, or the doc comment corrected/removed) is a FAIL —
#   remove its entry (`--update` does this automatically). Ordinary
#   `--update` never ADDS a row; `--seed` is the one-time bootstrap that
#   grandfathers every currently-dangling pointer in one shot (refuses to
#   run if the allowlist already has rows, to avoid clobbering hand-curated
#   entries) — used once, in the PR that introduced this gate, to
#   grandfather the large body of pre-existing drift a first-ever run
#   surfaces across a codebase this size. Prefer fixing the doc comment over
#   adding a new row by hand.
#
# Usage:
#   check_test_pointers.sh              # check mode (default); exit 0 = clean
#   check_test_pointers.sh --update      # prune allowlist entries that are no
#                                         # longer dangling (ratchet down only)
#   check_test_pointers.sh --seed        # bootstrap: grandfather every
#                                         # currently-dangling pointer (refuses
#                                         # if the allowlist already has rows)
#   check_test_pointers.sh --self-test   # run the fixture-based self-test
#                                         # suite (see run_self_test below)
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); POSIX tools + git only.

set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"

# ---------------------------------------------------------------------------
# Doc-comment "Test:" block extraction, shared by both scan() and the
# self-test. Reads a single file on stdin-free `awk -f`-style invocation via
# a shell function so it can be reused without spawning a child script.
#
# Output: one row per Test: annotation found: "<line>\t<blob>" where <blob>
# is the concatenated text of the annotation (first line + any continuation
# lines), backtick spans intact.
# ---------------------------------------------------------------------------
EXTRACT_AWK='
function is_doc(line) { return (line ~ /^[ \t]*(\/\/\/|\/\/!)/) }
function strip(line,    s) {
  s = line
  sub(/^[ \t]*(\/\/\/|\/\/!)[ \t]?/, "", s)
  return s
}
BEGIN { collecting = 0; buf = ""; bufstart = "" }
{
  line = $0
  handled = 0
  if (collecting) {
    if (is_doc(line)) {
      content = strip(line)
      if (content ~ /^Test:/) {
        print bufstart "\t" buf
        buf = content; bufstart = NR
        handled = 1
      } else if (content ~ /^(Why|What):/ || content == "") {
        print bufstart "\t" buf
        collecting = 0; buf = ""; bufstart = ""
        handled = 1
      } else {
        buf = buf " " content
        handled = 1
      }
    } else {
      print bufstart "\t" buf
      collecting = 0; buf = ""; bufstart = ""
    }
  }
  if (!handled && !collecting) {
    if (is_doc(line)) {
      content = strip(line)
      if (content ~ /^Test:/) {
        collecting = 1
        buf = content
        bufstart = NR
      }
    }
  }
}
END { if (collecting) print bufstart "\t" buf }
'

extract_test_blocks() {
  awk "$EXTRACT_AWK" "$1"
}

# ---------------------------------------------------------------------------
# candidate_names: given a Test: blob, print one "<KIND>\t<value>\t<parent>"
# row per candidate (or "ERR\t<message>" for a parse error) found — may print
# duplicates; caller de-dupes if it cares. KIND is one of:
#   NAME  — an exact lowercase snake_case final-segment candidate.
#   GLOB  — a final segment shaped like NAME but containing `*` wildcard(s);
#           resolved by pattern match, not exact match, by the caller.
#   ERR   — the annotation could not be interpreted (unterminated backtick or
#           parenthetical). The caller must treat this as a hard failure, not
#           a skip — this is the failure mode issue #3581 exists to close.
# <parent> (NAME/GLOB rows only) is the nearest `::`-qualified path segment
# before the final name that ISN'T `super`/`self`/`crate` and looks like a
# plausible lowercase module identifier — e.g. for
# `super::super::piped_native_tests::foo`, parent = "piped_native_tests".
# Empty when the citation has no such segment (a bare name, or only
# super/self/crate before it). issue #3581 blind spot 3 (reported
# independently against PR #3562's `piped_native_tests` vs the real
# `native_tests`): the pre-fix scan validated only the FINAL segment against
# a crate-wide name set, so a citation with the right final name but a
# WRONG stated module path was invisible — the wrong path was simply never
# looked at. The caller additionally requires `mod <parent>` to exist
# somewhere in the crate (see module_exists_in_crate) when parent is
# non-empty. This is deliberately weaker than true nesting verification
# (it does not confirm the final name is actually DEFINED inside that
# module, only that a module by that name exists somewhere in the crate) —
# implementing real containment checking would require a genuine Rust
# module-tree resolver (mod-to-file resolution, `#[path]` attributes,
# cfg-gated mods), which is out of reach for a pragmatic-grep gate. It still
# catches the concrete bug that motivated it (a module name that doesn't
# exist AT ALL anywhere in the crate) while staying a no-op for ubiquitous
# parent names like `tests` that exist in nearly every file.
#
# Critically, this only walks the LEADING run of the citation list
# immediately after "Test:": backtick-quoted spans, each optionally followed
# by its own `(parenthetical)` aside (e.g. "`name` (macOS-only);"), joined by
# whitespace/`,`/`;`/`&`/"and"/"plus" — and stops at the first token that
# isn't one of those. Real-world annotations routinely continue past the
# test-name list into prose that ALSO backtick-quotes unrelated symbols —
# field names, module names, or the item's own name — e.g. "Test: Used as
# the `pkg_mgr` field of `PlatformInfo` in `tests::hint_*`." (no real test
# cited at all) or "Test: `commit_shas_gated_on_merged_at` exercises
# `merged_at` / `merge_commit_sha`." (only the first span is a test name; the
# rest are fields being exercised). Scanning every backtick in the whole
# blob made those false positives; the leading-run restriction accepts a
# false NEGATIVE instead (a second real test name buried after prose is
# missed) which is the safe direction for a blocking gate, matching
# check_line_cap.sh's stated leniency philosophy.
#
# The `(parenthetical)` allowance exists because a citation is routinely
# annotated inline right where it appears — "`a_test` (macOS-only);
# `b_test` (any OS)." — issue #3581's second blind spot: the old parser
# treated the `(` as "unrecognized prose" and stopped the whole scan there,
# silently dropping every citation after it. A parenthetical attached
# directly to a citation is part of the citation list, not free prose; free
# prose starting with any other token still stops the scan exactly as
# before (see "trailing prose after real name" in the self-test fixture).
# ---------------------------------------------------------------------------
CANDIDATES_AWK='
function find_parent(parts, k,    i, p) {
  for (i = k - 1; i >= 1; i--) {
    p = parts[i]
    if (p == "super" || p == "self" || p == "crate" || p == "") continue
    if (p ~ /^[a-z][a-z0-9_]*$/) return p
    return ""
  }
  return ""
}
function emit(nm,    parts, k, final, parent) {
  k = split(nm, parts, "::")
  final = parts[k]
  if (final == "") return
  parent = find_parent(parts, k)
  if (final ~ /^[a-z][a-z0-9_]*$/ && final ~ /_/) { print "NAME\t" final "\t" parent; return }
  if (final ~ /^[a-z][a-z0-9_*]*$/ && index(final, "*") > 0 && final ~ /_/) {
    print "GLOB\t" final "\t" parent
    return
  }
  # Not a candidate shape at all (bare module/type ref, CamelCase, no
  # underscore) — deliberately skipped, not an error.
}
{
  s = $0
  sub(/^Test:[ \t]*/, "", s)
  n = length(s)
  i = 1
  errflag = 0
  while (i <= n) {
    c = substr(s, i, 1)
    if (c == "`") {
      j = index(substr(s, i + 1), "`")
      if (j == 0) {
        print "ERR\tunterminated backtick citation"
        errflag = 1
        break
      }
      emit(substr(s, i + 1, j - 1))
      i = i + 1 + j
    } else if (c == " " || c == "\t" || c == "," || c == ";" || c == "&") {
      i++
    } else if (c == "(") {
      # Balanced, backtick-aware skip: a parenthetical aside routinely
      # embeds its own backtick-quoted code span whose content may itself
      # contain parens, e.g. "(sanity check ... a fresh `read()`)." or
      # "(called with `.with_count(2)`)." A naive index-of-first-")" search
      # stops at the INNER ")" (inside the backtick span), misreads the
      # leftover backtick as the start of a new, unterminated citation, and
      # false-positives a parse error on perfectly well-formed prose. Track
      # paren depth, but treat backtick spans as opaque (skip them whole —
      # their internal parens never affect depth).
      depth = 1
      i++
      while (i <= n && depth > 0) {
        cc = substr(s, i, 1)
        if (cc == "`") {
          jj = index(substr(s, i + 1), "`")
          if (jj == 0) {
            print "ERR\tunterminated backtick inside parenthetical"
            errflag = 1
            i = n + 1
            depth = 0
            break
          }
          i = i + 1 + jj
        } else if (cc == "(") {
          depth++
          i++
        } else if (cc == ")") {
          depth--
          i++
        } else {
          i++
        }
      }
      if (errflag) break
      if (depth > 0) {
        print "ERR\tunterminated parenthetical after citation"
        errflag = 1
        break
      }
    } else {
      rest = substr(s, i)
      if (rest ~ /^and([ \t]|$)/)       { i += 3 }
      else if (rest ~ /^plus([ \t]|$)/) { i += 4 }
      else { break }
    }
  }
}
'

candidate_names() {
  printf '%s\n' "$1" | awk "$CANDIDATES_AWK"
}

# ---------------------------------------------------------------------------
# crate_root_for: print the nearest ancestor directory (relative to the repo
# root, cwd) containing a Cargo.toml for a given repo-relative file path.
# Falls back to "." (workspace root) if none is found above it.
# ---------------------------------------------------------------------------
crate_root_for() {
  local f="$1" dir
  dir="$(dirname "$f")"
  while [ "$dir" != "." ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/Cargo.toml" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  printf '%s\n' "."
}

# ---------------------------------------------------------------------------
# Per-crate `fn` name cache. Spawning a `git grep` per CITATION does not
# scale (thousands of citations workspace-wide); instead, the first time a
# given crate root is queried in a scan() run, we grep it ONCE for every
# `fn <ident>` occurrence and cache the resulting name set to a temp file.
# Every subsequent lookup for that crate is then a local `grep -x` against
# the cached set — O(crates) subprocess spawns instead of O(citations).
# ---------------------------------------------------------------------------
CRATE_FN_CACHE_DIR=""

crate_cache_file() {
  local crate="$1" key
  key="$(printf '%s' "$crate" | tr -c 'A-Za-z0-9' '_')"
  printf '%s/%s.fns\n' "$CRATE_FN_CACHE_DIR" "$key"
}

# ---------------------------------------------------------------------------
# ensure_crate_cache: build (if not already cached) and print the path to
# the crate root $1's `fn`/`mod` name-cache file. Shared by
# name_exists_in_crate (exact match) and name_glob_exists_in_crate (pattern
# match) so the cache is only ever built once per crate per scan() run.
# ---------------------------------------------------------------------------
ensure_crate_cache() {
  local crate="$1" cache
  cache="$(crate_cache_file "$crate")"
  if [ ! -f "$cache" ]; then
    git grep -ohE '(fn|mod)[[:space:]]+[a-zA-Z_][a-zA-Z0-9_]*[[:space:]]*[(<{;]' -- "${crate}/*.rs" 2>/dev/null \
      | sed -E -e 's/^(fn|mod)[[:space:]]+//' -e 's/[[:space:]]*[(<{;]$//' \
      | sort -u > "$cache" || : > "$cache"
  fi
  printf '%s\n' "$cache"
}

# ---------------------------------------------------------------------------
# name_exists_in_crate: 0 (true) if `fn <name>` (as a whole identifier,
# followed by `(` or `<` for a generic fn) OR `mod <name>` (followed by `;`
# or `{`) is defined anywhere among tracked .rs files under crate root $1.
# Builds/reuses the crate's name cache.
#
# Module citations are an established convention in this codebase (e.g.
# "Test: `ctrl::tests::pm_task_tests` covers ..." citing a whole test
# module, not a single test fn) — a citation naming a real `mod` block is
# just as valid a pointer as one naming a real `fn`.
# ---------------------------------------------------------------------------
name_exists_in_crate() {
  local crate="$1" name="$2" cache
  cache="$(ensure_crate_cache "$crate")"
  grep -qxF "$name" "$cache"
}

# ---------------------------------------------------------------------------
# name_glob_exists_in_crate: 0 (true) if at least one cached `fn`/`mod` name
# in crate root $1 matches glob pattern $2 (`*` = any run of chars, the only
# wildcard this codebase's citations use). issue #3581: a glob citation with
# zero matches is exactly as dangling as an exact citation with zero
# matches — it must be resolved, not silently dropped from the candidate
# list the way the pre-fix parser dropped it.
# ---------------------------------------------------------------------------
name_glob_exists_in_crate() {
  local crate="$1" pattern="$2" cache regex
  cache="$(ensure_crate_cache "$crate")"
  regex="^$(printf '%s' "$pattern" | sed 's/\*/.*/g')$"
  grep -qE -- "$regex" "$cache"
}

# ---------------------------------------------------------------------------
# mod_cache_file / ensure_mod_cache / module_exists_in_crate: a SEPARATE
# cache of `mod <name>` declarations only (unlike ensure_crate_cache's
# combined fn+mod set) — issue #3581 blind spot 3's parent-module check
# must not be satisfied by a same-named `fn`, only a real `mod`.
# ---------------------------------------------------------------------------
mod_cache_file() {
  local crate="$1" key
  key="$(printf '%s' "$crate" | tr -c 'A-Za-z0-9' '_')"
  printf '%s/%s.mods\n' "$CRATE_FN_CACHE_DIR" "$key"
}

ensure_mod_cache() {
  local crate="$1" cache
  cache="$(mod_cache_file "$crate")"
  if [ ! -f "$cache" ]; then
    git grep -ohE 'mod[[:space:]]+[a-zA-Z_][a-zA-Z0-9_]*[[:space:]]*[;{]' -- "${crate}/*.rs" 2>/dev/null \
      | sed -E -e 's/^mod[[:space:]]+//' -e 's/[[:space:]]*[;{]$//' \
      | sort -u > "$cache" || : > "$cache"
  fi
  printf '%s\n' "$cache"
}

# ---------------------------------------------------------------------------
# module_exists_in_crate: 0 (true) if `mod <name>` is declared anywhere
# among tracked .rs files under crate root $1. Used for the parent-module
# check ONLY (see candidate_names' find_parent doc comment) — deliberately
# weaker than true containment (does not confirm the cited item is actually
# nested inside this module), but catches a stated module path that does
# not exist AT ALL anywhere in the crate, which is invisible to the
# final-segment-only check that shipped originally.
# ---------------------------------------------------------------------------
module_exists_in_crate() {
  local crate="$1" name="$2" cache
  cache="$(ensure_mod_cache "$crate")"
  grep -qxF "$name" "$cache"
}

# ---------------------------------------------------------------------------
# EXCLUDE_PREFIXES: repo-relative path prefixes skipped entirely by the
# scan. These are directories whose own Cargo.toml declares a standalone
# (non-member) `[workspace]` table — i.e. they explicitly opt OUT of the
# trusty-tools workspace and its conventions:
#   - crates/trusty-search/tests/benchmark_corpus/synthetic: a synthetic,
#     LLM-authored fixture crate (see its own README: "Not part of the
#     workspace") used purely as non-circular search-benchmark corpus text.
#     It imitates the Why/What/Test doc style closely enough to read as
#     realistic chunking input, so it "cites" fake test names by design —
#     linting it would be grading fiction, not real doc drift.
# ---------------------------------------------------------------------------
is_excluded_path() {
  case "$1" in
    crates/trusty-search/tests/benchmark_corpus/synthetic/*) return 0 ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------------------
# scan: run the full lint over the git repo rooted at cwd. Writes violation
# lines ("<path>\t<line>\t<name>\t<crate>\t<detail>", line kept only for the
# FAIL message's file:line pointer; detail is empty for an ordinary dangling
# name/glob, or "badpath:<parent>" when the final name resolves but the
# cited module path doesn't — see blind spot 3 above) to the file named by
# $1, stale-allowlist-entry lines ("<path>\t<name>") to $2, and hard
# parse-error lines ("<path>\t<line>\t<message>") to $3. All three files are
# truncated first. Returns nothing (caller inspects the files).
#
# Parse errors (unterminated backtick/parenthetical — see candidate_names)
# are NEVER allowlist-suppressible: they represent syntax the parser could
# not interpret at all, not a dangling-but-understood citation, so ratcheting
# them into `.test-pointer-allowlist.tsv` would defeat issue #3581's whole
# point (a lint that can be made to pass by feeding it more nonsense). The
# only fix is correcting the doc comment.
#
# The dangling-citation allowlist is keyed on (path, cited-name) — NOT line
# number. A citation repeated at several lines in the same file collapses to
# one allowlist row (harmless: they're all-or-nothing the same pointer).
# This is deliberately more coarse than line-exact matching: line numbers
# drift on every unrelated edit above a grandfathered row (this PR itself
# needed two manual re-syncs for that reason), which turns "someone edited
# an unrelated function above this doc comment" into a spurious gate
# failure. (path, name) is stable across that churn while still catching
# real regressions — a NEW dangling citation of a name that already has an
# allowlisted citation elsewhere in the same file is still just as
# legitimate to suppress (same rot, same fix), and a genuinely different
# name at any line is a different key so is never accidentally covered by an
# unrelated row.
# ---------------------------------------------------------------------------
scan() {
  local viol_out="$1" stale_out="$2" err_out="$3"
  : > "$viol_out"
  : > "$stale_out"
  : > "$err_out"

  CRATE_FN_CACHE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tpfncache.XXXXXX")"

  local allowlist=".test-pointer-allowlist.tsv"
  local raw
  raw="$(mktemp "${TMPDIR:-/tmp}/tpraw.XXXXXX")"
  : > "$raw"

  git ls-files '*.rs' | while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    is_excluded_path "$f" && continue
    local crate
    crate="$(crate_root_for "$f")"
    extract_test_blocks "$f" | while IFS=$'\t' read -r line blob; do
      [ -n "${line:-}" ] || continue
      local seen_file
      seen_file="$(mktemp "${TMPDIR:-/tmp}/tpseen.XXXXXX")"
      candidate_names "$blob" | while IFS=$'\t' read -r kind name parent; do
        [ -n "$kind" ] || continue
        case "$kind" in
          ERR)
            printf '%s\t%s\t%s\n' "$f" "$line" "$name" >> "$err_out"
            continue
            ;;
        esac
        [ -n "$name" ] || continue
        # De-dupe within a single annotation (same name cited twice in one blob).
        if grep -qxF "${kind}:${name}" "$seen_file" 2>/dev/null; then continue; fi
        printf '%s\n' "${kind}:${name}" >> "$seen_file"
        if [ "$kind" = "GLOB" ]; then
          name_ok=1
          name_glob_exists_in_crate "$crate" "$name" || name_ok=0
        else
          name_ok=1
          name_exists_in_crate "$crate" "$name" || name_ok=0
        fi
        if [ "$name_ok" = "0" ]; then
          printf '%s\t%s\t%s\t%s\t%s\n' "$f" "$line" "$name" "$crate" "" >> "$raw"
        elif [ -n "$parent" ] && ! module_exists_in_crate "$crate" "$parent"; then
          # issue #3581 blind spot 3: the final segment resolves, but the
          # citation's stated module path does not exist anywhere in the
          # crate — the pre-fix scan never looked at the path, only the
          # final name, so this was invisible even though the path is wrong.
          printf '%s\t%s\t%s\t%s\t%s\n' "$f" "$line" "$name" "$crate" "badpath:$parent" >> "$raw"
        fi
      done
      rm -f "$seen_file"
    done
  done

  if [ -f "$allowlist" ]; then
    # Build the (path, name) allowlist key set once.
    local allow_keys
    allow_keys="$(mktemp "${TMPDIR:-/tmp}/tpkeys.XXXXXX")"
    awk -F'\t' '$0 !~ /^#/ && NF>=2 {print $1"\t"$2}' "$allowlist" | sort -u > "$allow_keys"

    # Violations = raw dangling citations whose (path, name) key is NOT
    # allowlisted.
    while IFS=$'\t' read -r p l n c detail; do
      [ -n "${p:-}" ] || continue
      if grep -F -q -x -- "$(printf '%s\t%s' "$p" "$n")" "$allow_keys"; then
        : # allowlisted — suppress
      else
        printf '%s\t%s\t%s\t%s\t%s\n' "$p" "$l" "$n" "$c" "$detail" >> "$viol_out"
      fi
    done < "$raw"

    # Stale = allowlist keys with NO matching raw dangling citation anywhere
    # in that file anymore (fixed, renamed away, or the doc line was
    # dropped) — must be pruned via --update.
    local raw_keys
    raw_keys="$(mktemp "${TMPDIR:-/tmp}/tprawkeys.XXXXXX")"
    awk -F'\t' '{print $1"\t"$3}' "$raw" | sort -u > "$raw_keys"
    while IFS=$'\t' read -r p n; do
      [ -n "${p:-}" ] || continue
      if ! grep -F -q -x -- "$(printf '%s\t%s' "$p" "$n")" "$raw_keys"; then
        printf '%s\t%s\n' "$p" "$n" >> "$stale_out"
      fi
    done < "$allow_keys"
    rm -f "$allow_keys" "$raw_keys"
  else
    cat "$raw" > "$viol_out"
  fi

  rm -f "$raw"
  rm -rf "$CRATE_FN_CACHE_DIR"
  CRATE_FN_CACHE_DIR=""
}

# ---------------------------------------------------------------------------
# prune_stale_from_allowlist: rewrite allowlist file $1 in place, dropping
# every (path, name) row also present in stale-entries file $2 (as produced
# by scan()'s stale_out). Comment lines and malformed rows pass through
# unchanged. Factored out of the `--update` entry-point branch so run_self_test
# can exercise the exact same pruning logic directly against a fixture
# allowlist, without recursively invoking this script (which would resolve
# REPO_ROOT via SCRIPT_DIR back to the real repo, not the fixture — see
# SCRIPT_DIR's definition above).
# ---------------------------------------------------------------------------
prune_stale_from_allowlist() {
  local allowlist="$1" stale_file="$2" new
  new="$(mktemp "${TMPDIR:-/tmp}/tpnew.XXXXXX")"
  awk -F'\t' -v stalefile="$stale_file" '
    BEGIN {
      while ((getline line < stalefile) > 0) { stale[line] = 1 }
    }
    /^#/ { print; next }
    NF < 2 { print; next }
    { key = $1"\t"$2; if (!(key in stale)) print }
  ' "$allowlist" > "$new"
  mv "$new" "$allowlist"
}

# ---------------------------------------------------------------------------
# run_self_test: builds a throwaway git repo with a fixture crate and
# asserts the checker correctly passes a valid pointer and fails a dangling
# one. Mirrors the level of self-verification check_line_cap.sh has (none,
# beyond being exercised by its introducing PR) by giving this script at
# least one mechanical regression guard, since the extraction logic here is
# materially more complex (multi-line blocks, backtick parsing).
# ---------------------------------------------------------------------------
run_self_test() {
  local tmp ok=1
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/tp-selftest.XXXXXX")"
  trap 'rm -rf "$tmp"' RETURN

  mkdir -p "$tmp/crates/fixture/src"
  cat > "$tmp/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/fixture"]
resolver = "2"
EOF
  cat > "$tmp/crates/fixture/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
EOF
  cat > "$tmp/crates/fixture/src/lib.rs" <<'EOF'
/// Why: fixture for check_test_pointers.sh self-test.
/// What: does nothing.
/// Test: `real_test_exists`, `dangling_test_missing`.
pub fn widget() {}

/// Test: covered by prose with no identifiers, and a glob `widget_*` and a
/// module ref `some::module`.
pub fn untested_by_design() {}

/// Test: Used as the `field_name` field of `Widget` in `tests::hint_*`.
pub fn field_style_reference() {}

/// Test: Covered via the `field_style_reference` tool's graceful-error test
/// today; a populated-index test is future work.
pub fn self_referential_prose() {}

/// Test: `real_test_exists` exercises `unrelated_field` / `other_symbol`.
pub fn trailing_prose_after_real_name() {}

/// Test: `tests::module_style_tests` covers this end-to-end (module
/// citation, not a single fn — an established convention).
pub fn module_cited_by_whole_test_module() {}

/// Test: `tests::nonexistent_module_tests` is claimed to cover this but the
/// module was never written.
pub fn dangling_module_citation() {}

/// Test: `glob_matches_real_test_*` resolves to at least one real test below
/// (issue #3581 fix: a glob-shaped citation is resolved by pattern match
/// against the crate's name cache, not silently dropped as a non-candidate).
pub fn glob_resolves_to_real_test() {}

/// Test: `glob_dangling_nothing_matches_*` is claimed but nothing in this
/// crate matches the pattern — must be flagged (issue #3581 blind spot 1:
/// pre-fix, `final !~ /^[a-z][a-z0-9_]*$/` rejected any citation containing
/// `*`, so a glob citation produced zero candidates and was never checked at
/// all, dangling or not).
pub fn glob_dangling_citation() {}

/// Test: `real_test_exists` (smoke coverage); `dangling_after_paren_test`
/// (regression coverage).
///
/// Isolates blind spot 2 with NO glob involved: two plain exact citations,
/// the first (real, passing) immediately followed by a `(parenthetical)`
/// aside, then a second (dangling) citation after `;`. Pre-fix, the `(`
/// broke the leading-run scan right after the first citation, so the
/// dangling second citation was never even reached — this annotation
/// reported 0 violations despite citing a nonexistent test. Post-fix it
/// must report exactly one violation (dangling_after_paren_test), while
/// real_test_exists still correctly passes.
pub fn parenthetical_then_second_citation_dangling() {}

/// Test: `disclaimed_combo_spawn_*` (macOS-only);
/// `disclaimed_combo_native_missing_test` (any OS).
///
/// Mirrors the real-world annotation shape from issue #3581 (PR #3562's
/// stderr_piped.rs / stdout_piped.rs): a glob citation immediately followed
/// by a `(parenthetical)` aside, then a second citation after `;`. Neither
/// name matches a real test. Pre-fix, the `(` broke the leading-run scan
/// before the second citation was ever reached, AND the glob was dropped by
/// the character-class filter — so this annotation produced ZERO
/// candidates and reported "0 dangling pointers" despite both citations
/// being dangling. Post-fix it must report exactly two.
pub fn parenthetical_then_second_citation_both_dangling() {}

/// Test: `unterminated_backtick_citation is malformed doc syntax (no
/// closing backtick) that the parser cannot interpret — issue #3581
/// requires this to fail loudly, not silently pass as zero candidates.
pub fn malformed_unterminated_backtick() {}

/// Test: `real_test_exists` (also exercised indirectly via `helper_call()`).
///
/// Regression for the parenthetical-skip fix itself: the aside embeds its
/// OWN backtick-quoted code span containing parens (`helper_call()`). A
/// naive first-`)`-wins search stops at the INNER `)` (inside the backtick
/// span), misreads the leftover backtick as the start of a new,
/// unterminated citation, and would wrongly report a parse error on this
/// perfectly well-formed annotation. Must resolve cleanly to exactly the
/// one real citation, zero parse errors.
pub fn nested_parens_inside_parenthetical_is_not_a_parse_error() {}

/// Test: `wrong_module_path_tests::real_test_exists` claims real_test_exists
/// lives under a module that was never declared anywhere in this crate.
///
/// Issue #3581 blind spot 3 (found independently against PR #3562's
/// `piped_native_tests` vs the real `native_tests`): the final segment
/// (`real_test_exists`) DOES resolve — it's a real test — but the stated
/// module path (`wrong_module_path_tests`) does not exist anywhere in the
/// crate. Pre-blind-spot-3 fix, only the final segment was ever checked, so
/// this citation passed clean despite naming a module that doesn't exist.
pub fn bad_module_path_citation() {}

/// Test: `tests::real_test_exists` — properly `::`-qualified with a module
/// path that DOES exist, to prove the blind-spot-3 fix doesn't false-positive
/// on correctly-qualified citations.
pub fn correctly_qualified_path_citation() {}

#[cfg(test)]
mod tests {
    #[test]
    fn real_test_exists() {
        assert!(true);
    }

    #[test]
    fn glob_matches_real_test_case_a() {
        assert!(true);
    }

    mod module_style_tests {
        #[test]
        fn covers_it() {
            assert!(true);
        }
    }
}
EOF
  ( cd "$tmp" && git init -q && git add -A && git -c user.email=t@t -c user.name=t commit -q -m fixture )

  local viol stale err rc
  viol="$(mktemp "${TMPDIR:-/tmp}/tpself.viol.XXXXXX")"
  stale="$(mktemp "${TMPDIR:-/tmp}/tpself.stale.XXXXXX")"
  err="$(mktemp "${TMPDIR:-/tmp}/tpself.err.XXXXXX")"
  ( cd "$tmp" && scan "$viol" "$stale" "$err" )

  # Expect exactly seven violations: dangling_test_missing (fn citation),
  # nonexistent_module_tests (mod citation), glob_dangling_nothing_matches_*
  # (dangling glob, issue #3581 blind spot 1, isolated from any parenthetical),
  # dangling_after_paren_test (blind spot 2 isolated from any glob — a plain
  # exact citation after a parenthetical aside), the two dangling citations
  # from the combined glob+parenthetical annotation that mirrors the real
  # PR #3562 shape (disclaimed_combo_spawn_* and
  # disclaimed_combo_native_missing_test), and the blind-spot-3 bad-module-path
  # citation (real_test_exists resolves, but under a module path —
  # wrong_module_path_tests — that doesn't exist). Nothing else — not a
  # bare real_test_exists (only the badpath-qualified one), not
  # glob_matches_real_test_* (glob resolves to glob_matches_real_test_case_a),
  # not correctly_qualified_path_citation's `tests::real_test_exists` (the
  # parent module `tests` DOES exist), not the glob/module-ref/prose-only
  # annotation, not the trailing-prose patterns (field mentions,
  # self-referential tool names, or symbols named after the real leading
  # test name) that a naive whole-blob backtick scan would misfire on, and
  # critically NOT module_style_tests — a real `mod module_style_tests { ... }`
  # citation must resolve, not just `fn` citations (this is exactly the case
  # that shipped broken: module citations only ever passed via the
  # allowlist, never verified for real).
  if [ "$(wc -l < "$viol" | tr -d ' ')" != "7" ]; then
    echo "self-test FAIL: expected exactly 7 violations, got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -q "dangling_test_missing" "$viol"; then
    echo "self-test FAIL: expected violation to cite dangling_test_missing, got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -q "nonexistent_module_tests" "$viol"; then
    echo "self-test FAIL: expected violation to cite nonexistent_module_tests, got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -qF "glob_dangling_nothing_matches_*" "$viol"; then
    echo "self-test FAIL: expected a dangling GLOB violation citing glob_dangling_nothing_matches_* (blind spot 1: glob citations must be resolved, not dropped), got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -q "dangling_after_paren_test" "$viol"; then
    echo "self-test FAIL: expected a dangling NAME violation citing dangling_after_paren_test — a plain exact citation after a parenthetical aside, isolated from any glob (blind spot 2), got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -qF "disclaimed_combo_spawn_*" "$viol"; then
    echo "self-test FAIL: expected a dangling GLOB violation citing disclaimed_combo_spawn_* from the combined glob+parenthetical annotation (blind spots 1+2 together, mirrors PR #3562), got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -q "disclaimed_combo_native_missing_test" "$viol"; then
    echo "self-test FAIL: expected a dangling NAME violation citing disclaimed_combo_native_missing_test — the second citation after a parenthetical aside (blind spot 2: pre-fix the '(' broke the scan before this citation was ever reached), got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -qF "badpath:wrong_module_path_tests" "$viol"; then
    echo "self-test FAIL: expected a badpath violation for bad_module_path_citation — real_test_exists resolves but wrong_module_path_tests is not a real module anywhere in the crate (issue #3581 blind spot 3), got:" >&2
    cat "$viol" >&2
    ok=0
  elif grep -qE 'field_name|Widget|hint_|self_referential|unrelated_field|other_symbol|module_style_tests|glob_matches_real_test_\*' "$viol"; then
    echo "self-test FAIL: a trailing-prose symbol, the valid module_style_tests citation, or the resolvable glob_matches_real_test_* was incorrectly flagged as dangling:" >&2
    cat "$viol" >&2
    ok=0
  elif grep "real_test_exists" "$viol" | grep -qv "badpath:"; then
    echo "self-test FAIL: real_test_exists was flagged as dangling outside the blind-spot-3 badpath case — either a real citation regressed, or correctly_qualified_path_citation's tests::real_test_exists (valid parent module) was wrongly flagged:" >&2
    cat "$viol" >&2
    ok=0
  elif [ "$(wc -l < "$err" | tr -d ' ')" != "1" ] || ! grep -q "unterminated backtick" "$err"; then
    echo "self-test FAIL: expected exactly 1 parse-error row citing the unterminated backtick in malformed_unterminated_backtick's annotation (issue #3581: must fail loudly, not silently pass) — and NOT the well-formed nested-parens-inside-parenthetical annotation (\`helper_call()\` inside a \`(...)\` aside), got:" >&2
    cat "$err" >&2
    ok=0
  fi
  rm -f "$viol" "$stale" "$err"

  # --- allowlist ratchet: both properties, keyed on (path, name) only -----
  # 1. A still-genuinely-dangling citation that IS allowlisted must be
  #    suppressed from violations, while an unrelated, non-allowlisted
  #    citation in the SAME file (nonexistent_module_tests) must still be
  #    flagged — proving suppression is scoped to the exact (path, name)
  #    key, not a blanket per-file pass.
  # 2. An allowlisted (path, name) row whose name is no longer among the
  #    file's dangling citations (phantom_fixed_pointer — never cited at
  #    all, standing in for "the doc comment was corrected") must be
  #    reported stale, and `--update` must prune exactly that row while
  #    leaving the still-valid dangling_test_missing row untouched.
  local allowlist_path="$tmp/.test-pointer-allowlist.tsv"
  {
    printf 'crates/fixture/src/lib.rs\tdangling_test_missing\n'
    printf 'crates/fixture/src/lib.rs\tphantom_fixed_pointer\n'
  } > "$allowlist_path"

  local viol2 stale2 err2
  viol2="$(mktemp "${TMPDIR:-/tmp}/tpself.viol2.XXXXXX")"
  stale2="$(mktemp "${TMPDIR:-/tmp}/tpself.stale2.XXXXXX")"
  err2="$(mktemp "${TMPDIR:-/tmp}/tpself.err2.XXXXXX")"
  ( cd "$tmp" && scan "$viol2" "$stale2" "$err2" )

  if grep -q "dangling_test_missing" "$viol2"; then
    echo "self-test FAIL: dangling_test_missing is allowlisted but was still reported as a violation:" >&2
    cat "$viol2" >&2
    ok=0
  elif ! grep -q "nonexistent_module_tests" "$viol2"; then
    echo "self-test FAIL: nonexistent_module_tests is NOT allowlisted but was suppressed (allowlist not scoped to (path,name)):" >&2
    cat "$viol2" >&2
    ok=0
  elif [ "$(wc -l < "$stale2" | tr -d ' ')" != "1" ] || ! grep -q "phantom_fixed_pointer" "$stale2"; then
    echo "self-test FAIL: expected exactly 1 stale entry (phantom_fixed_pointer), got:" >&2
    cat "$stale2" >&2
    ok=0
  fi

  if [ "$ok" -eq 1 ]; then
    # Feed scan()'s own stale2 output straight into the real pruning
    # function — an end-to-end exercise of the exact code path --update
    # runs, not a hand-rolled duplicate of its expected input.
    prune_stale_from_allowlist "$allowlist_path" "$stale2"
    if grep -q "phantom_fixed_pointer" "$allowlist_path"; then
      echo "self-test FAIL: --update did not prune the stale phantom_fixed_pointer row:" >&2
      cat "$allowlist_path" >&2
      ok=0
    elif ! grep -q "dangling_test_missing" "$allowlist_path"; then
      echo "self-test FAIL: --update incorrectly pruned the still-valid dangling_test_missing row:" >&2
      cat "$allowlist_path" >&2
      ok=0
    fi
  fi
  rm -f "$viol2" "$stale2" "$err2"

  if [ "$ok" -eq 1 ]; then
    echo "check_test_pointers self-test: OK (valid pointer passes, dangling pointer caught, module citations resolve, prose-only/module-ref citations ignored, glob citations resolved by pattern match, parenthetical asides no longer break multi-citation scanning, unterminated backticks fail loudly, allowlist ratchet suppresses/prunes correctly by (path,name))."
    return 0
  fi
  return 1
}

# ===========================================================================
# Entry point
# ===========================================================================
MODE="check"
for arg in "$@"; do
  case "$arg" in
    --update) MODE="update" ;;
    --seed) MODE="seed" ;;
    --self-test) MODE="self-test" ;;
    -h|--help)
      grep '^#' "$SCRIPT_PATH" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_test_pointers: unknown argument: $arg" >&2
      echo "usage: check_test_pointers.sh [--update | --seed | --self-test]" >&2
      exit 2
      ;;
  esac
done

if [ "$MODE" = "self-test" ]; then
  run_self_test
  exit $?
fi

REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"
ALLOWLIST="$REPO_ROOT/.test-pointer-allowlist.tsv"

VIOL="$(mktemp "${TMPDIR:-/tmp}/tpviol.XXXXXX")"
STALE="$(mktemp "${TMPDIR:-/tmp}/tpstale.XXXXXX")"
ERR="$(mktemp "${TMPDIR:-/tmp}/tperr.XXXXXX")"
trap 'rm -f "$VIOL" "$STALE" "$ERR"' EXIT

scan "$VIOL" "$STALE" "$ERR"

# Parse errors (unterminated backtick/parenthetical — see candidate_names)
# are a hard, unconditional, non-allowlistable failure in EVERY mode,
# including --seed and --update: issue #3581 exists because "the parser
# couldn't interpret this so it quietly produced nothing" was allowed to
# read as success. Fail loudly here before any mode-specific logic runs.
if [ -s "$ERR" ]; then
  while IFS=$'\t' read -r p l m; do
    [ -n "$p" ] || continue
    echo "FAIL: ${p}:${l}: Test: annotation could not be parsed — ${m}. Fix the doc comment; this is never allowlist-suppressible." >&2
  done < "$ERR"
  echo "test-pointers: $(wc -l < "$ERR" | tr -d ' ') unparseable Test: annotation(s) — FAILED." >&2
  exit 1
fi

if [ "$MODE" = "seed" ]; then
  # Bulk-grandfather every CURRENT dangling pointer (mirrors
  # check_line_cap.sh --seed: the one-time bootstrap for an existing
  # codebase that has never been mechanically checked before). Never used
  # for ordinary maintenance — new dangling pointers introduced after the
  # gate lands must be fixed, not seeded. Refuses to run if the allowlist
  # already has grandfathered rows, to avoid accidentally re-seeding over
  # hand-curated entries; use --update to prune, or edit the TSV directly.
  existing_rows=0
  if [ -f "$ALLOWLIST" ]; then
    existing_rows="$(awk -F'\t' '$0 !~ /^#/ && NF>=2' "$ALLOWLIST" | wc -l | tr -d ' ')"
  fi
  if [ "${existing_rows:-0}" -gt 0 ]; then
    echo "check_test_pointers --seed: refusing — $ALLOWLIST already has ${existing_rows} row(s). Remove it first (or edit by hand) if you really intend to re-seed." >&2
    exit 1
  fi
  {
    echo "# .test-pointer-allowlist.tsv — grandfathered dangling Test: doc-comment"
    echo "# pointers (issue #2458)."
    echo "# Format: <relative/path><TAB><cited-test-name>  (one row per grandfathered"
    echo "# dangling pointer). Keyed on (path, name) — NOT line number: a repeated"
    echo "# citation of the same name at several lines in one file collapses to a"
    echo "# single row, and unrelated edits that shift line numbers above a row never"
    echo "# spuriously fail the gate (see scan()'s doc comment in the script)."
    echo "# A row is grandfathered ONLY when fixing it genuinely requires writing a"
    echo "# new test first — prefer fixing the doc comment (correct the name, or drop"
    echo "# the reference) over adding an entry here."
    echo "# Ratchet: entries may only be REMOVED once the pointer is fixed (or the"
    echo "# named test is written); the gate FAILS on any entry that is no longer"
    echo "# dangling, forcing prompt removal. Prune with: scripts/check_test_pointers.sh --update"
    echo "#"
    echo "# Seeded $(date -u +%Y-%m-%d) by --seed: first-ever run of this gate found"
    echo "# this many pre-existing dangling pointers across a workspace where the"
    echo "# Test: convention was never mechanically checked before. New pointers"
    echo "# introduced from this point on are NOT grandfathered — fix them for real."
    awk -F'\t' '{print $1"\t"$3}' "$VIOL" | sort -u
  } > "$ALLOWLIST"
  seeded="$(awk -F'\t' '{print $1"\t"$3}' "$VIOL" | sort -u | wc -l | tr -d ' ')"
  echo "check_test_pointers --seed: wrote $ALLOWLIST with ${seeded} grandfathered row(s)."
  exit 0
fi

if [ "$MODE" = "update" ]; then
  if [ ! -f "$ALLOWLIST" ] || [ ! -s "$STALE" ]; then
    echo "check_test_pointers --update: no stale allowlist entries to prune."
    exit 0
  fi
  prune_stale_from_allowlist "$ALLOWLIST" "$STALE"
  pruned="$(wc -l < "$STALE" | tr -d ' ')"
  echo "check_test_pointers --update: pruned ${pruned} stale entr$([ "$pruned" = "1" ] && echo y || echo ies) from $ALLOWLIST."
  exit 0
fi

# ----- check mode -----
violations=0
if [ -s "$VIOL" ]; then
  while IFS=$'\t' read -r p l n c detail; do
    [ -n "$p" ] || continue
    case "$detail" in
      badpath:*)
        parent="${detail#badpath:}"
        echo "FAIL: ${p}:${l}: Test: pointer cites \`${n}\` under module path \`${parent}\` — \`${n}\` exists somewhere in crate \`${c}\`, but no \`mod ${parent}\` does (issue #3581: the stated module path was never verified)." >&2
        ;;
      *)
        echo "FAIL: ${p}:${l}: Test: pointer cites \`${n}\` — no \`fn ${n}(\` or \`mod ${n}\` found in crate \`${c}\`." >&2
        ;;
    esac
    violations=$((violations + 1))
  done < "$VIOL"
fi

stale=0
if [ -s "$STALE" ]; then
  while IFS=$'\t' read -r p n; do
    [ -n "$p" ] || continue
    echo "FAIL: ${p}: \`${n}\` is allowlisted in .test-pointer-allowlist.tsv but is no longer dangling anywhere in this file — remove its entry (run --update)." >&2
    stale=$((stale + 1))
  done < "$STALE"
fi

total=$((violations + stale))
if [ "$total" -gt 0 ]; then
  echo "test-pointers: ${violations} dangling pointer(s), ${stale} stale allowlist entr$([ "$stale" = "1" ] && echo y || echo ies) — FAILED." >&2
  exit 1
fi

echo "test-pointers: 0 dangling pointers — OK."
exit 0

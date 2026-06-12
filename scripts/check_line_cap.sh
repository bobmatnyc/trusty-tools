#!/usr/bin/env bash
#
# check_line_cap.sh — ratcheted SLOC file-size cap enforcement (issue #610).
#
# Why: the 500-SLOC file cap documented in CLAUDE.md had zero mechanical
#   enforcement, so source files silently grew past it under feature pressure
#   (e.g. trusty-search/src/service/server.rs reached 5,403 lines). Advice
#   without a gate loses. This script turns the cap into a CI/pre-commit gate
#   whose grandfather allowlist can only SHRINK — no new oversized files, and
#   existing oversized files may never grow.
#
# DUAL-CAP RULES (issue #1131):
#   Production source files   → PROD_CAP = 500 SLOC
#   Test / benchmark files    → TEST_CAP = 1500 SLOC
#
#   A file is classified as a test/benchmark file when ANY of these match:
#     - basename is exactly `tests.rs`
#     - basename ends with `_test.rs` or `_tests.rs`
#     - path contains a `/tests/` directory segment
#       (covers both `crates/*/tests/*.rs` integration tests and
#        any `src/**/tests/*.rs` inline test modules)
#     - path contains a `/benches/` directory segment
#   All other tracked `.rs` files are production files capped at 500 SLOC.
#
# What: scans every tracked `.rs` file (`git ls-files '*.rs'`) and enforces:
#   - SLOC <= applicable cap, not allowlisted                    -> OK
#   - SLOC >  applicable cap, not allowlisted                    -> FAIL  (new oversized file)
#   - allowlisted, current SLOC > recorded budget                -> FAIL  (grew beyond frozen budget)
#   - allowlisted, current SLOC <= applicable cap                -> FAIL  (now under cap; drop the entry)
#   - allowlisted, applicable_cap < current SLOC <= budget       -> OK    (grandfathered, not growing)
#   Exit non-zero on any FAIL; exit 0 when clean. Prints a one-line summary.
#
#   --update     regenerates the allowlist but only SAFELY: it may LOWER an
#                existing budget or REMOVE entries that dropped <= applicable cap.
#                It REFUSES to raise a budget or add a brand-new > cap file
#                unless --seed or --force-add is also passed.
#   --seed       initial seeding: allowed to add brand-new entries. Implies update.
#   --force-add  like --update but permits adding new > cap files / raising
#                budgets (escape hatch; use sparingly, e.g. an unavoidable bump).
#
# SLOC definition — a line is counted ONLY when it contains non-whitespace
#   source code after all comment matter is stripped. Excluded:
#     - blank / whitespace-only lines
#     - lines consisting entirely of // line comments (including /// and //!)
#     - lines consisting entirely of /* ... */ block comments (including /**/)
#     - lines that are inside an open /* ... */ block comment
#   A line that has code followed by a trailing // comment COUNTS (it has code).
#   A line inside a multi-line /* */ block does NOT count.
#
# Lenient-heuristic note: the SLOC counter is a pragmatic awk heuristic.
#   Edge cases where // or /* appear inside a string literal, char literal, or
#   raw string (r#"..."#) may be miscounted. The counter is designed to err
#   TOWARD LENIENCY — it may undercount code lines (treating code as comments),
#   but it will NEVER over-count (treating comments as code). This means the
#   gate may pass a file with marginally more real SLOC than the cap, but it
#   will NEVER falsely fail a legitimate file. Pathological cases (e.g. a raw
#   string containing /*) can be noted as exceptions in a code comment.
#
# Test: exercised in the PR that introduced SLOC counting (clean tree exits 0;
#   a production file with 600 SLOC fails; 600 SLOC in a test path passes;
#   1600 SLOC in a test path fails). The logic is pure SLOC counting against
#   the committed allowlist; no unit-test harness.
#
# Portability: works on bash 3.2 (macOS system bash) and bash 5 (Linux CI).
#   Uses POSIX tools only — `git`, `sort`, `awk`. No associative arrays,
#   no bash-4 features, no extra dependencies.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
PROD_CAP=500
TEST_CAP=1500

# Resolve repo root so the script works from any cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
ALLOWLIST="$REPO_ROOT/.line-cap-allowlist.tsv"

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# Mode parsing
# ---------------------------------------------------------------------------
MODE="check"      # check | update
ALLOW_GROW=0      # may add new >cap files / raise budgets (--seed or --force-add)
for arg in "$@"; do
  case "$arg" in
    --update)    MODE="update" ;;
    --seed)      MODE="update"; ALLOW_GROW=1 ;;
    --force-add) MODE="update"; ALLOW_GROW=1 ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_line_cap: unknown argument: $arg" >&2
      echo "usage: check_line_cap.sh [--update | --seed | --force-add]" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# SLOC counter: shared awk program that counts code lines (SLOC) in one file.
#
# Algorithm:
#   - track `in_block` state for /* ... */ spans
#   - when in_block, look for */ to close it; don't count any part of the block
#   - on a non-block line, strip a /* ... */ that opens and closes on the same
#     line (repeat to handle multiple same-line blocks), then check for a
#     remaining // that kills the rest of the line; if what's left has
#     non-whitespace, count it.
#
# Intentional leniency: // or /* inside a string literal will suppress the
# remainder of that line (undercounts code). Raw strings (r#"..."#) containing
# /* will open an apparent block comment (undercounts). Both errors lean toward
# leniency (lower SLOC count), so the gate never falsely fails a real file.
# ---------------------------------------------------------------------------
SLOC_AWK='
BEGIN { in_block = 0; sloc = 0 }
{
  line = $0
  if (in_block) {
    # Inside a block comment: look for the closing */
    pos = index(line, "*/")
    if (pos > 0) {
      # Remainder after */ may have code — fall through to the while loop
      # below which will re-scan it for further /* ... */ pairs.
      in_block = 0
      line = substr(line, pos + 2)
    } else {
      # Entire line is inside block comment
      next
    }
  }
  # Strip complete /* ... */ pairs on the same line (may be multiple).
  # Guard: a stray */ that appears BEFORE the first /* (e.g. `foo */ bar /* baz`)
  # must not be mistaken for the closer of the opener.  We find the closing */
  # by searching only in the substring that starts AFTER the opener (two chars),
  # so a pre-existing stray */ is invisible to that search.
  while (1) {
    blk_open = index(line, "/*")
    if (blk_open == 0) break
    # Search for */ only in the portion after the opener to avoid matching a
    # stray */ that appears earlier in the line.
    after_open = substr(line, blk_open + 2)
    rel_close = index(after_open, "*/")
    if (rel_close > 0) {
      # rel_close is 1-based within after_open; absolute position in line:
      blk_close = blk_open + 1 + rel_close   # +1 for the two chars of /*
      line = substr(line, 1, blk_open - 1) substr(line, blk_close + 2)
    } else {
      # /* opened but no */ on this line — remainder is a block comment
      line = substr(line, 1, blk_open - 1)
      in_block = 1
      break
    }
  }
  # Strip trailing line comment (// ... to end of line)
  pos = index(line, "//")
  if (pos > 0) {
    line = substr(line, 1, pos - 1)
  }
  # Count the line if anything non-whitespace remains
  gsub(/[[:space:]]/, "", line)
  if (length(line) > 0) sloc++
}
END { print sloc }
'

# ---------------------------------------------------------------------------
# cap_for_path: print the applicable SLOC cap for a given repo-relative path.
#
# A file is a test/benchmark file when any of these match:
#   - basename is `tests.rs`
#   - basename ends with `_test.rs` or `_tests.rs`
#   - path contains a `/tests/` directory segment
#   - path contains a `/benches/` directory segment
# All other files are production files.
#
# Implementation uses only shell parameter expansion (no external commands)
# so it works on bash 3.2 (macOS) and bash 5 (Linux) without relying on
# `basename` being in PATH — which may not be the case in all CI environments.
# ---------------------------------------------------------------------------
cap_for_path() {
  local path="$1"
  # Extract basename using parameter expansion: strip leading directory portion.
  local base="${path##*/}"
  # Match test/benchmark patterns
  case "$base" in
    tests.rs|*_test.rs|*_tests.rs)
      echo "$TEST_CAP"; return ;;
  esac
  case "$path" in
    */tests/*|*/benches/*)
      echo "$TEST_CAP"; return ;;
  esac
  echo "$PROD_CAP"
}

# ---------------------------------------------------------------------------
# Build a current snapshot: "<sloc>\t<path>" for every tracked .rs file that
# still exists in the working tree. Computed once, reused by both modes.
# ---------------------------------------------------------------------------
CURRENT="$(mktemp "${TMPDIR:-/tmp}/linecap.cur.XXXXXX")"
trap 'rm -f "$CURRENT"' EXIT

git ls-files '*.rs' | while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  n="$(awk "$SLOC_AWK" "$f")"
  printf '%s\t%s\n' "$n" "$f"
done > "$CURRENT"

# Ensure the allowlist file path resolves even when absent (awk -f handles it).
ALLOWLIST_READ="$ALLOWLIST"
[ -f "$ALLOWLIST_READ" ] || ALLOWLIST_READ="/dev/null"

# ===========================================================================
# UPDATE MODE  (--update / --seed / --force-add)
# ===========================================================================
if [ "$MODE" = "update" ]; then
  NEWLIST="$(mktemp "${TMPDIR:-/tmp}/linecap.new.XXXXXX")"
  ERRFILE="$(mktemp "${TMPDIR:-/tmp}/linecap.err.XXXXXX")"
  # shellcheck disable=SC2064
  trap 'rm -f "$CURRENT" "$NEWLIST" "$NEWLIST.body" "$ERRFILE"' EXIT

  # Build a per-path cap map: "<path>\t<cap>" for all tracked .rs files.
  # This is written to a temp file so the awk merge step can read it.
  CAPMAP="$(mktemp "${TMPDIR:-/tmp}/linecap.cap.XXXXXX")"
  # shellcheck disable=SC2064
  trap 'rm -f "$CURRENT" "$NEWLIST" "$NEWLIST.body" "$ERRFILE" "$CAPMAP"' EXIT

  git ls-files '*.rs' | while IFS= read -r f; do
    [ -n "$f" ] || continue
    cap="$(cap_for_path "$f")"
    printf '%s\t%s\n' "$f" "$cap"
  done > "$CAPMAP"

  # Tag each input stream so awk distinguishes them even when the allowlist is
  # empty (a plain FNR==NR split breaks on an empty first file):
  #   Allowlist rows: "A<TAB>path<TAB>budget"
  #   Snapshot rows:  "C<TAB>sloc<TAB>path"
  #   Cap-map rows:   "P<TAB>path<TAB>cap"
  #
  # IMPORTANT: P rows must come BEFORE C rows so that the file_cap[] array
  # is fully populated when C rows are processed (awk is single-pass).
  {
    awk 'BEGIN{FS=OFS="\t"} $0 !~ /^#/ && NF>=2 {print "A", $1, $2}' "$ALLOWLIST_READ"
    awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "P", $1, $2}' "$CAPMAP"
    awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "C", $1, $2}' "$CURRENT"
  } | awk -v allow_grow="$ALLOW_GROW" -v errfile="$ERRFILE" \
         -v prod_cap="$PROD_CAP" -v test_cap="$TEST_CAP" '
    BEGIN { FS = OFS = "\t" }
    # ----- allowlist rows: A <path> <budget> -----
    $1 == "A" { old[$2] = $3; next }
    # ----- cap-map rows: P <path> <cap> -----
    $1 == "P" { file_cap[$2] = $3 + 0; next }
    # ----- snapshot rows:  C <sloc> <path> -----
    {
      n = $2 + 0
      path = $3
      cap = (path in file_cap) ? file_cap[path] : prod_cap
      cap_label = (cap == prod_cap) ? (prod_cap " prod cap") : (test_cap " test cap")
      if (n <= cap) next                 # under applicable cap -> drop from list
      if (path in old) {
        if (n > old[path] + 0) {
          if (allow_grow == 1) { keep[path] = n }
          else {
            printf "REFUSE: %s grew to %d SLOC (frozen budget %s; %s). Split it before updating the allowlist.\n", path, n, old[path], cap_label > errfile
            err = 1
          }
        } else {
          keep[path] = n                 # ratchet down (n <= old budget)
        }
      } else {
        if (allow_grow == 1) { keep[path] = n }
        else {
          printf "REFUSE: %s is a new oversized file (%d SLOC > %s). Split it; do not add it to the allowlist.\n", path, n, cap_label > errfile
          err = 1
        }
      }
    }
    END {
      if (err) { exit 3 }
      for (p in keep) printf "%s\t%s\n", p, keep[p]
    }
  ' > "$NEWLIST.body" || {
    rc=$?
    if [ "$rc" -eq 3 ]; then
      cat "$ERRFILE" >&2
      echo "check_line_cap --update aborted: unresolved violations above." >&2
      echo "Split the offending file(s), or pass --seed/--force-add only for an intentional initial seed / unavoidable bump." >&2
      exit 1
    fi
    echo "check_line_cap --update: awk failed (rc=$rc)." >&2
    exit "$rc"
  }

  count="$(awk 'END{print NR}' "$NEWLIST.body")"
  {
    echo "# .line-cap-allowlist.tsv — grandfathered files over the SLOC cap (issue #610)."
    echo "# Format: <relative/path><TAB><budget>  (budget = frozen max SLOC count; code lines only)."
    echo "# Dual cap: production source = ${PROD_CAP} SLOC; test/benchmark files = ${TEST_CAP} SLOC."
    echo "# Test/benchmark = basename is tests.rs, ends with _test.rs or _tests.rs,"
    echo "#   or path contains /tests/ or /benches/ segment. All others = production."
    echo "# SLOC excludes blank lines, // line comments, /// doc comments, //! inner-doc comments,"
    echo "# and /* ... */ block comments (including multi-line spans). Trailing-comment lines count."
    echo "# Ratchet: budgets may only DECREASE; when a file drops <= its applicable cap, remove it."
    echo "# Regenerate with: scripts/check_line_cap.sh --update  (use --seed only to bootstrap)."
    sort "$NEWLIST.body"
  } > "$ALLOWLIST"
  rm -f "$NEWLIST.body"

  echo "check_line_cap: wrote $ALLOWLIST with ${count} grandfathered file(s)."
  exit 0
fi

# ===========================================================================
# CHECK MODE
# ===========================================================================
RESULT="$(mktemp "${TMPDIR:-/tmp}/linecap.res.XXXXXX")"
# shellcheck disable=SC2064
trap 'rm -f "$CURRENT" "$RESULT"' EXIT

# Build a per-path cap map for check mode too.
CAPMAP_CHK="$(mktemp "${TMPDIR:-/tmp}/linecap.cap.XXXXXX")"
# shellcheck disable=SC2064
trap 'rm -f "$CURRENT" "$RESULT" "$CAPMAP_CHK"' EXIT

git ls-files '*.rs' | while IFS= read -r f; do
  [ -n "$f" ] || continue
  cap="$(cap_for_path "$f")"
  printf '%s\t%s\n' "$f" "$cap"
done > "$CAPMAP_CHK"

# Tag all three streams:
#   Allowlist rows -> "A\tpath\tbudget"
#   Snapshot rows  -> "C\tsloc\tpath"
#   Cap-map rows   -> "P\tpath\tcap"
#
# IMPORTANT: P rows must come BEFORE C rows so that file_cap[] is fully
# populated when C rows arrive (awk is single-pass).
{
  awk 'BEGIN{FS=OFS="\t"} $0 !~ /^#/ && NF>=2 {print "A", $1, $2}' "$ALLOWLIST_READ"
  awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "P", $1, $2}' "$CAPMAP_CHK"
  awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "C", $1, $2}' "$CURRENT"
} | awk -v prod_cap="$PROD_CAP" -v test_cap="$TEST_CAP" '
  BEGIN { FS = OFS = "\t" }
  # ----- allowlist rows: A <path> <budget> -----
  $1 == "A" { budget[$2] = $3; have[$2] = 1; next }
  # ----- cap-map rows: P <path> <cap> -----
  $1 == "P" { file_cap[$2] = $3 + 0; next }
  # ----- snapshot rows:  C <sloc> <path> -----
  {
    n = $2 + 0; path = $3
    seen[path] = 1
    cap = (path in file_cap) ? file_cap[path] : prod_cap
    cap_label = (cap == prod_cap) ? (prod_cap " prod cap") : (test_cap " test cap")
    if (path in budget) {
      allowlisted++
      if (n <= cap) {
        printf "FAIL: %s is now %d SLOC (<= %s). Remove it from .line-cap-allowlist.tsv (ratchet down).\n", path, n, cap_label
        viol++
      } else if (n > budget[path] + 0) {
        printf "FAIL: %s grew to %d SLOC (frozen budget %s; cap is %s). Split it.\n", path, n, budget[path], cap_label
        viol++
      }
      # else applicable_cap < n <= budget -> grandfathered, OK
    } else {
      if (n > cap) {
        printf "FAIL: %s is %d SLOC (> %s) and not allowlisted. New oversized file; split it or it cannot merge.\n", path, n, cap_label
        viol++
      }
    }
  }
  END {
    # Allowlist entries whose file no longer exists (drift, informational).
    for (p in have) if (!(p in seen)) {
      printf "WARN: allowlisted %s no longer exists as a tracked .rs file. Remove it from .line-cap-allowlist.tsv.\n", p
    }
    printf "@SUMMARY\t%d\t%d\n", allowlisted+0, viol+0
  }
' > "$RESULT"

# Split awk output: messages to stderr, summary parsed here.
allowlisted=0
violations=0
while IFS= read -r line; do
  case "$line" in
    @SUMMARY*)
      allowlisted="$(printf '%s' "$line" | cut -f2)"
      violations="$(printf '%s' "$line" | cut -f3)"
      ;;
    FAIL:*|WARN:*)
      echo "$line" >&2
      ;;
  esac
done < "$RESULT"

if [ "$violations" -gt 0 ]; then
  echo "line-cap: $allowlisted allowlisted, $violations violation(s) — FAILED." >&2
  echo "Caps: ${PROD_CAP} SLOC (production) / ${TEST_CAP} SLOC (test/benchmark)." >&2
  echo "To re-freeze after an intentional split, run: scripts/check_line_cap.sh --update" >&2
  exit 1
fi

echo "line-cap: $allowlisted allowlisted, 0 violations — OK."
exit 0

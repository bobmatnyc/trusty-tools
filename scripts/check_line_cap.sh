#!/usr/bin/env bash
#
# check_line_cap.sh — ratcheted SLOC file-size cap enforcement (issue #610).
#
# Why: the 500-SLOC file cap documented in .trusty-mpm/INSTRUCTIONS.md had zero mechanical
#   enforcement, so source files silently grew past it under feature pressure
#   (e.g. trusty-search/src/service/server.rs reached 5,403 lines). Advice
#   without a gate loses. This script turns the cap into a CI/pre-commit gate
#   whose grandfather allowlist can only SHRINK — no new oversized files, and
#   existing oversized files may never grow.
#
# DUAL-CAP RULES (issue #1131, TEST_CAP raised #4074):
#   Production source files   → PROD_CAP = 500 SLOC
#   Test / benchmark files    → TEST_CAP = 3000 SLOC
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
# PATH-LIST MODE (issue #6406 sweep): with one or more `.rs` paths as positional
#   arguments, the scan set is those paths instead of the whole tracked tree.
#   Same SLOC counter, same cap_for_path, same allowlist comparison — there is
#   no second implementation, only a smaller input list. The pre-commit hook
#   passes the staged `.rs` files this way (`pass_filenames: true`), which drops
#   a commit's line-cap cost from ~34s over 4,363 files to well under a second.
#   CI keeps calling the no-argument whole-tree form, which is unchanged.
#
#   Two whole-tree-only checks are deliberately skipped in path-list mode
#   because a subset cannot answer them, and CI still runs both on every PR:
#     - the #4618 scan floor (MIN_RS_FILES), whose whole job is to catch a
#       broken enumeration of the FULL tree; a 1-file scan is not that.
#     - the informational drift WARN for an allowlisted file that no longer
#       exists, which would otherwise fire for every allowlisted file merely
#       absent from the staged set.
#   Every FAIL a path-list run can reach — new oversized file, allowlisted file
#   grown past its frozen budget, allowlisted file now under cap — is decided
#   per file, so a staged file that violates the cap fails the hook exactly as
#   it fails the whole-tree run.
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
#     - inline `#[cfg(test)] mod <name> { … }` unit-test modules (issue #5153)
#   A line that has code followed by a trailing // comment COUNTS (it has code).
#   A line inside a multi-line /* */ block does NOT count.
#
# #[cfg(test)] EXCLUSION (issue #5153): a 460-SLOC module that gained inline
#   tests used to cross the 500 production cap and had to have its test module
#   mechanically split into a sibling _tests.rs — an inflated diff for no
#   change in production code, which is already what the cap is about. The
#   matcher recognises ONLY `#[cfg(test)]` + `mod <name> {` at a shared indent,
#   and excludes the body through the matching close. It is deliberately
#   FAIL-CLOSED: `#[cfg(test)] mod tests;` sibling declarations, `#[cfg(test)]`
#   on an fn/impl/use/static, any non-literal cfg predicate (`all(test, …)`,
#   `any(test, …)`, `not(test)`), and any module whose brace balance is skewed
#   by braces inside string literals are all COUNTED AS PRODUCTION. The exact
#   matcher, its two independent agreement checks, and the full list of what it
#   does NOT exclude are in scripts/lib/sloc_awk.sh.
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
#   SWALLOW-TO-EOF RISK (issue #2563 item 2, intentional, not a bug): an
#   unmatched /* inside a string/char literal — one with no real */ anywhere
#   later in the file — is indistinguishable from a genuinely unterminated
#   block comment, so it swallows every subsequent line to EOF as "inside a
#   comment". This can undercount a large tail of a file, not just one line.
#   It is still safe under the never-overcount invariant above (the gate can
#   only pass files it should fail, never fail files it should pass) and is
#   pinned as a fixture in scripts/test-data/sloc-string-literal-slash-star.rs.
#
# Test: exercised in the PR that introduced SLOC counting (clean tree exits 0;
#   a production file with 600 SLOC fails; 600 SLOC in a test path passes;
#   3100 SLOC in a test path fails). The logic is pure SLOC counting against
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
TEST_CAP=3000

# Resolve repo root so the script works from any cwd. INVOCATION_DIR is captured
# BEFORE the cd so a relative path argument still resolves against the caller's
# cwd, not silently against the repo root (a path that resolved to nothing would
# be skipped, and a gate that skips the file it was handed fails open).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
INVOCATION_DIR="$PWD"
ALLOWLIST="$REPO_ROOT/.line-cap-allowlist.tsv"

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# Mode parsing
# ---------------------------------------------------------------------------
MODE="check"      # check | update
ALLOW_GROW=0      # may add new >cap files / raise budgets (--seed or --force-add)
PATH_MODE=0       # 1 when positional paths narrowed the scan set
PATHS=()
for arg in "$@"; do
  case "$arg" in
    --update)    MODE="update" ;;
    --seed)      MODE="update"; ALLOW_GROW=1 ;;
    --force-add) MODE="update"; ALLOW_GROW=1 ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "check_line_cap: unknown argument: $arg" >&2
      echo "usage: check_line_cap.sh [--update | --seed | --force-add] [path.rs ...]" >&2
      exit 2
      ;;
    *)
      PATH_MODE=1
      PATHS[${#PATHS[@]}]="$arg"
      ;;
  esac
done

# The allowlist is regenerated holistically — a subset of paths would drop every
# entry it did not scan. Refuse the combination rather than silently truncate.
if [ "$MODE" = "update" ] && [ "$PATH_MODE" -eq 1 ]; then
  echo "check_line_cap: --update/--seed/--force-add regenerate the whole allowlist" >&2
  echo "      and cannot take path arguments. Run them with no paths." >&2
  exit 2
fi

# resolve_repo_path: print the repo-relative form of one path argument.
# Tries the repo root first, then the caller's cwd; fails when the result lies
# outside the repo, because the allowlist is keyed by repo-relative path.
resolve_repo_path() {
  local p="$1" abs
  case "$p" in
    /*) abs="$p" ;;
    *)
      if [ -e "$REPO_ROOT/$p" ]; then abs="$REPO_ROOT/$p"; else abs="$INVOCATION_DIR/$p"; fi
      ;;
  esac
  case "$abs" in
    "$REPO_ROOT"/*) printf '%s\n' "${abs#"$REPO_ROOT"/}" ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------------------
# SLOC counter: shared awk program that counts code lines (SLOC) in one file.
# Defines $SLOC_AWK. See scripts/lib/sloc_awk.sh for the full algorithm note
# and the issue #2489/#2509 fix (a `/*` inside `//`/`///`/`//!` line-comment
# prose must never be mistaken for a block-comment opener).
# ---------------------------------------------------------------------------
# shellcheck source=lib/sloc_awk.sh
. "$SCRIPT_DIR/lib/sloc_awk.sh"

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
RSLIST="$(mktemp "${TMPDIR:-/tmp}/linecap.rslist.XXXXXX")"
trap 'rm -f "$CURRENT" "$RSLIST"' EXIT

# #4618: the scan floor. This gate's violation count is legitimately 0 once the
# allowlist ratchets to zero — the stated goal — at which point "0 violations"
# stops distinguishing "measured 3571 files, all under cap" from "measured
# nothing". The number that distinguishes them is how many files were measured,
# so it is floored and reported. 500 sits far below the current 3571 tracked
# .rs files and far above zero.
MIN_RS_FILES=500

# The enumeration is materialised before the loop so its exit status is
# observable; `git ls-files | while` hid a failing git behind an empty stream.
if [ "$PATH_MODE" -eq 1 ]; then
  : > "$RSLIST"
  for f in "${PATHS[@]}"; do
    case "$f" in *.rs) ;; *) continue ;; esac
    if ! rel="$(resolve_repo_path "$f")"; then
      echo "FAIL: '$f' resolves outside the repository; the allowlist is keyed by" >&2
      echo "      repo-relative path, so this file cannot be judged. NOT a pass." >&2
      exit 1
    fi
    # A path that no longer exists is a deletion or a rename's old side; there
    # is nothing to measure and nothing the cap can be violated by.
    [ -f "$rel" ] || continue
    printf '%s\n' "$rel" >> "$RSLIST"
  done
elif ! git ls-files '*.rs' > "$RSLIST"; then
  echo "FAIL: TOOL ERROR — 'git ls-files *.rs' exited non-zero; the file set could" >&2
  echo "      not be enumerated, so nothing was measured. NOT a pass (#4618)." >&2
  exit 1
fi

while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  n="$(awk "$SLOC_AWK" "$f")"
  printf '%s\t%s\n' "$n" "$f"
done < "$RSLIST" > "$CURRENT"

RS_SCANNED="$(awk 'END{print NR}' "$CURRENT")"
if [ "$PATH_MODE" -eq 0 ] && [ "${RS_SCANNED:-0}" -lt "$MIN_RS_FILES" ]; then
  echo "FAIL: SCAN FLOOR — only ${RS_SCANNED} tracked .rs file(s) were measured, below" >&2
  echo "      the declared minimum of ${MIN_RS_FILES} (MIN_RS_FILES in scripts/check_line_cap.sh)." >&2
  echo "      A gate that measures nothing reports '0 violations' and cannot fail;" >&2
  echo "      that is a broken scan, not a clean tree (issue #4618)." >&2
  exit 1
fi

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
  trap 'rm -f "$CURRENT" "$RSLIST" "$NEWLIST" "$NEWLIST.body" "$ERRFILE"' EXIT

  # Build a per-path cap map: "<path>\t<cap>" for all tracked .rs files.
  # This is written to a temp file so the awk merge step can read it.
  CAPMAP="$(mktemp "${TMPDIR:-/tmp}/linecap.cap.XXXXXX")"
  # shellcheck disable=SC2064
  trap 'rm -f "$CURRENT" "$RSLIST" "$NEWLIST" "$NEWLIST.body" "$ERRFILE" "$CAPMAP"' EXIT

  while IFS= read -r f; do
    [ -n "$f" ] || continue
    cap="$(cap_for_path "$f")"
    printf '%s\t%s\n' "$f" "$cap"
  done < "$RSLIST" > "$CAPMAP"

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
    echo "# SLOC also excludes inline '#[cfg(test)] mod <name> { ... }' bodies (issue #5153)."
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
trap 'rm -f "$CURRENT" "$RSLIST" "$RESULT"' EXIT

# Build a per-path cap map for check mode too.
CAPMAP_CHK="$(mktemp "${TMPDIR:-/tmp}/linecap.cap.XXXXXX")"
# shellcheck disable=SC2064
trap 'rm -f "$CURRENT" "$RSLIST" "$RESULT" "$CAPMAP_CHK"' EXIT

while IFS= read -r f; do
  [ -n "$f" ] || continue
  cap="$(cap_for_path "$f")"
  printf '%s\t%s\n' "$f" "$cap"
done < "$RSLIST" > "$CAPMAP_CHK"

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
} | awk -v prod_cap="$PROD_CAP" -v test_cap="$TEST_CAP" -v path_mode="$PATH_MODE" '
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
    # Undecidable from a subset — every allowlisted file outside the scanned
    # paths is absent for that reason alone, so the whole-tree run owns it.
    if (path_mode == 0) for (p in have) if (!(p in seen)) {
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

if [ "$PATH_MODE" -eq 1 ]; then
  SCOPE="$RS_SCANNED .rs path(s) given (subset scan; CI runs the whole tree)"
else
  SCOPE="$RS_SCANNED tracked .rs file(s) (floor $MIN_RS_FILES)"
fi

if [ "$violations" -gt 0 ]; then
  echo "line-cap: measured $SCOPE; $allowlisted allowlisted, $violations violation(s) — FAILED." >&2
  echo "Caps: ${PROD_CAP} SLOC (production) / ${TEST_CAP} SLOC (test/benchmark)." >&2
  echo "To re-freeze after an intentional split, run: scripts/check_line_cap.sh --update" >&2
  exit 1
fi

echo "line-cap: measured $SCOPE; $allowlisted allowlisted, 0 violations — OK."
exit 0

#!/usr/bin/env bash
#
# check_line_cap.sh — ratcheted 500-line file-size cap enforcement (issue #610).
#
# Why: the 500-line file cap documented in CLAUDE.md had zero mechanical
#   enforcement, so source files silently grew past it under feature pressure
#   (e.g. trusty-search/src/service/server.rs reached 5,403 lines). Advice
#   without a gate loses. This script turns the cap into a CI/pre-commit gate
#   whose grandfather allowlist can only SHRINK — no new oversized files, and
#   existing oversized files may never grow.
#
# What: scans every tracked `.rs` file (`git ls-files '*.rs'`) and enforces:
#   - <= CAP lines, not allowlisted          -> OK
#   - >  CAP lines, not allowlisted          -> FAIL  (new oversized file)
#   - allowlisted, current > recorded budget -> FAIL  (grew beyond frozen budget)
#   - allowlisted, current <= CAP            -> FAIL  (now under cap; drop the entry)
#   - allowlisted, CAP < current <= budget   -> OK    (grandfathered, not growing)
#   Exit non-zero on any FAIL; exit 0 when clean. Prints a one-line summary.
#
#   --update     regenerates the allowlist but only SAFELY: it may LOWER an
#                existing budget or REMOVE entries that dropped <= CAP. It
#                REFUSES to raise a budget or add a brand-new > CAP file unless
#                --seed or --force-add is also passed.
#   --seed       initial seeding: allowed to add brand-new entries. Implies update.
#   --force-add  like --update but permits adding new > CAP files / raising
#                budgets (escape hatch; use sparingly, e.g. an unavoidable bump).
#
# Test: exercised manually in the issue #610 PR (clean tree exits 0; a 501-line
#   probe file fails; appending past an allowlisted budget fails). The logic is
#   pure line counting against the committed allowlist; no unit-test harness.
#
# Portability: works on bash 3.2 (macOS system bash) and bash 5 (Linux CI).
#   Uses POSIX tools only — `git`, `wc`, `sort`, `awk`. No associative arrays,
#   no bash-4 features, no extra dependencies.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
CAP=500

# Resolve repo root so the script works from any cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
ALLOWLIST="$REPO_ROOT/.line-cap-allowlist.tsv"

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# Mode parsing
# ---------------------------------------------------------------------------
MODE="check"      # check | update
ALLOW_GROW=0      # may add new >CAP files / raise budgets (--seed or --force-add)
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
# Build a current snapshot: "<lines>\t<path>" for every tracked .rs file that
# still exists in the working tree. Computed once, reused by both modes.
# ---------------------------------------------------------------------------
CURRENT="$(mktemp "${TMPDIR:-/tmp}/linecap.cur.XXXXXX")"
trap 'rm -f "$CURRENT"' EXIT

git ls-files '*.rs' | while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  n="$(wc -l < "$f" | tr -d ' ')"
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

  # Tag each input stream so awk distinguishes them even when the allowlist is
  # empty (a plain FNR==NR split breaks on an empty first file). Allowlist rows
  # become "A<TAB>path<TAB>budget"; snapshot rows become "C<TAB>lines<TAB>path".
  {
    awk 'BEGIN{FS=OFS="\t"} $0 !~ /^#/ && NF>=2 {print "A", $1, $2}' "$ALLOWLIST_READ"
    awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "C", $1, $2}' "$CURRENT"
  } | awk -v cap="$CAP" -v allow_grow="$ALLOW_GROW" -v errfile="$ERRFILE" '
    BEGIN { FS = OFS = "\t" }
    # ----- allowlist rows: A <path> <budget> -----
    $1 == "A" { old[$2] = $3; next }
    # ----- snapshot rows:  C <lines> <path> -----
    {
      n = $2 + 0
      path = $3
      if (n <= cap) next                 # under cap -> drop from list
      if (path in old) {
        if (n > old[path] + 0) {
          if (allow_grow == 1) { keep[path] = n }
          else {
            printf "REFUSE: %s grew to %d (frozen budget %s). Split it before updating the allowlist.\n", path, n, old[path] > errfile
            err = 1
          }
        } else {
          keep[path] = n                 # ratchet down (n <= old budget)
        }
      } else {
        if (allow_grow == 1) { keep[path] = n }
        else {
          printf "REFUSE: %s is a new oversized file (%d lines > %d). Split it; do not add it to the allowlist.\n", path, n, cap > errfile
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

  count="$(wc -l < "$NEWLIST.body" | tr -d ' ')"
  {
    echo "# .line-cap-allowlist.tsv — grandfathered files over the ${CAP}-line cap (issue #610)."
    echo "# Format: <relative/path><TAB><budget>  (budget = frozen max line count)."
    echo "# Ratchet: budgets may only DECREASE; when a file drops <= ${CAP} remove it."
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

# Tag both streams (see UPDATE mode for why): allowlist rows -> "A\tpath\tbudget",
# snapshot rows -> "C\tlines\tpath". This survives an empty allowlist file.
{
  awk 'BEGIN{FS=OFS="\t"} $0 !~ /^#/ && NF>=2 {print "A", $1, $2}' "$ALLOWLIST_READ"
  awk 'BEGIN{FS=OFS="\t"} NF>=2 {print "C", $1, $2}' "$CURRENT"
} | awk -v cap="$CAP" '
  BEGIN { FS = OFS = "\t" }
  # ----- allowlist rows: A <path> <budget> -----
  $1 == "A" { budget[$2] = $3; have[$2] = 1; next }
  # ----- snapshot rows:  C <lines> <path> -----
  {
    n = $2 + 0; path = $3
    seen[path] = 1
    if (path in budget) {
      allowlisted++
      if (n <= cap) {
        printf "FAIL: %s is now %d lines (<= %d). Remove it from .line-cap-allowlist.tsv (ratchet down).\n", path, n, cap
        viol++
      } else if (n > budget[path] + 0) {
        printf "FAIL: %s grew to %d lines (frozen budget %s). Split it; the cap is %d.\n", path, n, budget[path], cap
        viol++
      }
      # else CAP < n <= budget -> grandfathered, OK
    } else {
      if (n > cap) {
        printf "FAIL: %s is %d lines (> %d) and not allowlisted. New oversized file; split it or it cannot merge.\n", path, n, cap
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
  echo "Cap is $CAP lines/file. To re-freeze after an intentional split, run: scripts/check_line_cap.sh --update" >&2
  exit 1
fi

echo "line-cap: $allowlisted allowlisted, 0 violations — OK."
exit 0

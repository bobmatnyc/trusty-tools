#!/usr/bin/env bash
#
# check_engine_api_audit.sh — the whole trusty-memory `--no-default-features`
#   public API equals the audit table's public rows (issue #9278, E10).
#
# Why: E10 narrows the trusty-memory library surface (the ADR-0066 frozen
#   `engine` plus the crate root and `transport`) one batch at a time, and
#   records every item's disposition in
#   docs/reference/trusty-memory-engine-api-audit.tsv. Without a check the
#   table and the code drift apart: an item marked NARROW stays public, or a
#   new public item lands anywhere in the crate with no row. D1 defines done
#   as every path of the no-default-features listing triaged, so the check
#   covers the whole listing, not a list of audited modules (A6 Q3).
#
# What: reads a `cargo public-api` listing of trusty-memory built with
#   `--no-default-features`, takes the item path of every `pub` line (impl
#   headers are skipped), and compares the set of `trusty_memory` paths with
#   the table:
#     - EXTRA      a listed path whose row is NARROW or HIDE (`cargo
#                  public-api` omits `#[doc(hidden)]` items, so a HIDE item
#                  that is listed lost its attribute);
#     - UNAUDITED  a listed path with no row at all;
#     - MISSING    a KEEP or DEPRECATE row whose path is not listed.
#   Each failure prints one line naming the path.
#
#   One path may carry two rows only when they differ in `kind` and agree on
#   public versus absent (a field and a method sharing a name print the same
#   path; the listing cannot tell them apart).
#
#   Fails CLOSED with exit 2, never a pass over nothing:
#     - the table is missing or has no data row;
#     - a row lacks exactly 5 non-empty tab-separated fields, names a path
#       that is not a plain `trusty_memory[::segment]*` path, has a `kind`
#       outside the known set or a disposition outside
#       {KEEP, NARROW, HIDE, DEPRECATE} (the message names the line);
#     - two rows share a path and kind, or one path has a public row and an
#       absent row (the message names the path);
#     - the listing is missing or empty, has no `trusty_memory` path, or has a
#       `pub` line the parser cannot cut to a `trusty_memory` path, such as a
#       generic group it cannot strip (the message names the line).
#
# Usage:
#   bash scripts/check_engine_api_audit.sh --api-file <listing>
#   bash scripts/check_engine_api_audit.sh            # runs cargo public-api
#   bash scripts/check_engine_api_audit.sh --help
#   Make the listing with:
#     cargo +nightly public-api -p trusty-memory --no-default-features \
#       --omit blanket-impls,auto-trait-impls,auto-derived-impls
#
# Env (fixtures only):
#   ENGINE_API_AUDIT_TSV  the audit table (default docs/reference/...tsv)
#
# Exit: 0 when the listing matches the table; 1 on any EXTRA, UNAUDITED or
#   MISSING path; 2 on a usage, tool, table or listing error.
#
# Test: scripts/check_engine_api_audit_selftest.sh — a matching listing, a
#   NARROW or HIDE row still listed, an unaudited path (in an engine module,
#   at the crate root, under `transport`, sharing a row's stem, on a generic
#   type, each generic shape, and generics carrying `->`), a missing KEEP or
#   DEPRECATE row, a field
#   and method sharing a path, the malformed-row, duplicate-row and
#   conflicting-row errors, and the missing, empty, crate-less and unparsable
#   listing errors.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); awk, sort, comm.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AUDIT="${ENGINE_API_AUDIT_TSV:-$REPO_ROOT/docs/reference/trusty-memory-engine-api-audit.tsv}"
API_FILE=''

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      sed -n '2,/^# Portability/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    --api-file)
      [ $# -ge 2 ] || { echo "check_engine_api_audit: --api-file needs a path" >&2; exit 2; }
      API_FILE="$2"
      shift 2
      ;;
    *)
      echo "check_engine_api_audit: unknown argument '$1' (try --help)" >&2
      exit 2
      ;;
  esac
done

[ -f "$AUDIT" ] || { echo "check_engine_api_audit: no audit table at $AUDIT" >&2; exit 2; }

# Fail closed on a malformed table, before any comparison, so a typo
# (`narrow`, `Keep`, `fucntion`), a space-separated row or a row naming
# something outside the crate is never read as "audited". #9278 A6: the path
# must be plain segments, since every listed path is compared with every row.
if ! awk -F '\t' '
  NR == 1 || $0 == "" || $0 ~ /^#/ { next }
  { rows++ }
  NF != 5 || $1 !~ /^trusty_memory(::[A-Za-z_][A-Za-z0-9_]*)*$/ \
    || $2 !~ /^(mod|struct|enum|union|variant|field|fn|method|trait|trait_method|const|static|type|reexport|macro)$/ \
    || $3 !~ /^(KEEP|NARROW|HIDE|DEPRECATE)$/ || $4 == "" || $5 == "" {
    printf "check_engine_api_audit: malformed row at line %d in %s: %s\n", NR, FILENAME, $0 > "/dev/stderr"
    bad = 1
    next
  }
  (($1, $2) in seen) {
    printf "check_engine_api_audit: duplicate row for %s (%s) at lines %d and %d\n", $1, $2, seen[$1, $2], NR > "/dev/stderr"
    bad = 1
  }
  { seen[$1, $2] = NR
    side = ($3 == "KEEP" || $3 == "DEPRECATE") ? "public" : "absent"
    if (($1 in sides) && sides[$1] != side) {
      printf "check_engine_api_audit: conflicting rows for %s: one public, one NARROW or HIDE (line %d)\n", $1, NR > "/dev/stderr"
      bad = 1
    }
    sides[$1] = side }
  END {
    if (rows == 0) { printf "check_engine_api_audit: no data row in %s\n", FILENAME > "/dev/stderr"; bad = 1 }
    exit bad
  }' "$AUDIT"; then
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [ -z "$API_FILE" ]; then
  API_FILE="$WORK/api.txt"
  if ! cargo +nightly public-api -p trusty-memory --no-default-features \
      --omit blanket-impls,auto-trait-impls,auto-derived-impls > "$API_FILE"; then
    echo "check_engine_api_audit: cargo public-api failed" >&2
    exit 2
  fi
fi
[ -f "$API_FILE" ] || { echo "check_engine_api_audit: no listing at $API_FILE" >&2; exit 2; }
[ -s "$API_FILE" ] || { echo "check_engine_api_audit: empty listing $API_FILE" >&2; exit 2; }

# Item path of each listing line: drop leading attributes, skip impl headers,
# strip the keyword, then drop generic groups (`<...>`, nested and lifetimes),
# then cut at the first `(`, `{`, `: ` or ` `. #9278 A6: a `pub` line that does
# not cut to a `trusty_memory` path, or a line that is neither `pub` nor an
# impl header, is a parser gap: it fails closed rather than drop a path.
if ! awk '{
  s = $0
  while (s ~ /^#\[[^]]*\] /) sub(/^#\[[^]]*\] /, "", s)
  if (s == "" || s ~ /^(unsafe )?impl/) next
  if (s !~ /^pub /) { bad_line(); next }
  sub(/^pub /, "", s)
  sub(/^((unsafe|async|const|extern "[^"]*") )*fn /, "", s)
  sub(/^(struct|enum|union|trait|unsafe trait|type|mod|const|static|use|macro) /, "", s)
  # #9278: strip generic groups (nested too) before the cut, so `X<T>::m` is
  # `X::m`, not a collapse into the row of the type itself. A1: hide `->`
  # first, or `<fn() -> u8>` ends the group at its `>`.
  gsub(/->/, "\001", s)
  while (s ~ /<[^<>]*>/) gsub(/<[^<>]*>/, "", s)
  sub(/[({].*/, "", s); sub(/: .*/, "", s)
  # A1: a `<` or `>` left in the path part is a group the strip could not
  # read (e.g. `<{ N > 1 }>`); fail closed rather than cut it at a space.
  if (s ~ /[<>]/) { bad_line(); next }
  sub(/ .*/, "", s); sub(/:$/, "", s)
  if (s ~ /^trusty_memory(::[A-Za-z_][A-Za-z0-9_]*)*$/) print s
  else bad_line()
}
function bad_line() {
  printf "check_engine_api_audit: unparsable listing line %d in %s: %s\n", NR, FILENAME, $0 > "/dev/stderr"
  bad = 1
}
END { exit bad }' "$API_FILE" > "$WORK/paths"; then
  exit 2
fi
LC_ALL=C sort -u "$WORK/paths" > "$WORK/listed"

if [ ! -s "$WORK/listed" ]; then
  echo "check_engine_api_audit: no trusty_memory path in $API_FILE" >&2
  exit 2
fi

awk -F '\t' 'NR > 1 && $0 != "" && $1 !~ /^#/ { print $1 "\t" $3 }' "$AUDIT" > "$WORK/rows"
cut -f1 "$WORK/rows" | LC_ALL=C sort -u > "$WORK/audited"
awk -F '\t' '$2 == "NARROW" || $2 == "HIDE" { print $1 }' "$WORK/rows" \
  | LC_ALL=C sort -u > "$WORK/absent"
awk -F '\t' '$2 == "KEEP" || $2 == "DEPRECATE" { print $1 }' "$WORK/rows" \
  | LC_ALL=C sort -u > "$WORK/public"

FAILS=0
report() {
  local label="$1" file="$2" n
  n="$(awk 'END { print NR }' "$file")"
  [ "$n" -eq 0 ] && return 0
  FAILS=$((FAILS + n))
  sed "s/^/$label /" "$file"
}

LC_ALL=C comm -12 "$WORK/listed" "$WORK/absent" > "$WORK/extra"
LC_ALL=C comm -23 "$WORK/listed" "$WORK/audited" > "$WORK/unaudited"
LC_ALL=C comm -13 "$WORK/listed" "$WORK/public" > "$WORK/missing"
report EXTRA "$WORK/extra"
report UNAUDITED "$WORK/unaudited"
report MISSING "$WORK/missing"

if [ "$FAILS" -gt 0 ]; then
  echo "check_engine_api_audit: FAIL — $FAILS path(s) disagree with $AUDIT"
  exit 1
fi
echo "check_engine_api_audit: OK — $(awk 'END { print NR }' "$WORK/listed") audited path(s) match"

#!/usr/bin/env bash
#
# check_engine_api_audit.sh — the audited part of the trusty-memory engine
#   public API equals the audit table's public rows (issue #9278, E10).
#
# Why: E10 narrows `trusty_memory::engine::*` (the ADR-0066 frozen surface)
#   one batch at a time, and records every item's disposition in
#   docs/reference/trusty-memory-engine-api-audit.tsv. Without a check the
#   table and the code drift apart: an item marked NARROW stays public, or a
#   new public item lands in an audited module with no row.
#
# What: reads a `cargo public-api` listing of trusty-memory built with
#   `--no-default-features`, takes the item path of every line (impl headers
#   are skipped), and keeps the paths under an audited scope prefix
#   (SCOPE_PREFIXES below; each batch adds its prefixes). It compares them with
#   the table rows under the same prefixes:
#     - EXTRA      a listed path whose row is NARROW or HIDE (`cargo
#                  public-api` omits `#[doc(hidden)]` items, so a HIDE item
#                  that is listed lost its attribute);
#     - UNAUDITED  a listed path with no row at all;
#     - MISSING    a KEEP or DEPRECATE row whose path is not listed.
#   Each failure prints one line naming the path. A path matches a prefix
#   when it equals it or continues with `::`.
#
#   Fails CLOSED: an empty listing, or a listing with no path under any scope
#   prefix, is exit 2, never a pass over nothing.
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
#   MISSING path; 2 on a usage or tool error.
#
# Test: scripts/check_engine_api_audit_selftest.sh — a matching listing, a
#   NARROW or HIDE row still listed, an unaudited path, a missing KEEP row, an
#   out-of-scope path, a prefix that only shares a stem, and the empty-listing
#   and bad-argument errors.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); awk, sort, comm.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

# See #9278: one line per audited prefix. A3a = store knowledge-graph modules.
SCOPE_PREFIXES='
trusty_memory::engine::store::kg
trusty_memory::engine::store::kg_redb
trusty_memory::engine::store::kg_store
trusty_memory::engine::store::kg_writer
trusty_memory::engine::store::KnowledgeGraph
'

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
[ -s "$API_FILE" ] || { echo "check_engine_api_audit: empty listing $API_FILE" >&2; exit 2; }

printf '%s\n' "$SCOPE_PREFIXES" | awk 'NF' > "$WORK/scopes"

# in_scope: keep the input lines whose first field is under a scope prefix.
in_scope() {
  awk -F '\t' 'NR == FNR { p[$1] = 1; next }
    { for (s in p) if ($1 == s || index($1, s "::") == 1) { print; next } }' "$WORK/scopes" -
}

# Item path of each listing line: drop leading attributes, skip impl headers,
# strip the keyword, then cut at the first `(`, `<`, `: `, ` ` or `{`.
awk '{
  s = $0
  while (s ~ /^#\[[^]]*\] /) sub(/^#\[[^]]*\] /, "", s)
  if (s ~ /^impl/ || s !~ /^pub /) next
  sub(/^pub /, "", s)
  sub(/^((unsafe|async|const|extern "[^"]*") )*fn /, "", s)
  sub(/^(struct|enum|union|trait|unsafe trait|type|mod|const|static|use|macro) /, "", s)
  sub(/[(<{].*/, "", s); sub(/: .*/, "", s); sub(/ .*/, "", s); sub(/:$/, "", s)
  if (s ~ /^trusty_memory(::|$)/) print s
}' "$API_FILE" | in_scope | LC_ALL=C sort -u > "$WORK/listed"

if [ ! -s "$WORK/listed" ]; then
  echo "check_engine_api_audit: no listed path under any scope prefix in $API_FILE" >&2
  exit 2
fi

awk -F '\t' 'NR > 1 && $1 !~ /^#/ && NF >= 3 { print $1 "\t" $3 }' "$AUDIT" \
  | in_scope > "$WORK/rows"
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

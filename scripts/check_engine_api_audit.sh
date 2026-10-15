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
#     - MISSING    a KEEP or DEPRECATE row whose path is not listed;
#     - UNMARKED   a listed `pub enum` whose attribute prefix lacks
#                  `#[non_exhaustive]` (A1 Q1: no enum is exhaustive and
#                  there is no allow-list; a re-export path counts too);
#                  or a listed `pub struct` with a public field (a field
#                  line under its path, or a `pub ` tuple field in any
#                  position) whose prefix lacks it, unless exempt (A2b); or a
#                  listed struct-like enum variant (a variant line with a
#                  field line under its path) whose prefix lacks it, unless
#                  its row carries a pending marker (A2c);
#     - ANYHOW     a listed path whose line holds the token `anyhow::`
#                  (return, argument, type alias, field, trait method; any
#                  line of the path) and whose KEEP row carries no
#                  `anyhow-pending` marker, or that has no row (T0).
#   Each failure prints one line naming the path. The struct and variant
#   joins run after the whole listing, so line order does not matter. Known
#   gap: a unit struct and an opaque struct print alike (`pub struct X`) and
#   both pass.
#
#   Struct exemption: a `struct` KEEP row whose reason cell starts
#   `exhaustive: ` and says why (PalaceId). It fails closed (exit 2,
#   `stale exemption <path>: <why>`) on any other kind or disposition, on a
#   path that is not a listed struct, on a struct already marked, on a
#   struct with no public field, and on an empty or blank reason (A2c).
#
#   Variant pending marker (A2c ruling): a `variant` KEEP row whose reason
#   cell starts `variant-pending T<n>: <reason>`, n in 1-4, names the slice
#   that will mark that unmarked struct-like variant. It is the only
#   disposition for one (no allow-list, no scoped rule). Each slice removes
#   its markers as it marks the variants, so T4 ends at zero. It fails closed
#   (exit 2, `stale pending <path>: <why>`) on any other kind or disposition,
#   on a path that is not a listed variant, on a variant already marked, on a
#   variant with no field line, and on a missing or out-of-range tag or an
#   empty reason.
#
#   Anyhow pending marker (T0): a KEEP row of kind fn, method, trait_method,
#   type or field whose reason cell starts `anyhow-pending T<n>: <reason>`,
#   n in 1-4, names the slice that converts that anyhow path. The token is
#   `anyhow::` after a non-word character, so `not_anyhow::` and
#   `anyhow_like` do not count. Stale (exit 2, `stale pending <path>: <why>`):
#   any other kind or disposition, a path that is not listed, a path whose
#   lines hold no `anyhow::` (converted: the slice must clear the marker), a
#   missing or out-of-range tag, an empty or blank reason. Known gaps: a type
#   that wraps anyhow privately (`struct E(anyhow::Error)`) prints no token,
#   and a public `impl From<anyhow::Error> for X` header is skipped by the
#   parser; both count 0 today (T0 Q8).
#
#   Ratchet (T0): docs/reference/trusty-memory-engine-api-ratchet.tsv, header
#   `marker slice initial ceiling`, one row per marker (anyhow, variant) and
#   slice (T1-T4). `initial` is the T0 count and never changes; `ceiling`
#   may only fall. Each fault prints `RATCHET <why>` (exit 1):
#     R1 the table's valid markers per marker and slice equal the ceiling;
#     R2 a ceiling is at most its initial;
#     R3 with --base-ratchet: a ceiling is at most the base file's and an
#        initial equals it. Without the flag the guard is stateless and
#        cannot see a raised ceiling; R3 is the only check that reads history;
#     R4 a slice with progress (a ceiling below its initial, either marker)
#        needs every earlier slice at ceiling 0 for both markers. A stateless
#        guard cannot know which PR it runs in, so R4 bites at the NEXT
#        slice's PR; a slice may fall in steps (T1a, T1b);
#     every `variant-pending` row of one enum (keyed by the enum name, so a
#        re-export path counts) carries one tag.
#     R6 with --base-audit: every valid pending marker of the head table
#        exists in the base audit table for the same path, marker kind and
#        T<n>. Markers may disappear; they may not appear, move to another
#        path or be retagged. R1-R4 compare counts only, so a retag or a swap
#        that leaves the counts alone passes them. Prints
#        `RATCHET audit <path>: <marker>-pending T<n> is not in the base audit table`.
#   A missing ratchet file, a bad header, a malformed, duplicate or missing
#   row fails closed (exit 2), for the --base-ratchet file too. So does a
#   missing, unreadable, empty, headerless or row-less --base-audit file, and
#   an empty-string path to --base-ratchet or --base-audit (an unset variable
#   must not switch R3 or R6 off).
#
#   --final (the 1.0 end state, run by the T4 gate): every valid pending
#   marker of either kind prints `PENDING <path>: <marker>-pending T<n>` (exit 1).
#
#   One path may carry two rows only when they differ in `kind` and agree on
#   public versus absent (a field and a method sharing a name print the same
#   path; the listing cannot tell them apart).
#
#   Fails CLOSED with exit 2, never a pass over nothing:
#     - the table is missing, has no data row, or its line 1 is not exactly
#       the header `path kind disposition consumer_or_reason batch`
#       (tab-separated, no CR);
#     - a row lacks exactly 5 non-empty tab-separated fields, names a path
#       that is not a plain `trusty_memory[::segment]*` path, has a `kind`
#       outside the known set or a disposition outside
#       {KEEP, NARROW, HIDE, DEPRECATE} (the message names the line);
#     - two rows share a path and kind, or one path has a public row and an
#       absent row (the message names the path);
#     - the listing is missing or empty, has no `trusty_memory` path, or has a
#       `pub` line the parser cannot cut to a `trusty_memory` path, such as a
#       generic group it cannot strip or an attribute glued to `pub` (the
#       message names the line);
#     - a stale or misplaced `exhaustive:` exemption, or a stale, misplaced
#       or malformed `variant-pending` or `anyhow-pending` marker (above);
#     - a bad ratchet file (above).
#
# Usage:
#   bash scripts/check_engine_api_audit.sh --api-file <listing>
#   bash scripts/check_engine_api_audit.sh            # runs cargo public-api
#   bash scripts/check_engine_api_audit.sh [--base-ratchet <file>] [--base-audit <file>] [--final]
#   bash scripts/check_engine_api_audit.sh --help
#   Make the listing with:
#     cargo +nightly public-api -p trusty-memory --no-default-features \
#       --omit blanket-impls,auto-trait-impls,auto-derived-impls
#   Slice gate (T1-T3; T4 adds --final), <base> = the PR's base branch:
#     git show origin/<base>:docs/reference/trusty-memory-engine-api-ratchet.tsv \
#       > "$S/base-ratchet.tsv"
#     git show origin/<base>:docs/reference/trusty-memory-engine-api-audit.tsv \
#       > "$S/base-audit.tsv"
#     bash scripts/check_engine_api_audit.sh --api-file <listing> \
#       --base-ratchet "$S/base-ratchet.tsv" --base-audit "$S/base-audit.tsv"
#
# Env (fixtures only):
#   ENGINE_API_AUDIT_TSV   the audit table (default docs/reference/...tsv)
#   ENGINE_API_RATCHET_TSV the ratchet table (default docs/reference/...tsv)
#
# Exit: 0 when the listing matches the table; 1 on any EXTRA, UNAUDITED,
#   MISSING, UNMARKED, ANYHOW, RATCHET or PENDING line; 2 on a usage, tool,
#   table, ratchet, base file or listing error.
#
# Test: scripts/check_engine_api_audit_selftest.sh — a matching listing, a
#   NARROW or HIDE row still listed, an unaudited path (in an engine module,
#   at the crate root, under `transport`, sharing a row's stem, on a generic
#   type, each generic shape, and generics carrying `->`), a missing KEEP or
#   DEPRECATE row, an enum, a struct with a public field (any tuple field
#   too) or a struct-like variant without `#[non_exhaustive]`, the
#   `exhaustive:` exemption and its stale, misplaced and reason-less forms,
#   the `variant-pending` marker and its stale, misplaced and malformed forms,
#   the `anyhow::` token in each line shape and the `anyhow-pending` marker
#   and its stale, misplaced and malformed forms, the ratchet rules R1-R4 and
#   the per-enum tag, a bad ratchet or base file, --final, attribute order and glued attributes, a field
#   and method sharing a path, the bad-header, malformed-row, duplicate-row and
#   conflicting-row errors, and the missing, empty, crate-less and unparsable
#   listing errors.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); awk, sort, comm.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AUDIT="${ENGINE_API_AUDIT_TSV:-$REPO_ROOT/docs/reference/trusty-memory-engine-api-audit.tsv}"
RATCHET="${ENGINE_API_RATCHET_TSV:-$REPO_ROOT/docs/reference/trusty-memory-engine-api-ratchet.tsv}"
API_FILE=''
BASE_RATCHET=''
BASE_AUDIT=''
FINAL=0

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
    --base-ratchet)
      # An empty path (an unset variable) must not switch R3 off.
      [ $# -ge 2 ] && [ -n "$2" ] || { echo "check_engine_api_audit: --base-ratchet needs a path" >&2; exit 2; }
      BASE_RATCHET="$2"
      shift 2
      ;;
    --base-audit)
      [ $# -ge 2 ] && [ -n "$2" ] || { echo "check_engine_api_audit: --base-audit needs a path" >&2; exit 2; }
      BASE_AUDIT="$2"
      shift 2
      ;;
    --final)
      FINAL=1
      shift
      ;;
    *)
      echo "check_engine_api_audit: unknown argument '$1' (try --help)" >&2
      exit 2
      ;;
  esac
done

[ -f "$AUDIT" ] || { echo "check_engine_api_audit: no audit table at $AUDIT" >&2; exit 2; }

# #9278 A1: both awks below skip line 1 as the header, so line 1 must BE the
# header, byte for byte (a trailing CR is rejected too). A deleted header would
# otherwise drop the first data row from validation and from the sets.
WANT_HEADER="$(printf 'path\tkind\tdisposition\tconsumer_or_reason\tbatch')"
HEADER=''
IFS= read -r HEADER < "$AUDIT" || true
if [ "$HEADER" != "$WANT_HEADER" ]; then
  echo "check_engine_api_audit: bad header at line 1 in $AUDIT (want path, kind, disposition, consumer_or_reason, batch, tab-separated)" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Fail closed on a malformed table, before any comparison, so a typo
# (`narrow`, `Keep`, `fucntion`), a space-separated row or a row naming
# something outside the crate is never read as "audited". #9278 A6: the path
# must be plain segments, since every listed path is compared with every row.
# #9278 A2b: every `exhaustive:` reason goes to $WORK/exempt in table order,
# flagged E (a struct KEEP exemption) or M (misplaced); the listing awk joins.
# A2c: a struct KEEP row with no reason after the prefix is flagged R. Every
# `variant-pending` marker goes there too, flagged P (valid), PM (misplaced)
# or PR (no T1-T4 tag or no reason). T0: so does every `anyhow-pending`
# marker, flagged A, AM or AR; a valid P or A row carries its tag.
: > "$WORK/exempt"
if ! awk -F '\t' -v exemptf="$WORK/exempt" '
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
    sides[$1] = side
    if ($4 ~ /^exhaustive:/) {
      if ($2 != "struct" || $3 != "KEEP") print $1 "\tM\t" $2 " " $3 > exemptf
      else if ($4 ~ /^exhaustive: +[^ ]/) print $1 "\tE\t" > exemptf
      else print $1 "\tR\t" > exemptf
    }
    if ($4 ~ /^variant-pending/) {
      if ($2 != "variant" || $3 != "KEEP") print $1 "\tPM\t" $2 " " $3 > exemptf
      else if ($4 ~ /^variant-pending T[1-4]: +[^ ]/) print $1 "\tP\t" substr($4, 17, 2) > exemptf
      else print $1 "\tPR\t" > exemptf
    }
    if ($4 ~ /^anyhow-pending/) {
      if ($3 != "KEEP" || $2 !~ /^(fn|method|trait_method|type|field)$/) print $1 "\tAM\t" $2 " " $3 > exemptf
      else if ($4 ~ /^anyhow-pending T[1-4]: +[^ ]/) print $1 "\tA\t" substr($4, 16, 2) > exemptf
      else print $1 "\tAR\t" > exemptf
    } }
  END {
    if (rows == 0) { printf "check_engine_api_audit: no data row in %s\n", FILENAME > "/dev/stderr"; bad = 1 }
    exit bad
  }' "$AUDIT"; then
  exit 2
fi

# T0 ratchet_read <file> <out>: R5, fail closed (exit 2) unless line 1 is the
# exact header and the rows are one integer `initial` and `ceiling` per
# marker and slice; writes the rows to <out>.
WANT_RATCHET_HEADER="$(printf 'marker\tslice\tinitial\tceiling')"
ratchet_read() {
  local f="$1" head=''
  [ -f "$f" ] || { echo "check_engine_api_audit: no ratchet table at $f" >&2; exit 2; }
  IFS= read -r head < "$f" || true
  if [ "$head" != "$WANT_RATCHET_HEADER" ]; then
    echo "check_engine_api_audit: bad ratchet header at line 1 in $f (want marker, slice, initial, ceiling, tab-separated)" >&2
    exit 2
  fi
  if ! awk -F '\t' '
    NR == 1 || $0 == "" || $0 ~ /^#/ { next }
    NF != 4 || $1 !~ /^(anyhow|variant)$/ || $2 !~ /^T[1-4]$/ || $3 !~ /^[0-9]+$/ || $4 !~ /^[0-9]+$/ {
      printf "check_engine_api_audit: malformed ratchet row at line %d in %s: %s\n", NR, FILENAME, $0 > "/dev/stderr"
      bad = 1
      next
    }
    (($1, $2) in seen) {
      printf "check_engine_api_audit: duplicate ratchet row %s %s at lines %d and %d in %s\n", $1, $2, seen[$1, $2], NR, FILENAME > "/dev/stderr"
      bad = 1
      next
    }
    { seen[$1, $2] = NR; print $1 "\t" $2 "\t" ($3 + 0) "\t" ($4 + 0) }
    END {
      split("anyhow variant", m, " ")
      for (i = 1; i <= 2; i++) for (j = 1; j <= 4; j++) if (!((m[i], "T" j) in seen)) {
        printf "check_engine_api_audit: ratchet table lacks %s T%d in %s\n", m[i], j, FILENAME > "/dev/stderr"
        bad = 1
      }
      exit bad
    }' "$f" > "$2"; then
    exit 2
  fi
}
ratchet_read "$RATCHET" "$WORK/ratchet"
: > "$WORK/base-ratchet"
[ -z "$BASE_RATCHET" ] || ratchet_read "$BASE_RATCHET" "$WORK/base-ratchet"

# T0 R6: the base branch's audit table, reduced to its pending markers
# (`marker slice path`); fails closed (exit 2) like the base ratchet file.
: > "$WORK/base-markers"
if [ -n "$BASE_AUDIT" ]; then
  [ -f "$BASE_AUDIT" ] && [ -r "$BASE_AUDIT" ] || { echo "check_engine_api_audit: no base audit table at $BASE_AUDIT" >&2; exit 2; }
  BAHEAD=''
  IFS= read -r BAHEAD < "$BASE_AUDIT" || true
  if [ "$BAHEAD" != "$WANT_HEADER" ]; then
    echo "check_engine_api_audit: bad base audit header at line 1 in $BASE_AUDIT (want path, kind, disposition, consumer_or_reason, batch, tab-separated)" >&2
    exit 2
  fi
  if ! awk -F '\t' '
    NR == 1 || $0 == "" || $0 ~ /^#/ { next }
    { rows++ }
    $4 ~ /^anyhow-pending T[1-4]: +[^ ]/ { print "anyhow\t" substr($4, 16, 2) "\t" $1 }
    $4 ~ /^variant-pending T[1-4]: +[^ ]/ { print "variant\t" substr($4, 17, 2) "\t" $1 }
    END { if (rows == 0) exit 1 }' "$BASE_AUDIT" > "$WORK/base-markers"; then
    echo "check_engine_api_audit: no data row in the base audit table $BASE_AUDIT" >&2
    exit 2
  fi
fi

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
# #9278 A1 Q1: the attribute prefix is saved as it is dropped; a `pub enum`
# line whose prefix lacks `#[non_exhaustive]` writes its path to $WORK/unmarked.
# #9278 A2b: structs, their marks and their public fields are recorded per
# path and joined in END, where an unmarked struct with a public field that is
# not exempt is written to $WORK/unmarked too. A2c: so is an unmarked variant
# (a keyword-less line that is not a field) with a field line under it. T0: a
# path with an `anyhow::` line and no valid marker goes to $WORK/anyhow.
: > "$WORK/unmarked"
: > "$WORK/anyhow"
if ! awk -v unmarked="$WORK/unmarked" -v exemptf="$WORK/exempt" -v anyhowf="$WORK/anyhow" '
BEGIN {
  while ((getline line < exemptf) > 0) {
    split(line, f, "\t"); n++
    ex_path[n] = f[1]; ex_flag[n] = f[2]; ex_note[n] = f[3]
    if (f[2] == "E") exempt[f[1]] = 1
  }
  close(exemptf)
}
{
  s = $0; attrs = ""
  while (match(s, /^#\[[^]]*\] /)) { attrs = attrs substr(s, 1, RLENGTH); s = substr(s, RLENGTH + 1) }
  if (s == "" || s ~ /^(unsafe )?impl/) next
  if (s !~ /^pub /) { bad_line(); next }
  sub(/^pub /, "", s)
  # T0: read before the generic strip, which would drop `Output = anyhow::..`.
  has_anyhow = (s ~ /(^|[^A-Za-z0-9_])anyhow::/)
  is_enum = (s ~ /^enum /); is_struct = (s ~ /^struct /)
  marked = (index(attrs, "#[non_exhaustive] ") > 0)
  kw = s
  sub(/^((unsafe|async|const|extern "[^"]*") )*fn /, "", s)
  sub(/^(struct|enum|union|trait|unsafe trait|type|mod|const|static|use|macro) /, "", s)
  # A2b: a line with no keyword is a field or a variant; a field prints `: `.
  # A2c: a line with no keyword that is not a field is a variant (unit, tuple
  # or struct-like); read before the cut below changes s.
  nokw = (s == kw)
  is_field = (nokw && s ~ /^trusty_memory(::[A-Za-z_][A-Za-z0-9_]*)*: /)
  # #9278: strip generic groups (nested too) before the cut, so `X<T>::m` is
  # `X::m`, not a collapse into the row of the type itself. A1: hide `->`
  # first, or `<fn() -> u8>` ends the group at its `>`.
  gsub(/->/, "\001", s)
  while (s ~ /<[^<>]*>/) gsub(/<[^<>]*>/, "", s)
  # A2c: any tuple field, not only the first (`X(_, pub u8)`).
  tuple_pub = (is_struct && s ~ /^[^({ ]*\((.*, )?pub /)
  sub(/[({].*/, "", s); sub(/: .*/, "", s)
  # A1: a `<` or `>` left in the path part is a group the strip could not
  # read (e.g. `<{ N > 1 }>`); fail closed rather than cut it at a space.
  if (s ~ /[<>]/) { bad_line(); next }
  sub(/ .*/, "", s); sub(/:$/, "", s)
  if (s !~ /^trusty_memory(::[A-Za-z_][A-Za-z0-9_]*)*$/) { bad_line(); next }
  print s
  listed[s] = 1; npaths++
  if (has_anyhow) anyp[s] = 1
  if (is_enum && !marked) print s > unmarked
  if (is_struct) { structs[s] = 1; if (marked) ne[s] = 1; if (tuple_pub) haspub[s] = 1 }
  if (nokw && !is_field) { variants[s] = 1; if (marked) vne[s] = 1 }
  # The parent of a field is its path minus the last segment, matched exactly: a
  # struct-variant field has the variant as parent, never a struct. A2c: that
  # makes the variant struct-like.
  if (is_field) { p = s; sub(/::[A-Za-z_][A-Za-z0-9_]*$/, "", p); haspub[p] = 1 }
}
function bad_line() {
  printf "check_engine_api_audit: unparsable listing line %d in %s: %s\n", NR, FILENAME, $0 > "/dev/stderr"
  bad = 1
}
END {
  # T0: with no path listed the shell reports the listing; a stale check over
  # nothing would only bury that message.
  if (npaths == 0) exit bad
  for (i = 1; i <= n; i++) {
    p = ex_path[i]
    if (ex_flag[i] ~ /^A/) {
      if (ex_flag[i] == "AM") why = "anyhow-pending is only for a KEEP row of kind fn, method, trait_method, type or field (" ex_note[i] ")"
      else if (ex_flag[i] == "AR") why = "anyhow-pending needs a T1-T4 tag and a reason"
      else if (!(p in listed)) why = "not a listed path"
      else if (!(p in anyp)) why = "the path holds no anyhow"
      else { apend[p] = 1; continue }
      printf "check_engine_api_audit: stale pending %s: %s\n", p, why > "/dev/stderr"
      bad = 1
      continue
    }
    if (ex_flag[i] ~ /^P/) {
      if (ex_flag[i] == "PM") why = "variant-pending is only for a variant KEEP row (" ex_note[i] ")"
      else if (ex_flag[i] == "PR") why = "variant-pending needs a T1-T4 tag and a reason"
      else if (!(p in variants)) why = "not a listed variant"
      else if (p in vne) why = "the variant is #[non_exhaustive]"
      else if (!(p in haspub)) why = "the variant has no field line"
      else { pending[p] = 1; continue }
      printf "check_engine_api_audit: stale pending %s: %s\n", p, why > "/dev/stderr"
      bad = 1
      continue
    }
    if (ex_flag[i] == "M") why = "exhaustive: is only for a struct KEEP row (" ex_note[i] ")"
    else if (!(p in structs)) why = "not a listed struct"
    else if (p in ne) why = "the struct is #[non_exhaustive]"
    else if (!(p in haspub)) why = "the struct has no public field"
    else if (ex_flag[i] == "R") why = "exhaustive: needs a reason"
    else continue
    printf "check_engine_api_audit: stale exemption %s: %s\n", p, why > "/dev/stderr"
    bad = 1
  }
  for (p in structs) if ((p in haspub) && !(p in ne) && !(p in exempt)) print p > unmarked
  for (p in variants) if ((p in haspub) && !(p in vne) && !(p in pending)) print p > unmarked
  for (p in anyp) if (!(p in apend)) print p > anyhowf
  exit bad
}' "$API_FILE" > "$WORK/paths"; then
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
LC_ALL=C sort -u "$WORK/unmarked" > "$WORK/unmarked.sorted"
report UNMARKED "$WORK/unmarked.sorted"
LC_ALL=C sort -u "$WORK/anyhow" > "$WORK/anyhow.sorted"
report ANYHOW "$WORK/anyhow.sorted"

# T0: the valid markers, `marker slice path`, sorted by path; every stale
# one has already failed closed above.
awk -F '\t' '$2 == "A" { print "anyhow\t" $3 "\t" $1 } $2 == "P" { print "variant\t" $3 "\t" $1 }' \
  "$WORK/exempt" | LC_ALL=C sort -t "$(printf '\t')" -k3,3 > "$WORK/markers"

# Ratchet R1-R4 and the per-enum tag (header). Lines go to $WORK/ratchet.out.
awk -F '\t' -v rf="$WORK/ratchet" -v bf="$WORK/base-ratchet" -v base="${BASE_RATCHET:+1}" \
  -v af="$WORK/base-markers" -v basea="${BASE_AUDIT:+1}" '
  FILENAME == rf { init[$1, $2] = $3; ceil[$1, $2] = $4; next }
  FILENAME == bf { binit[$1, $2] = $3; bceil[$1, $2] = $4; next }
  FILENAME == af { bm[$1, $2, $3] = 1; next }
  { cnt[$1, $2]++ }
  basea && !(($1, $2, $3) in bm) { printf "audit %s: %s-pending %s is not in the base audit table\n", $3, $1, $2 }
  $1 == "variant" {
    e = $3; sub(/::[A-Za-z_][A-Za-z0-9_]*$/, "", e); sub(/.*::/, "", e)
    if (!(e in ntag)) { ntag[e] = 0; order[++ne] = e }
    if (!((e, $2) in at)) { at[e, $2] = $3; ntag[e]++ }
  }
  END {
    split("anyhow variant", m, " ")
    for (i = 1; i <= 2; i++) for (j = 1; j <= 4; j++) {
      k = m[i] SUBSEP "T" j; c = cnt[k] + 0; lbl = m[i] " T" j
      if (c != ceil[k]) printf "%s: table has %d, ceiling %d\n", lbl, c, ceil[k]
      if (ceil[k] > init[k]) printf "%s: ceiling %d above initial %d\n", lbl, ceil[k], init[k]
      if (base && ceil[k] > bceil[k]) printf "%s: ceiling %d above base %d\n", lbl, ceil[k], bceil[k]
      if (base && init[k] != binit[k]) printf "%s: initial %d differs from base %d\n", lbl, init[k], binit[k]
    }
    for (j = 2; j <= 4; j++) {
      t = "T" j
      if (ceil["anyhow", t] >= init["anyhow", t] && ceil["variant", t] >= init["variant", t]) continue
      for (h = 1; h < j; h++)
        if (ceil["anyhow", "T" h] > 0 || ceil["variant", "T" h] > 0) printf "order: T%d is open while %s has progress\n", h, t
    }
    for (x = 1; x <= ne; x++) {
      e = order[x]
      if (ntag[e] < 2) continue
      line = ""
      for (j = 1; j <= 4; j++) if ((e, "T" j) in at) line = line (line == "" ? "" : ", ") "T" j " at " at[e, "T" j]
      printf "enum %s: mixed tags %s\n", e, line
    }
  }' "$WORK/ratchet" "$WORK/base-ratchet" "$WORK/base-markers" "$WORK/markers" > "$WORK/ratchet.out"
PATH_FAILS="$FAILS"
report RATCHET "$WORK/ratchet.out"
RATCHET_FAILS=$((FAILS - PATH_FAILS))
FAILS="$PATH_FAILS"

# T0 --final: the 1.0 end state, no pending marker of either kind.
if [ "$FINAL" -eq 1 ]; then
  awk -F '\t' '{ print $3 ": " $1 "-pending " $2 }' "$WORK/markers" > "$WORK/pending"
  report PENDING "$WORK/pending"
fi

if [ "$RATCHET_FAILS" -gt 0 ]; then
  echo "check_engine_api_audit: FAIL — $FAILS path(s) and $RATCHET_FAILS ratchet line(s) disagree with $AUDIT"
  exit 1
fi
if [ "$FAILS" -gt 0 ]; then
  echo "check_engine_api_audit: FAIL — $FAILS path(s) disagree with $AUDIT"
  exit 1
fi
echo "check_engine_api_audit: OK — $(awk 'END { print NR }' "$WORK/listed") audited path(s) match"

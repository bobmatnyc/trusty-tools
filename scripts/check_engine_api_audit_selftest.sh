#!/usr/bin/env bash
#
# check_engine_api_audit_selftest.sh — self-test for
# scripts/check_engine_api_audit.sh (issue #9278).
#
# Why: a gate whose parser silently stopped reading paths reports OK exactly
#   like a matching listing (#4618). Its ability to fail is tested here.
# What: each case writes a small `cargo public-api` listing and audit table to
#   a temp dir and runs the gate with `--api-file` and ENGINE_API_AUDIT_TSV,
#   under the same bash that runs this script. The gate covers the whole crate
#   (A6 Q3), so every listed `trusty_memory` path needs a row.
#   Failing cases (exit 1): a NARROW or HIDE row still listed; a listed path
#   with no row in an engine module, at the crate root (`trusty_memory`
#   itself and a root fn), under `transport::uds`, next to a row that only
#   shares its stem, on a generic type, and in each generic shape (engine and
#   crate root); a method on a type whose generics carry `->` (fn pointer,
#   `Fn` bound); a KEEP or DEPRECATE row not listed; a public enum without
#   `#[non_exhaustive]` (plain, re-exported, generic, or with another
#   attribute only); a struct with a public field and no `#[non_exhaustive]`
#   (plain, re-exported, field line first, a tuple field first or later,
#   generic, another attribute only); an exemption reason without the
#   `exhaustive: ` prefix; a struct-like variant without `#[non_exhaustive]`
#   (plain, re-exported, field line first, another attribute only, the second
#   path of a variant whose first path carries a pending marker).
#   Passing cases: a matching listing whose lines carry an attribute prefix, a
#   generic fn, a field, a variant and an impl header; the generic shapes
#   against a matching table; a fn-pointer generic method with its row; marked
#   enums; a field and a method sharing one path; marked structs (plain,
#   generic, either attribute order); opaque, unit and private-tuple structs;
#   a marked struct-like variant whose field is not a struct field; a struct
#   name that prefixes another; `exhaustive:` exemptions with a reason; tuple
#   and unit variants; an unmarked struct-like variant whose row carries a
#   `variant-pending T<n>: ` marker.
#   Errors (exit 2, naming the line or path): a missing, empty, crate-less or
#   unparsable listing (a generic group the strip cannot read, or an
#   attribute glued to `pub`, included); a stale exemption (on a marked
#   struct, an opaque struct, an unlisted path) or a misplaced one (a non-struct
#   kind, a NARROW row); an exemption with an empty, blank or bare reason, on
#   an unmarked or a marked struct; a `variant-pending` marker that is stale
#   (a marked variant, an unlisted path, a tuple variant), misplaced (a
#   non-variant kind, a NARROW row) or malformed (no tag, a tag outside
#   T1-T4, no reason); a missing or header-only table; a header line that is deleted, misspelled,
#   six columns wide or CRLF-terminated; a malformed row (an
#   unknown or mixed-case disposition, a space-separated row, a 4-field row,
#   an empty field, an unknown kind, a path outside the crate or carrying
#   generics); a duplicate path-and-kind row; a path with a public and an
#   absent row; a bad argument.
#   T0 (cases 101 on): every case gets a ratchet table generated from its own
#   marker counts unless it sets RATCHET_FIXTURE. A path whose line holds the
#   token `anyhow::` (return, argument, type alias, field, async-trait shape,
#   each re-export path) without an `anyhow-pending T<n>: ` marker fails
#   (exit 1); a word that only contains `anyhow` and a marked fn or field
#   pass. A stale anyhow marker (clean or unlisted path), a misplaced one
#   (NARROW row, variant kind) or a malformed one (no tag, T5, empty or blank
#   reason, glued tag) fails closed, as does a pending marker on a unit
#   variant (exit 2). Ratchet: count above or below the ceiling, ceiling above
#   initial, a ceiling above or an initial different from --base-ratchet, a
#   later slice with progress while an earlier one is open, and one enum with
#   two tags fail (exit 1); equal counts, a lower ceiling than base, an
#   earlier slice at zero and one tag on two paths pass. A missing, misheaded,
#   non-integer, duplicate-row or short ratchet file, a missing base file and
#   a bare --base-ratchet fail closed (exit 2). --final fails on any marker of
#   either kind and passes a table with none.
#
# Usage: bash scripts/check_engine_api_audit_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_engine_api_audit.sh"
PASSED=0
FAILED=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
K='trusty_memory::engine::store::kg_redb'
NL='
'

# The listing every case starts from: one kept struct with a field and a
# method, one kept enum with a variant, under the impl header that precedes it,
# and one crate-root fn.
cat > "$TMP/base.api" <<EOF
pub mod trusty_memory::engine::store::kg_redb
#[non_exhaustive] pub enum $K::Step
pub $K::Step::Copy
#[non_exhaustive] pub struct $K::Store
pub $K::Store::path: std::path::PathBuf
impl $K::Store
pub fn $K::Store::open<P: core::convert::AsRef<std::path::Path>>(P) -> std::io::Result<Self>
pub fn trusty_memory::socket_path() -> anyhow::Result<std::path::PathBuf>
EOF

# The table that matches base.api, plus one NARROW and one HIDE row. The
# anyhow-returning root fn carries its `anyhow-pending` marker (T0).
{
  printf 'path\tkind\tdisposition\tconsumer_or_reason\tbatch\n'
  printf '%s\tmod\tKEEP\ttests/a.rs\tA3a\n' "$K"
  printf '%s::Step\tenum\tKEEP\ttests/a.rs\tA3a\n' "$K"
  printf '%s::Step::Copy\tvariant\tKEEP\tmember of Step\tA3a\n' "$K"
  printf '%s::Store\tstruct\tKEEP\ttests/a.rs\tA3a\n' "$K"
  printf '%s::Store::path\tfield\tKEEP\ttests/a.rs\tA3a\n' "$K"
  printf '%s::Store::open\tmethod\tKEEP\ttests/a.rs\tA3a\n' "$K"
  printf '%s::Store::compact\tmethod\tNARROW\tno consumer\tA3a\n' "$K"
  printf '%s::LAYOUT\tconst\tHIDE\ttests/b.rs\tA3a\n' "$K"
  printf 'trusty_memory::socket_path\tfn\tKEEP\tanyhow-pending T4: trusty-agents tests\tA6\n'
} > "$TMP/base.tsv"

# gen_ratchet: a ratchet table whose initial and ceiling are the marker counts
# of case.tsv per kind and slice, so a case that is not about the ratchet
# passes it (T0). A case about the ratchet sets RATCHET_FIXTURE instead.
RATCHET_FIXTURE=''
gen_ratchet() {
  local src="$TMP/case.tsv"
  [ -f "$src" ] || src=/dev/null
  awk -F '\t' '
    BEGIN { print "marker\tslice\tinitial\tceiling" }
    NR > 1 && $4 ~ /^(anyhow|variant)-pending T[1-4]:/ {
      m = $4; sub(/-pending.*/, "", m)
      t = $4; sub(/^[a-z]+-pending /, "", t); c[m, substr(t, 1, 2)]++
    }
    END {
      split("anyhow variant", ms, " ")
      for (i = 1; i <= 2; i++) for (j = 1; j <= 4; j++) {
        k = c[ms[i], "T" j] + 0
        printf "%s\tT%d\t%d\t%d\n", ms[i], j, k, k
      }
    }' "$src" > "$TMP/ratchet.tsv"
}

# ratchet_file <file> <aT1> <aT2> <aT3> <aT4> <vT1> <vT2> <vT3> <vT4>: each
# argument is `initial:ceiling` for anyhow then variant, T1 to T4.
ratchet_file() {
  local f="$1" m t i=0 a
  shift
  printf 'marker\tslice\tinitial\tceiling\n' > "$f"
  for a in "$@"; do
    if [ "$i" -lt 4 ]; then m=anyhow; else m=variant; fi
    t=$((i % 4 + 1))
    printf '%s\tT%d\t%s\t%s\n' "$m" "$t" "${a%%:*}" "${a#*:}" >> "$f"
    i=$((i + 1))
  done
}

# run_case <name> <want-exit> <needle-or-empty> <listing> [gate args...]
run_case() {
  local name="$1" want="$2" needle="$3" listing="$4" out status rf
  shift 4
  status=0
  if [ -n "$RATCHET_FIXTURE" ]; then rf="$RATCHET_FIXTURE"; else gen_ratchet; rf="$TMP/ratchet.tsv"; fi
  out="$(ENGINE_API_AUDIT_TSV="$TMP/case.tsv" ENGINE_API_RATCHET_TSV="$rf" "$BASH" "$GATE" --api-file "$listing" "$@" 2>&1)" \
    || status=$?
  if [ "$status" -ne "$want" ]; then
    echo "FAIL $name: exit $status, want $want"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  # A `case` match, not `printf | grep -q`: grep -q exits early and pipefail
  # then reads the printf SIGPIPE as a failed match.
  if [ -n "$needle" ]; then
    case "$out" in
      *"$needle"*) ;;
      *)
        echo "FAIL $name: output lacks '$needle'"
        printf '%s\n' "$out" | sed 's/^/    /'
        FAILED=$((FAILED + 1))
        return
        ;;
    esac
  fi
  PASSED=$((PASSED + 1))
}

# with_line <name> <line...>: base.api plus the given lines, as $TMP/<name>.api.
with_line() {
  local name="$1"
  shift
  cp "$TMP/base.api" "$TMP/$name.api"
  printf '%s\n' "$@" >> "$TMP/$name.api"
}

cp "$TMP/base.tsv" "$TMP/case.tsv"
run_case matching 0 '7 audited path(s) match' "$TMP/base.api"

with_line narrow "pub fn $K::Store::compact(&self) -> anyhow::Result<()>"
run_case narrow-listed 1 "EXTRA $K::Store::compact" "$TMP/narrow.api"

with_line hide "pub const $K::LAYOUT: &str"
run_case hide-listed 1 "EXTRA $K::LAYOUT" "$TMP/hide.api"

with_line unaudited "pub fn $K::Store::stats(&self) -> u64"
run_case unaudited 1 "UNAUDITED $K::Store::stats" "$TMP/unaudited.api"

grep -v '::Store::open' "$TMP/base.api" > "$TMP/missing.api"
run_case keep-missing 1 "MISSING $K::Store::open" "$TMP/missing.api"

# A6 Q3: no scope list. A sibling module, a store-root item and a name that
# only shares a row's stem (`Store` versus `Storex`) all need rows of their own.
with_line sibling 'pub fn trusty_memory::engine::storex::other() -> u8'
run_case sibling-module-unaudited 1 'UNAUDITED trusty_memory::engine::storex::other' "$TMP/sibling.api"
with_line root 'pub struct trusty_memory::engine::store::NewRootReexport' 'pub mod trusty_memory::engine::store::brand_new_mod'
run_case store-root-unaudited 1 'UNAUDITED trusty_memory::engine::store::NewRootReexport' "$TMP/root.api"
run_case store-root-new-mod 1 'UNAUDITED trusty_memory::engine::store::brand_new_mod' "$TMP/root.api"
with_line stem "pub struct $K::Storex"
run_case shared-stem-unaudited 1 "UNAUDITED $K::Storex" "$TMP/stem.api"

# The crate root itself, a root fn and a transport constant are in scope. The
# root case pins the exact line and count, so `trusty_memory` cannot be
# satisfied by a longer path's row.
with_line crate-root 'pub mod trusty_memory'
run_case crate-root-unaudited 1 "UNAUDITED trusty_memory${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/crate-root.api"
grep -v '^trusty_memory::socket_path' "$TMP/base.tsv" > "$TMP/case.tsv"
run_case crate-root-fn-unaudited 1 'UNAUDITED trusty_memory::socket_path' "$TMP/base.api"
cp "$TMP/base.tsv" "$TMP/case.tsv"
with_line transport 'pub const trusty_memory::transport::uds::MAX_FRAME_BYTES: u64'
run_case transport-const-unaudited 1 'UNAUDITED trusty_memory::transport::uds::MAX_FRAME_BYTES' "$TMP/transport.api"

# A method on a generic in-scope type is a path of its own: `X<T>::m` must not
# collapse into the type's row (#9278), nested and lifetime groups included.
with_line generic "pub fn $K::Store<T>::generic_new(&self) -> u8" "pub fn $K::Store<'a, Vec<T>>::lifetimed(&self) -> u8"
run_case generic-type-unaudited 1 "UNAUDITED $K::Store::generic_new" "$TMP/generic.api"
run_case generic-type-nested-unaudited 1 "UNAUDITED $K::Store::lifetimed" "$TMP/generic.api"

# The generic shapes the listing prints, in an engine module (A5) and at the
# crate root (A6). Each line must cut to its own path: a lifetime-and-type
# generic fn, a trait-impl method whose bound nests a lifetime, an async_trait
# method with a `where` tail, a type alias whose right side carries `(`, `<`
# and `&`, and a method on a generic root type. With a row per path the gate
# passes; a wrong cut would show as UNAUDITED plus MISSING.
E='trusty_memory::engine'
R='trusty_memory'
cat > "$TMP/generic-shapes.api" <<EOF
pub async fn $E::timeouts::lock_with_timeout<'a, T>(&'a alloc::sync::Arc<tokio::sync::Mutex<T>>, core::time::Duration, &str) -> anyhow::Result<tokio::sync::MutexGuard<'a, T>>
pub fn $E::content_hash::ContentHash::deserialize<D: serde_core::de::Deserializer<'de>>(D) -> core::result::Result<Self, <D as serde_core::de::Deserializer>::Error>
pub fn $E::semantic_consolidation::Inference::consolidate<'life0, 'life1, 'async_trait>(&'life0 self, &'life1 [$E::palace::Drawer]) -> core::pin::Pin<alloc::boxed::Box<(dyn core::future::Future<Output = anyhow::Result<alloc::vec::Vec<u8>>> + core::marker::Send + 'async_trait)>> where Self: 'async_trait, 'life0: 'async_trait, 'life1: 'async_trait
pub type $E::dream::AfterCycle = alloc::sync::Arc<(dyn core::ops::function::Fn(&$E::palace::PalaceId, core::option::Option<&$E::dream::DreamStats>) + core::marker::Send + core::marker::Sync)>
pub fn $R::lock_root<'a, T: core::clone::Clone>(&'a T) -> &'a T
pub type $R::RootHook = alloc::sync::Arc<(dyn core::ops::function::Fn(&str) + core::marker::Send)>
pub fn $R::Frame<'a, alloc::vec::Vec<T>>::len(&self) -> usize
EOF
{
  printf 'path\tkind\tdisposition\tconsumer_or_reason\tbatch\n'
  printf '%s::timeouts::lock_with_timeout\tfn\tKEEP\tanyhow-pending T3: tests/a.rs\tA5\n' "$E"
  printf '%s::content_hash::ContentHash::deserialize\ttrait_method\tKEEP\ttests/a.rs\tA5\n' "$E"
  printf '%s::semantic_consolidation::Inference::consolidate\ttrait_method\tKEEP\tanyhow-pending T3: tests/a.rs\tA5\n' "$E"
  printf '%s::dream::AfterCycle\ttype\tKEEP\ttests/a.rs\tA5\n' "$E"
  printf '%s::lock_root\tfn\tKEEP\ttests/a.rs\tA6\n' "$R"
  printf '%s::RootHook\ttype\tKEEP\ttests/a.rs\tA6\n' "$R"
  printf '%s::Frame::len\tmethod\tKEEP\ttests/a.rs\tA6\n' "$R"
} > "$TMP/generic-shapes.tsv"
cp "$TMP/generic-shapes.tsv" "$TMP/case.tsv"
run_case generic-shapes-match 0 '7 audited path(s) match' "$TMP/generic-shapes.api"
head -n 1 "$TMP/generic-shapes.tsv" > "$TMP/case.tsv"
printf '%s::dream::Dreamer\tstruct\tKEEP\ttests/a.rs\tA5\n' "$E" >> "$TMP/case.tsv"
run_case generic-fn-unaudited 1 "UNAUDITED $E::timeouts::lock_with_timeout" "$TMP/generic-shapes.api"
run_case trait-bound-unaudited 1 "UNAUDITED $E::content_hash::ContentHash::deserialize" "$TMP/generic-shapes.api"
run_case async-trait-unaudited 1 "UNAUDITED $E::semantic_consolidation::Inference::consolidate" "$TMP/generic-shapes.api"
run_case type-alias-unaudited 1 "UNAUDITED $E::dream::AfterCycle" "$TMP/generic-shapes.api"
run_case root-generic-fn-unaudited 1 "UNAUDITED $R::lock_root" "$TMP/generic-shapes.api"
run_case root-type-alias-unaudited 1 "UNAUDITED $R::RootHook" "$TMP/generic-shapes.api"
run_case root-generic-type-unaudited 1 "UNAUDITED $R::Frame::len" "$TMP/generic-shapes.api"

# #9278 A1: a `->` inside generic arguments (a fn-pointer type or an `Fn`
# bound) must not end the generic group early and collapse the method onto
# its type's row; a generic group the strip cannot read fails closed.
cp "$TMP/base.tsv" "$TMP/case.tsv"
with_line fn-pointer "pub fn $K::Store<fn() -> u8>::hooked(&self)"
run_case fn-pointer-generic-unaudited 1 "UNAUDITED $K::Store::hooked" "$TMP/fn-pointer.api"
with_line fn-bound "pub fn $K::Store<F: Fn(u8) -> u8>::run(&self)"
run_case fn-bound-generic-unaudited 1 "UNAUDITED $K::Store::run" "$TMP/fn-bound.api"
with_line const-expr "pub fn $K::Store<{ N > 1 }>::m()"
run_case unreadable-generic-fails-closed 2 'unparsable listing line 9' "$TMP/const-expr.api"
printf '%s::Store::hooked\tmethod\tKEEP\ttests/a.rs\tA1\n' "$K" >> "$TMP/case.tsv"
run_case fn-pointer-generic-matches 0 '8 audited path(s) match' "$TMP/fn-pointer.api"

# #9278 A1 Q1: every public enum in the listing carries `#[non_exhaustive]`,
# read from the attribute prefix the listing prints. An unmarked enum fails at
# every path it is listed under (a re-export prints the enum again), and an
# attribute other than `#[non_exhaustive]` does not count.
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Mode\tenum\tKEEP\ttests/a.rs\tA1\n' "$K" >> "$TMP/case.tsv"
printf 'trusty_memory::engine::Mode\tenum\tKEEP\ttests/a.rs\tA1\n' >> "$TMP/case.tsv"
with_line unmarked "pub enum $K::Mode" 'pub enum trusty_memory::engine::Mode'
run_case unmarked-enum 1 "UNMARKED $K::Mode" "$TMP/unmarked.api"
run_case unmarked-reexported-enum 1 'UNMARKED trusty_memory::engine::Mode' "$TMP/unmarked.api"
with_line other-attr "#[repr(u8)] pub enum $K::Mode" '#[non_exhaustive] pub enum trusty_memory::engine::Mode'
run_case unmarked-enum-other-attribute 1 "UNMARKED $K::Mode${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/other-attr.api"
with_line marked "#[repr(u8)] #[non_exhaustive] pub enum $K::Mode" '#[non_exhaustive] pub enum trusty_memory::engine::Mode'
run_case marked-enums-match 0 '9 audited path(s) match' "$TMP/marked.api"
grep -v '::Mode' "$TMP/base.tsv" > "$TMP/case.tsv"
printf '%s::Mode\tenum\tKEEP\ttests/a.rs\tA1\n' "$K" >> "$TMP/case.tsv"
with_line generic-enum "pub enum $K::Mode<T>"
run_case unmarked-generic-enum 1 "UNMARKED $K::Mode" "$TMP/generic-enum.api"

# A field and a method sharing a name print one path; two rows that differ in
# kind and agree on public versus absent are one audited path.
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Store::path\tmethod\tKEEP\ttests/a.rs\tA3a\n' "$K" >> "$TMP/case.tsv"
with_line shared-name "pub fn $K::Store::path(&self) -> &std::path::Path"
run_case field-and-method-share-path 0 '7 audited path(s) match' "$TMP/shared-name.api"

# #9278 A2b: a public struct with a public field (named, or a `(pub ` tuple
# field) carries `#[non_exhaustive]`, unless its struct KEEP row's reason
# starts `exhaustive: `. The join runs after the whole listing, so line order
# does not matter. An opaque and a unit struct print alike and pass (known gap).
# struct_case <name> <rows> <line...>: base.api plus the lines as
# $TMP/<name>.api; base.tsv plus <rows> (kind=path, space-separated, KEEP).
struct_case() {
  local name="$1" rows="$2" r
  shift 2
  with_line "$name" "$@"
  cp "$TMP/base.tsv" "$TMP/case.tsv"
  for r in $rows; do
    printf '%s\t%s\tKEEP\ttests/a.rs\tA2b\n' "${r#*=}" "${r%%=*}" >> "$TMP/case.tsv"
  done
}
X="$K::Rec"
REC="struct=$X field=$X::n"
struct_case s-unmarked "$REC" "pub struct $X" "pub $X::n: u8"
run_case unmarked-struct-pub-field 1 "UNMARKED $X${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/s-unmarked.api"
struct_case s-marked "$REC" "#[non_exhaustive] pub struct $X" "pub $X::n: u8"
run_case marked-struct-passes 0 '9 audited path(s) match' "$TMP/s-marked.api"
struct_case s-reexport "$REC struct=trusty_memory::engine::Rec field=trusty_memory::engine::Rec::n" \
  "pub struct $X" "pub $X::n: u8" 'pub struct trusty_memory::engine::Rec' 'pub trusty_memory::engine::Rec::n: u8'
run_case unmarked-struct-reexport 1 "UNMARKED trusty_memory::engine::Rec${NL}UNMARKED $X${NL}check_engine_api_audit: FAIL — 2 path(s)" "$TMP/s-reexport.api"
struct_case s-field-first "$REC" "pub $X::n: u8" "pub struct $X"
run_case struct-field-before-struct-line 1 "UNMARKED $X" "$TMP/s-field-first.api"
struct_case s-opaque "struct=$K::Opaque method=$K::Opaque::len struct=$K::Unit" \
  "pub struct $K::Opaque" "impl $K::Opaque" "pub fn $K::Opaque::len(&self) -> usize" "pub struct $K::Unit"
run_case struct-opaque-and-unit-pass 0 '10 audited path(s) match' "$TMP/s-opaque.api"
struct_case s-tuple "struct=$K::Id" "pub struct $K::Id(pub alloc::string::String)"
run_case tuple-pub-field-unmarked 1 "UNMARKED $K::Id" "$TMP/s-tuple.api"
struct_case s-tuple-private "struct=$K::Hash" "pub struct $K::Hash(_)"
run_case tuple-private-field-passes 0 '8 audited path(s) match' "$TMP/s-tuple-private.api"
GEN="struct=$K::Wrap field=$K::Wrap::inner struct=$K::Tup"
struct_case s-generic "$GEN" "pub struct $K::Wrap<T>" "pub $K::Wrap::inner: T" "pub struct $K::Tup<'a, T: core::clone::Clone>(pub &'a T)"
run_case generic-struct-unmarked 1 "UNMARKED $K::Tup${NL}UNMARKED $K::Wrap${NL}check_engine_api_audit: FAIL — 2 path(s)" "$TMP/s-generic.api"
struct_case s-generic-marked "$GEN" "#[non_exhaustive] pub struct $K::Wrap<T>" "pub $K::Wrap::inner: T" "#[non_exhaustive] pub struct $K::Tup<'a, T: core::clone::Clone>(pub &'a T)"
run_case generic-struct-marked 0 '10 audited path(s) match' "$TMP/s-generic-marked.api"
EV="enum=$K::Ev variant=$K::Ev::Moved field=$K::Ev::Moved::from"
struct_case s-variant "$EV" \
  "#[non_exhaustive] pub enum $K::Ev" "#[non_exhaustive] pub $K::Ev::Moved" "pub $K::Ev::Moved::from: u8"
run_case variant-field-not-a-struct-field 0 '10 audited path(s) match' "$TMP/s-variant.api"
struct_case s-prefix "struct=$K::A struct=$K::A2 field=$K::A2::f" \
  "pub struct $K::A" "#[non_exhaustive] pub struct $K::A2" "pub $K::A2::f: u8"
run_case parent-prefix-not-confused 0 '10 audited path(s) match' "$TMP/s-prefix.api"

# The exemption: a struct KEEP row whose reason starts `exhaustive: `. A
# stale or misplaced one fails closed (exit 2), so a row cannot outlive the
# shape it excuses.
exempt_row() { printf '%s\t%s\t%s\texhaustive: shape is the contract\tA2b\n' "$1" "$2" "$3" >> "$TMP/case.tsv"; }
struct_case s-exempt "field=$X::n" "pub struct $X" "pub $X::n: u8" "pub struct $K::Id(pub alloc::string::String)"
exempt_row "$X" struct KEEP
exempt_row "$K::Id" struct KEEP
run_case exempt-row-passes 0 '10 audited path(s) match' "$TMP/s-exempt.api"
struct_case s-exempt-marked "field=$X::n" "#[non_exhaustive] pub struct $X" "pub $X::n: u8"
exempt_row "$X" struct KEEP
run_case exempt-row-on-marked-struct 2 "stale exemption $X: the struct is #[non_exhaustive]" "$TMP/s-exempt-marked.api"
struct_case s-exempt-opaque '' "pub struct $X"
exempt_row "$X" struct KEEP
run_case exempt-row-on-opaque-struct 2 "stale exemption $X: the struct has no public field" "$TMP/s-exempt-opaque.api"
cp "$TMP/base.tsv" "$TMP/case.tsv"
exempt_row "$K::Gone" struct KEEP
run_case exempt-row-on-unlisted-path 2 "stale exemption $K::Gone: not a listed struct" "$TMP/base.api"
cp "$TMP/base.tsv" "$TMP/case.tsv"
exempt_row "$K::Store::path" method KEEP
exempt_row "$K::Gone" struct NARROW
run_case exempt-prefix-on-non-struct-kind 2 "stale exemption $K::Store::path: exhaustive: is only for a struct KEEP row (method KEEP)${NL}check_engine_api_audit: stale exemption $K::Gone: exhaustive: is only for a struct KEEP row (struct NARROW)" "$TMP/base.api"
struct_case s-no-prefix '' "pub struct $K::Id(pub alloc::string::String)"
printf '%s\tstruct\tKEEP\tnewtype; exhaustive: by design\tA2b\n' "$K::Id" >> "$TMP/case.tsv"
run_case exempt-reason-without-prefix 1 "UNMARKED $K::Id" "$TMP/s-no-prefix.api"

# #9278 A2c: an `exhaustive:` row needs a reason after the colon. An empty or
# blank reason fails closed (exit 2) whether or not the struct is marked.
empty_exempt_row() { printf '%s\tstruct\tKEEP\t%s\tA2c\n' "$X" "$1" >> "$TMP/case.tsv"; }
struct_case s-empty-reason "field=$X::n" "pub struct $X" "pub $X::n: u8"
empty_exempt_row 'exhaustive: '
run_case exempt-row-empty-reason 2 "stale exemption $X: exhaustive: needs a reason" "$TMP/s-empty-reason.api"
struct_case s-blank-reason "field=$X::n" "pub struct $X" "pub $X::n: u8"
empty_exempt_row 'exhaustive:    '
run_case exempt-row-blank-reason 2 "stale exemption $X: exhaustive: needs a reason" "$TMP/s-blank-reason.api"
struct_case s-bare-prefix "field=$X::n" "pub struct $X" "pub $X::n: u8"
empty_exempt_row 'exhaustive:'
run_case exempt-row-bare-prefix 2 "stale exemption $X: exhaustive: needs a reason" "$TMP/s-bare-prefix.api"
struct_case s-empty-reason-marked "field=$X::n" "#[non_exhaustive] pub struct $X" "pub $X::n: u8"
empty_exempt_row 'exhaustive: '
run_case exempt-row-empty-reason-on-marked-struct 2 "stale exemption $X: the struct is #[non_exhaustive]" "$TMP/s-empty-reason-marked.api"

# #9278 A2c: the tuple check reads every field, not only the first.
struct_case s-tuple-later "struct=$K::Pair struct=$K::Triple" \
  "pub struct $K::Pair(_, pub u8)" "pub struct $K::Triple(_, _, pub alloc::string::String)"
run_case tuple-later-pub-field-unmarked 1 "UNMARKED $K::Pair${NL}UNMARKED $K::Triple${NL}check_engine_api_audit: FAIL — 2 path(s)" "$TMP/s-tuple-later.api"
struct_case s-tuple-all-private "struct=$K::Pair" "pub struct $K::Pair(_, _)"
run_case tuple-all-private-fields-pass 0 '8 audited path(s) match' "$TMP/s-tuple-all-private.api"

# #9278 A2c: a struct-like enum variant (a variant line with field lines under
# it) carries `#[non_exhaustive]`, at every path it is listed under. Tuple and
# unit variants print no field line and pass. The join runs in END, so a field
# line before its variant line still counts.
struct_case v-unmarked "$EV" "#[non_exhaustive] pub enum $K::Ev" "pub $K::Ev::Moved" "pub $K::Ev::Moved::from: u8"
run_case unmarked-struct-variant 1 "UNMARKED $K::Ev::Moved${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/v-unmarked.api"
struct_case v-reexport "$EV enum=trusty_memory::engine::Ev variant=trusty_memory::engine::Ev::Moved field=trusty_memory::engine::Ev::Moved::from" \
  "#[non_exhaustive] pub enum $K::Ev" "#[non_exhaustive] pub $K::Ev::Moved" "pub $K::Ev::Moved::from: u8" \
  '#[non_exhaustive] pub enum trusty_memory::engine::Ev' 'pub trusty_memory::engine::Ev::Moved' 'pub trusty_memory::engine::Ev::Moved::from: u8'
run_case unmarked-struct-variant-reexport 1 "UNMARKED trusty_memory::engine::Ev::Moved${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/v-reexport.api"
struct_case v-field-first "$EV" "#[non_exhaustive] pub enum $K::Ev" "pub $K::Ev::Moved::from: u8" "pub $K::Ev::Moved"
run_case struct-variant-field-before-variant-line 1 "UNMARKED $K::Ev::Moved" "$TMP/v-field-first.api"
struct_case v-tuple-unit "enum=$K::Ev variant=$K::Ev::Pair variant=$K::Ev::Done" \
  "#[non_exhaustive] pub enum $K::Ev" "pub $K::Ev::Pair(u8, alloc::string::String)" "pub $K::Ev::Done"
run_case tuple-and-unit-variants-pass 0 '10 audited path(s) match' "$TMP/v-tuple-unit.api"
struct_case v-other-attr "$EV" "#[non_exhaustive] pub enum $K::Ev" "#[doc(alias = \"m\")] pub $K::Ev::Moved" "pub $K::Ev::Moved::from: u8"
run_case struct-variant-other-attribute-unmarked 1 "UNMARKED $K::Ev::Moved" "$TMP/v-other-attr.api"

# #9278 A2c ruling: an unmarked struct-like variant may carry a pending marker
# on its variant KEEP row, `variant-pending T<n>: <reason>` with n in 1-4: the
# slice that marks it. The marker is the only disposition. A stale (variant
# marked, unlisted, or not struct-like), misplaced or malformed marker fails
# closed (exit 2). Each case is base.api plus the lines; base.tsv plus <rows>
# (KEEP, as struct_case) plus one marker row.
# pending_case <name> <rows> <marker-path> <kind> <disp> <reason> <line...>
pending_case() {
  local name="$1" rows="$2" mp="$3" kind="$4" disp="$5" reason="$6"
  shift 6
  struct_case "$name" "$rows" "$@"
  printf '%s\t%s\t%s\t%s\tA2c\n' "$mp" "$kind" "$disp" "$reason" >> "$TMP/case.tsv"
}
MV="$K::Ev::Moved"
EVF="enum=$K::Ev field=$MV::from"
EVL_U="#[non_exhaustive] pub enum $K::Ev"
pending_case p-ok "$EVF" "$MV" variant KEEP 'variant-pending T3: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8"
run_case variant-pending-marker-passes 0 '10 audited path(s) match' "$TMP/p-ok.api"
pending_case p-marked "$EVF" "$MV" variant KEEP 'variant-pending T3: member of Ev' \
  "$EVL_U" "#[non_exhaustive] pub $MV" "pub $MV::from: u8"
run_case variant-pending-on-marked-variant 2 "stale pending $MV: the variant is #[non_exhaustive]" "$TMP/p-marked.api"
pending_case p-no-tier "$EVF" "$MV" variant KEEP 'variant-pending: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8"
run_case variant-pending-without-tier 2 "stale pending $MV: variant-pending needs a T1-T4 tag and a reason" "$TMP/p-no-tier.api"
pending_case p-bad-tier "$EVF" "$MV" variant KEEP 'variant-pending T5: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8"
run_case variant-pending-tier-out-of-range 2 "stale pending $MV: variant-pending needs a T1-T4 tag and a reason" "$TMP/p-bad-tier.api"
pending_case p-no-reason "$EVF" "$MV" variant KEEP 'variant-pending T2: ' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8"
run_case variant-pending-empty-reason 2 "stale pending $MV: variant-pending needs a T1-T4 tag and a reason" "$TMP/p-no-reason.api"
pending_case p-unlisted '' "$K::Ev::Gone" variant KEEP 'variant-pending T1: gone'
run_case variant-pending-on-unlisted-path 2 "stale pending $K::Ev::Gone: not a listed variant" "$TMP/p-unlisted.api"
pending_case p-tuple "enum=$K::Ev" "$K::Ev::Pair" variant KEEP 'variant-pending T1: member of Ev' \
  "$EVL_U" "pub $K::Ev::Pair(u8)"
run_case variant-pending-on-tuple-variant 2 "stale pending $K::Ev::Pair: the variant has no field line" "$TMP/p-tuple.api"
pending_case p-kind "field=$X::n" "$X" struct KEEP 'variant-pending T1: not a variant' \
  "pub struct $X" "pub $X::n: u8"
run_case variant-pending-on-non-variant-kind 2 "stale pending $X: variant-pending is only for a variant KEEP row (struct KEEP)" "$TMP/p-kind.api"
pending_case p-narrow '' "$K::Ev::Old" variant NARROW 'variant-pending T1: narrowed'
run_case variant-pending-on-narrow-row 2 "stale pending $K::Ev::Old: variant-pending is only for a variant KEEP row (variant NARROW)" "$TMP/p-narrow.api"
pending_case p-reexport "$EVF enum=trusty_memory::engine::Ev variant=trusty_memory::engine::Ev::Moved field=trusty_memory::engine::Ev::Moved::from" \
  "$MV" variant KEEP 'variant-pending T3: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8" \
  '#[non_exhaustive] pub enum trusty_memory::engine::Ev' 'pub trusty_memory::engine::Ev::Moved' 'pub trusty_memory::engine::Ev::Moved::from: u8'
run_case variant-pending-one-path-of-two 1 "UNMARKED trusty_memory::engine::Ev::Moved${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/p-reexport.api"

# #9278 A1 LOW gaps, for enums and structs: an attribute glued to `pub`
# fails closed, and `#[non_exhaustive]` counts in either attribute order.
struct_case glued-enum "enum=$K::Mode" "#[non_exhaustive]pub enum $K::Mode"
run_case attr-without-trailing-space-enum 2 'unparsable listing line 9' "$TMP/glued-enum.api"
struct_case glued-struct "$REC" "#[non_exhaustive]pub struct $X" "pub $X::n: u8"
run_case attr-without-trailing-space-struct 2 'unparsable listing line 9' "$TMP/glued-struct.api"
struct_case multi-enum "enum=$K::Mode enum=$K::Mode2" \
  "#[repr(u8)] #[non_exhaustive] pub enum $K::Mode" "#[non_exhaustive] #[repr(u8)] pub enum $K::Mode2"
run_case multi-attr-both-orders-enum 0 '9 audited path(s) match' "$TMP/multi-enum.api"
struct_case multi-struct "$REC struct=$K::Rec2 field=$K::Rec2::n" \
  "#[repr(C)] #[non_exhaustive] pub struct $X" "pub $X::n: u8" "#[non_exhaustive] #[repr(C)] pub struct $K::Rec2" "pub $K::Rec2::n: u8"
run_case multi-attr-both-orders-struct 0 '11 audited path(s) match' "$TMP/multi-struct.api"
struct_case s-other-attr "$REC enum=$K::Mode" "#[repr(C)] pub struct $X" "pub $X::n: u8" "#[repr(u8)] pub enum $K::Mode"
run_case other-attr-only-unmarked 1 "UNMARKED $K::Mode${NL}UNMARKED $X${NL}check_engine_api_audit: FAIL — 2 path(s)" "$TMP/s-other-attr.api"

# Listing errors.
cp "$TMP/base.tsv" "$TMP/case.tsv"
: > "$TMP/empty.api"
run_case empty-listing 2 'empty listing' "$TMP/empty.api"
run_case missing-listing 2 'no listing at' "$TMP/absent.api"
echo 'pub fn other_crate::socket_path() -> u8' > "$TMP/crateless.api"
run_case crate-less-listing 2 'unparsable listing line 1' "$TMP/crateless.api"
echo 'impl other_crate::Thing' > "$TMP/impl-only.api"
run_case no-crate-path 2 'no trusty_memory path' "$TMP/impl-only.api"
with_line unparsable 'pub auto trait trusty_memory::engine::Marker'
run_case unparsable-pub-line 2 'unparsable listing line 9' "$TMP/unparsable.api"
with_line stray 'trusty_memory::engine::NoKeyword'
run_case non-pub-line 2 'unparsable listing line 9' "$TMP/stray.api"

run_case bad-argument 2 'unknown argument' "$TMP/base.api" --bogus

# Table errors. A missing table and a header-only table pass over nothing.
rm -f "$TMP/case.tsv"
run_case missing-table 2 'no audit table at' "$TMP/base.api"
head -n 1 "$TMP/base.tsv" > "$TMP/case.tsv"
run_case header-only-table 2 'no data row' "$TMP/base.api"

# #9278 A1: line 1 must be the exact header. Without the check a deleted
# header silently drops the first data row from validation and from the sets.
tail -n +2 "$TMP/base.tsv" > "$TMP/case.tsv"
run_case header-deleted-keep-first 2 'bad header at line 1' "$TMP/base.api"
{
  grep '::Store::compact' "$TMP/base.tsv"
  tail -n +2 "$TMP/base.tsv" | grep -v '::Store::compact'
} > "$TMP/case.tsv"
run_case header-deleted-narrow-first 2 'bad header at line 1' "$TMP/base.api"
{
  printf 'path\tkind\tdisposition\treason\tbatch\n'
  tail -n +2 "$TMP/base.tsv"
} > "$TMP/case.tsv"
run_case header-misspelled 2 'bad header at line 1' "$TMP/base.api"
{
  printf 'path\tkind\tdisposition\tconsumer_or_reason\tbatch\tnote\n'
  tail -n +2 "$TMP/base.tsv"
} > "$TMP/case.tsv"
run_case header-extra-column 2 'bad header at line 1' "$TMP/base.api"
{
  printf 'path\tkind\tdisposition\tconsumer_or_reason\tbatch\r\n'
  tail -n +2 "$TMP/base.tsv"
} > "$TMP/case.tsv"
run_case header-crlf 2 'bad header at line 1' "$TMP/base.api"

# Malformed rows fail closed (exit 2, naming the line). Each case drops the
# Store::open row from the base table and appends one bad row as line 10.
OPEN_ROW="$K::Store::open"
bad_row_case() {
  local name="$1" row="$2" want="$3" needle="$4"
  grep -v '::Store::open' "$TMP/base.tsv" > "$TMP/case.tsv"
  printf '%s\n' "$row" >> "$TMP/case.tsv"
  run_case "$name" "$want" "$needle" "$TMP/base.api"
}
bad_row_case unknown-disposition "$(printf '%s\tmethod\tnarrow\tnone\tA3a' "$OPEN_ROW")" 2 'line 10'
bad_row_case mixed-case-disposition "$(printf '%s\tmethod\tKeep\ttests/a.rs\tA3a' "$OPEN_ROW")" 2 'line 10'
bad_row_case space-separated-row "$OPEN_ROW method KEEP tests/a.rs A3a" 2 'line 10'
bad_row_case four-field-row "$(printf '%s\tmethod\tKEEP\ttests/a.rs' "$OPEN_ROW")" 2 'line 10'
bad_row_case empty-reason "$(printf '%s\tmethod\tKEEP\t\tA3a' "$OPEN_ROW")" 2 'line 10'
bad_row_case unknown-kind "$(printf '%s\tfunction\tKEEP\ttests/a.rs\tA3a' "$OPEN_ROW")" 2 'line 10'
bad_row_case path-outside-crate "$(printf 'trusty_common::open\tfn\tKEEP\ttests/a.rs\tA3a')" 2 'line 10'
bad_row_case path-with-generics "$(printf '%s::Store<T>::open\tmethod\tKEEP\ttests/a.rs\tA3a' "$K")" 2 'line 10'

# One row per path and kind; one path is public or absent, never both.
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Store::open\tmethod\tKEEP\ttests/c.rs\tA6\n' "$K" >> "$TMP/case.tsv"
run_case duplicate-row 2 "duplicate row for $K::Store::open (method) at lines 7 and 11" "$TMP/base.api"
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Store::path\tmethod\tNARROW\tno consumer\tA6\n' "$K" >> "$TMP/case.tsv"
run_case conflicting-rows 2 "conflicting rows for $K::Store::path" "$TMP/base.api"

# A DEPRECATE row is public: absent from the listing it is MISSING.
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Store::old\tmethod\tDEPRECATE\ttests/a.rs\tA3a\n' "$K" >> "$TMP/case.tsv"
run_case deprecate-missing 1 "MISSING $K::Store::old" "$TMP/base.api"

# Cases 101 on (#9278 T0). 101: a pending marker on a unit variant is stale.
pending_case p-unit "enum=$K::Ev" "$K::Ev::Done" variant KEEP 'variant-pending T1: member of Ev' \
  "$EVL_U" "pub $K::Ev::Done"
run_case variant-pending-on-unit-variant 2 "stale pending $K::Ev::Done: the variant has no field line" "$TMP/p-unit.api"

# 102-110: a listed path whose line holds the token `anyhow::` (return,
# argument, type alias, field, the async-trait shape, each re-export path)
# needs `anyhow-pending T<n>: <reason>` on its KEEP row, else ANYHOW. A word
# that only contains `anyhow` is not the token.
LOAD="$K::Store::load"
LOADL="pub fn $LOAD(&self) -> anyhow::Result<u8>"
AP2='anyhow-pending T2: tests/a.rs'
struct_case a-ret "method=$LOAD" "$LOADL"
run_case anyhow-return-unmarked 1 "ANYHOW $LOAD${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/a-ret.api"
pending_case a-ok '' "$LOAD" method KEEP "$AP2" "$LOADL"
run_case anyhow-return-pending-marker 0 '8 audited path(s) match' "$TMP/a-ok.api"
struct_case a-arg "method=$K::Store::feed" "pub fn $K::Store::feed(&self, anyhow::Error) -> u8"
run_case anyhow-in-argument 1 "ANYHOW $K::Store::feed" "$TMP/a-arg.api"
struct_case a-type "type=$K::Hook" "pub type $K::Hook = alloc::sync::Arc<(dyn core::ops::function::Fn(u8) -> anyhow::Result<()> + core::marker::Send)>"
run_case anyhow-in-type-alias 1 "ANYHOW $K::Hook" "$TMP/a-type.api"
struct_case a-field "field=$K::Store::err" "pub $K::Store::err: anyhow::Error"
run_case anyhow-in-field-type 1 "ANYHOW $K::Store::err" "$TMP/a-field.api"
struct_case a-trait "trait=$K::Vs trait_method=$K::Vs::upsert" "pub trait $K::Vs" \
  "pub fn $K::Vs::upsert<'life0, 'async_trait>(&'life0 self, uuid::Uuid, alloc::vec::Vec<f32>) -> core::pin::Pin<alloc::boxed::Box<(dyn core::future::future::Future<Output = anyhow::Result<()>> + core::marker::Send + 'async_trait)>> where Self: 'async_trait, 'life0: 'async_trait"
run_case anyhow-in-async-trait-shape 1 "ANYHOW $K::Vs::upsert" "$TMP/a-trait.api"
pending_case a-two "method=trusty_memory::engine::Store::load" "$LOAD" method KEEP "$AP2" \
  "$LOADL" 'pub fn trusty_memory::engine::Store::load(&self) -> anyhow::Result<u8>'
run_case anyhow-reexport-two-paths-one-marked 1 "ANYHOW trusty_memory::engine::Store::load${NL}check_engine_api_audit: FAIL — 1 path(s)" "$TMP/a-two.api"
struct_case a-word "method=$K::Store::a method=$K::Store::b" \
  "pub fn $K::Store::a(&self) -> not_anyhow::Result<u8>" "pub fn $K::Store::b(&self) -> anyhow_like::Error"
run_case anyhow-substring-not-token 0 '9 audited path(s) match' "$TMP/a-word.api"
pending_case a-field-ok '' "$K::Store::err" field KEEP "$AP2" "pub $K::Store::err: anyhow::Error"
run_case anyhow-marker-on-field-passes 0 '8 audited path(s) match' "$TMP/a-field-ok.api"

# 111-119: a stale, misplaced or malformed `anyhow-pending` marker fails
# closed (exit 2), so a converted path cannot keep its marker.
pending_case a-clean '' "$K::Store::size" method KEEP 'anyhow-pending T1: tests/a.rs' "pub fn $K::Store::size(&self) -> u64"
run_case anyhow-marker-on-clean-path 2 "stale pending $K::Store::size: the path holds no anyhow" "$TMP/a-clean.api"
pending_case a-unlisted '' "$K::Store::gone" method KEEP 'anyhow-pending T1: gone'
run_case anyhow-marker-on-unlisted-path 2 "stale pending $K::Store::gone: not a listed path" "$TMP/a-unlisted.api"
AKINDS='anyhow-pending is only for a KEEP row of kind fn, method, trait_method, type or field'
pending_case a-narrow '' "$K::Store::old" method NARROW 'anyhow-pending T1: narrowed'
run_case anyhow-marker-on-narrow-row 2 "stale pending $K::Store::old: $AKINDS (method NARROW)" "$TMP/a-narrow.api"
pending_case a-variant "enum=$K::Ev" "$K::Ev::Fail" variant KEEP 'anyhow-pending T1: member of Ev' \
  "$EVL_U" "pub $K::Ev::Fail(anyhow::Error)"
run_case anyhow-marker-on-variant-kind 2 "stale pending $K::Ev::Fail: $AKINDS (variant KEEP)" "$TMP/a-variant.api"
ATAG="stale pending $LOAD: anyhow-pending needs a T1-T4 tag and a reason"
for bad in 'no-tier=anyhow-pending: tests/a.rs' 'tier-out-of-range=anyhow-pending T5: tests/a.rs' \
  'empty-reason=anyhow-pending T2: ' 'blank-reason=anyhow-pending T2:    ' 'glued-tag=anyhow-pendingT2: tests/a.rs'; do
  pending_case "a-${bad%%=*}" '' "$LOAD" method KEEP "${bad#*=}" "$LOADL"
  run_case "anyhow-marker-${bad%%=*}" 2 "$ATAG" "$TMP/a-${bad%%=*}.api"
done

# 120-123: the ratchet. The table's marker count per kind and slice equals
# the ceiling (R1), and the ceiling never exceeds the initial count (R2).
# Every fixture keeps anyhow T4 at 1:1 for the socket_path marker in base.tsv.
pending_case a-r1 '' "$LOAD" method KEEP "$AP2" "$LOADL"
RATCHET_FIXTURE="$TMP/r.tsv"
ratchet_file "$RATCHET_FIXTURE" 0:0 1:1 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-count-equals-ceiling 0 '8 audited path(s) match' "$TMP/a-r1.api"
pending_case a-r2 '' "$LOAD" method KEEP "$AP2" "$LOADL" "pub fn $K::Store::load2(&self) -> anyhow::Result<u8>"
printf '%s::Store::load2\tmethod\tKEEP\t%s\tT0\n' "$K" "$AP2" >> "$TMP/case.tsv"
run_case ratchet-table-above-ceiling 1 "RATCHET anyhow T2: table has 2, ceiling 1" "$TMP/a-r2.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 1:2 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-ceiling-above-initial 1 "RATCHET anyhow T2: ceiling 2 above initial 1${NL}check_engine_api_audit: FAIL — 0 path(s) and 1 ratchet line(s)" "$TMP/a-r2.api"
cp "$TMP/base.tsv" "$TMP/case.tsv"
ratchet_file "$RATCHET_FIXTURE" 0:0 1:1 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-table-below-ceiling 1 "RATCHET anyhow T2: table has 0, ceiling 1" "$TMP/base.api"

# 124-129: --base-ratchet compares with the base branch's ratchet file. A
# ceiling may only fall and `initial` never changes (R3); a slice shows
# progress only while every earlier slice is at zero for both kinds (R4).
pending_case a-r4 '' "$LOAD" method KEEP "$AP2" "$LOADL"
ratchet_file "$RATCHET_FIXTURE" 0:0 1:1 0:0 1:1 0:0 0:0 0:0 0:0
ratchet_file "$TMP/base-r.tsv" 0:0 1:0 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-base-increase 1 "RATCHET anyhow T2: ceiling 1 above base 0" "$TMP/a-r4.api" --base-ratchet "$TMP/base-r.tsv"
ratchet_file "$RATCHET_FIXTURE" 0:0 2:1 0:0 1:1 0:0 0:0 0:0 0:0
ratchet_file "$TMP/base-r.tsv" 0:0 2:2 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-base-equal-or-lower 0 '8 audited path(s) match' "$TMP/a-r4.api" --base-ratchet "$TMP/base-r.tsv"
ratchet_file "$TMP/base-r.tsv" 0:0 3:2 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-base-initial-changed 1 "RATCHET anyhow T2: initial 2 differs from base 3" "$TMP/a-r4.api" --base-ratchet "$TMP/base-r.tsv"
pending_case a-r5 '' "$LOAD" method KEEP "$AP2" "$LOADL" "pub fn $K::Store::load2(&self) -> anyhow::Result<u8>"
printf '%s::Store::load2\tmethod\tKEEP\tanyhow-pending T1: tests/a.rs\tT0\n' "$K" >> "$TMP/case.tsv"
ratchet_file "$RATCHET_FIXTURE" 1:1 2:1 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-order-later-slice-moves-first 1 "RATCHET order: T1 is open while T2 has progress" "$TMP/a-r5.api"
ratchet_file "$RATCHET_FIXTURE" 1:1 1:1 0:0 1:1 0:0 0:0 3:0 0:0
run_case ratchet-order-variant-later-slice 1 "RATCHET order: T1 is open while T3 has progress" "$TMP/a-r5.api"
pending_case a-r6 '' "$LOAD" method KEEP "$AP2" "$LOADL"
ratchet_file "$RATCHET_FIXTURE" 1:0 2:1 0:0 1:1 0:0 0:0 0:0 0:0
run_case ratchet-order-earlier-slice-zero 0 '8 audited path(s) match' "$TMP/a-r6.api"
RATCHET_FIXTURE=''

# 130-131: every `variant-pending` row of one enum (keyed by its name, so a
# re-export path counts) carries one tag.
EVR="$EVF enum=trusty_memory::engine::Ev field=trusty_memory::engine::Ev::Moved::from"
pending_case v-mixed "$EVR" "$MV" variant KEEP 'variant-pending T1: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8" \
  '#[non_exhaustive] pub enum trusty_memory::engine::Ev' 'pub trusty_memory::engine::Ev::Moved' 'pub trusty_memory::engine::Ev::Moved::from: u8'
printf 'trusty_memory::engine::Ev::Moved\tvariant\tKEEP\tvariant-pending T3: member of Ev\tA2c\n' >> "$TMP/case.tsv"
run_case variant-mixed-tags-one-enum-two-paths 1 "RATCHET enum Ev: mixed tags T1 at $MV, T3 at trusty_memory::engine::Ev::Moved" "$TMP/v-mixed.api"
pending_case v-one "$EVR" "$MV" variant KEEP 'variant-pending T3: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8" \
  '#[non_exhaustive] pub enum trusty_memory::engine::Ev' 'pub trusty_memory::engine::Ev::Moved' 'pub trusty_memory::engine::Ev::Moved::from: u8'
printf 'trusty_memory::engine::Ev::Moved\tvariant\tKEEP\tvariant-pending T3: member of Ev\tA2c\n' >> "$TMP/case.tsv"
run_case variant-one-tag-two-paths 0 '13 audited path(s) match' "$TMP/v-one.api"

# 132-138: the ratchet file and the flags fail closed (exit 2).
cp "$TMP/base.tsv" "$TMP/case.tsv"
RATCHET_FIXTURE="$TMP/r.tsv"
rm -f "$RATCHET_FIXTURE"
run_case ratchet-file-missing 2 'no ratchet table at' "$TMP/base.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 0:0 0:0 1:1 0:0 0:0 0:0 0:0
sed '1s/ceiling/limit/' "$RATCHET_FIXTURE" > "$TMP/r-bad.tsv"
mv "$TMP/r-bad.tsv" "$RATCHET_FIXTURE"
run_case ratchet-file-bad-header 2 'bad ratchet header at line 1' "$TMP/base.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 0:0 0:x 1:1 0:0 0:0 0:0 0:0
run_case ratchet-file-non-integer 2 'malformed ratchet row at line 4' "$TMP/base.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 0:0 0:0 1:1 0:0 0:0 0:0 0:0
printf 'anyhow\tT2\t0\t0\n' >> "$RATCHET_FIXTURE"
run_case ratchet-file-duplicate-row 2 'duplicate ratchet row anyhow T2' "$TMP/base.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 0:0 0:0 1:1 0:0 0:0 0:0
run_case ratchet-file-missing-row 2 'ratchet table lacks variant T4' "$TMP/base.api"
ratchet_file "$RATCHET_FIXTURE" 0:0 0:0 0:0 1:1 0:0 0:0 0:0 0:0
run_case base-ratchet-file-missing 2 'no ratchet table at' "$TMP/base.api" --base-ratchet "$TMP/no-base.tsv"
run_case base-ratchet-without-path 2 '--base-ratchet needs a path' "$TMP/base.api" --base-ratchet
RATCHET_FIXTURE=''

# 139-141: --final (the T4 end state) fails on any pending marker of either
# kind, and passes a table with none.
pending_case a-final '' "$LOAD" method KEEP "$AP2" "$LOADL"
run_case final-fails-on-anyhow-marker 1 "PENDING $LOAD: anyhow-pending T2${NL}PENDING trusty_memory::socket_path: anyhow-pending T4" "$TMP/a-final.api" --final
pending_case p-ok "$EVF" "$MV" variant KEEP 'variant-pending T3: member of Ev' \
  "$EVL_U" "pub $MV" "pub $MV::from: u8"
run_case final-fails-on-variant-marker 1 "PENDING $MV: variant-pending T3" "$TMP/p-ok.api" --final
grep -v 'socket_path' "$TMP/base.api" > "$TMP/final-clean.api"
grep -v 'socket_path' "$TMP/base.tsv" > "$TMP/case.tsv"
run_case final-passes-with-no-marker 0 '6 audited path(s) match' "$TMP/final-clean.api" --final

echo "check_engine_api_audit_selftest: $PASSED passed, $FAILED failed (bash $BASH_VERSION)"
[ "$FAILED" -eq 0 ]

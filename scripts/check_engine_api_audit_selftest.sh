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
#   attribute only).
#   Passing cases: a matching listing whose lines carry an attribute prefix, a
#   generic fn, a field, a variant and an impl header; the generic shapes
#   against a matching table; a fn-pointer generic method with its row; marked
#   enums; a field and a method sharing one path.
#   Errors (exit 2, naming the line or path): a missing, empty, crate-less or
#   unparsable listing (a generic group the strip cannot read included); a
#   missing or header-only table; a header line that is deleted, misspelled,
#   six columns wide or CRLF-terminated; a malformed row (an
#   unknown or mixed-case disposition, a space-separated row, a 4-field row,
#   an empty field, an unknown kind, a path outside the crate or carrying
#   generics); a duplicate path-and-kind row; a path with a public and an
#   absent row; a bad argument.
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
pub struct $K::Store
pub $K::Store::path: std::path::PathBuf
impl $K::Store
pub fn $K::Store::open<P: core::convert::AsRef<std::path::Path>>(P) -> anyhow::Result<Self>
pub fn trusty_memory::socket_path() -> anyhow::Result<std::path::PathBuf>
EOF

# The table that matches base.api, plus one NARROW and one HIDE row.
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
  printf 'trusty_memory::socket_path\tfn\tKEEP\ttrusty-agents tests\tA6\n'
} > "$TMP/base.tsv"

# run_case <name> <want-exit> <needle-or-empty> <listing> [gate args...]
run_case() {
  local name="$1" want="$2" needle="$3" listing="$4" out status
  shift 4
  status=0
  out="$(ENGINE_API_AUDIT_TSV="$TMP/case.tsv" "$BASH" "$GATE" --api-file "$listing" "$@" 2>&1)" \
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
  printf '%s::timeouts::lock_with_timeout\tfn\tKEEP\ttests/a.rs\tA5\n' "$E"
  printf '%s::content_hash::ContentHash::deserialize\ttrait_method\tKEEP\ttests/a.rs\tA5\n' "$E"
  printf '%s::semantic_consolidation::Inference::consolidate\ttrait_method\tKEEP\ttests/a.rs\tA5\n' "$E"
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

echo "check_engine_api_audit_selftest: $PASSED passed, $FAILED failed (bash $BASH_VERSION)"
[ "$FAILED" -eq 0 ]

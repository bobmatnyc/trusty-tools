#!/usr/bin/env bash
#
# check_engine_api_audit_selftest.sh — self-test for
# scripts/check_engine_api_audit.sh (issue #9278).
#
# Why: a gate whose parser silently stopped reading paths reports OK exactly
#   like a matching listing (#4618). Its ability to fail is tested here.
# What: each case writes a small `cargo public-api` listing and audit table to
#   a temp dir and runs the gate with `--api-file` and ENGINE_API_AUDIT_TSV.
#   Failing cases: a NARROW row still listed, a HIDE row still listed, a listed
#   path with no row (including one under an engine-root re-export stem, or a
#   method on a generic type), a
#   KEEP row that is not listed. Passing cases: a matching
#   listing whose lines carry an attribute prefix, a generic fn, a field, a
#   variant and an impl header; an out-of-scope path and a path that only
#   shares a scope prefix's stem, both with no row. Errors: an empty listing,
#   a listing with no in-scope path, and a bad argument. Malformed table rows
#   (an unknown or mixed-case disposition, a space-separated row, a 4-field
#   row) are exit 2 naming the line; a DEPRECATE row not listed is MISSING.
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

# The listing every case starts from: one kept struct with a field and a
# method, one kept enum with a variant, under the impl header that precedes it.
cat > "$TMP/base.api" <<EOF
pub mod trusty_memory::engine::store::kg_redb
#[non_exhaustive] pub enum $K::Step
pub $K::Step::Copy
pub struct $K::Store
pub $K::Store::path: std::path::PathBuf
impl $K::Store
pub fn $K::Store::open<P: core::convert::AsRef<std::path::Path>>(P) -> anyhow::Result<Self>
pub fn trusty_memory::socket_path() -> std::path::PathBuf
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
} > "$TMP/base.tsv"

# run_case <name> <want-exit> <needle-or-empty> <listing> [gate args...]
run_case() {
  local name="$1" want="$2" needle="$3" listing="$4" out status
  shift 4
  status=0
  out="$(ENGINE_API_AUDIT_TSV="$TMP/case.tsv" bash "$GATE" --api-file "$listing" "$@" 2>&1)" \
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

cp "$TMP/base.tsv" "$TMP/case.tsv"
run_case matching 0 '6 audited path(s) match' "$TMP/base.api"

cp "$TMP/base.api" "$TMP/narrow.api"
echo "pub fn $K::Store::compact(&self) -> anyhow::Result<()>" >> "$TMP/narrow.api"
run_case narrow-listed 1 "EXTRA $K::Store::compact" "$TMP/narrow.api"

cp "$TMP/base.api" "$TMP/hide.api"
echo "pub const $K::LAYOUT: &str" >> "$TMP/hide.api"
run_case hide-listed 1 "EXTRA $K::LAYOUT" "$TMP/hide.api"

cp "$TMP/base.api" "$TMP/unaudited.api"
echo "pub fn $K::Store::stats(&self) -> u64" >> "$TMP/unaudited.api"
run_case unaudited 1 "UNAUDITED $K::Store::stats" "$TMP/unaudited.api"

grep -v '::Store::open' "$TMP/base.api" > "$TMP/missing.api"
run_case keep-missing 1 "MISSING $K::Store::open" "$TMP/missing.api"

cp "$TMP/base.api" "$TMP/scope.api"
echo 'pub fn trusty_memory::engine::storex::other() -> u8' >> "$TMP/scope.api"
echo 'pub struct trusty_memory::engine::store_old::Thing' >> "$TMP/scope.api"
run_case out-of-scope 0 '6 audited path(s) match' "$TMP/scope.api"

# A store-root item (a new submodule or re-export) with no row must fail.
cp "$TMP/base.api" "$TMP/root.api"
echo 'pub struct trusty_memory::engine::store::NewRootReexport' >> "$TMP/root.api"
echo 'pub mod trusty_memory::engine::store::brand_new_mod' >> "$TMP/root.api"
run_case store-root-unaudited 1 'UNAUDITED trusty_memory::engine::store::NewRootReexport' "$TMP/root.api"
run_case store-root-new-mod 1 'UNAUDITED trusty_memory::engine::store::brand_new_mod' "$TMP/root.api"

# A4: an engine-root re-export name is a stem too; a name that only shares its
# start (`PalaceHandleExt`) is not under it.
cp "$TMP/base.api" "$TMP/engine-root.api"
echo 'pub fn trusty_memory::engine::PalaceHandle::brand_new(&self)' >> "$TMP/engine-root.api"
run_case engine-root-unaudited 1 'UNAUDITED trusty_memory::engine::PalaceHandle::brand_new' "$TMP/engine-root.api"
cp "$TMP/base.api" "$TMP/engine-root-stem.api"
echo 'pub struct trusty_memory::engine::PalaceHandleExt' >> "$TMP/engine-root-stem.api"
run_case engine-root-shared-stem 0 '6 audited path(s) match' "$TMP/engine-root-stem.api"

# A method on a generic in-scope type is a path of its own: `X<T>::m` must not
# collapse into the type's row (#9278), nested and lifetime groups included.
cp "$TMP/base.api" "$TMP/generic.api"
echo "pub fn $K::Store<T>::generic_new(&self) -> u8" >> "$TMP/generic.api"
echo "pub fn $K::Store<'a, Vec<T>>::lifetimed(&self) -> u8" >> "$TMP/generic.api"
run_case generic-type-unaudited 1 "UNAUDITED $K::Store::generic_new" "$TMP/generic.api"
run_case generic-type-nested-unaudited 1 "UNAUDITED $K::Store::lifetimed" "$TMP/generic.api"

: > "$TMP/empty.api"
run_case empty-listing 2 'empty listing' "$TMP/empty.api"

echo 'pub fn trusty_memory::socket_path() -> std::path::PathBuf' > "$TMP/none.api"
run_case no-scoped-path 2 'no listed path under any scope prefix' "$TMP/none.api"

run_case bad-argument 2 'unknown argument' "$TMP/base.api" --bogus

# Malformed table rows fail closed (exit 2, naming the line). Each case drops
# the Store::open row from the base table and appends one bad row as line 9.
OPEN_ROW="$K::Store::open"
bad_row_case() {
  local name="$1" row="$2" want="$3" needle="$4"
  grep -v '::Store::open' "$TMP/base.tsv" > "$TMP/case.tsv"
  printf '%s\n' "$row" >> "$TMP/case.tsv"
  run_case "$name" "$want" "$needle" "$TMP/base.api"
}
bad_row_case unknown-disposition "$(printf '%s\tmethod\tnarrow\tnone\tA3a' "$OPEN_ROW")" 2 'line 9'
bad_row_case mixed-case-disposition "$(printf '%s\tmethod\tKeep\ttests/a.rs\tA3a' "$OPEN_ROW")" 2 'line 9'
bad_row_case space-separated-row "$OPEN_ROW method KEEP tests/a.rs A3a" 2 'line 9'
bad_row_case four-field-row "$(printf '%s\tmethod\tKEEP\ttests/a.rs' "$OPEN_ROW")" 2 'line 9'

# A DEPRECATE row is public: absent from the listing it is MISSING.
cp "$TMP/base.tsv" "$TMP/case.tsv"
printf '%s::Store::old\tmethod\tDEPRECATE\ttests/a.rs\tA3a\n' "$K" >> "$TMP/case.tsv"
run_case deprecate-missing 1 "MISSING $K::Store::old" "$TMP/base.api"

echo "check_engine_api_audit_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]

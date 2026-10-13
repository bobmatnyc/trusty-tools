#!/usr/bin/env bash
#
# check_non_exhaustive_errors_selftest.sh — mutation self-test for
# scripts/check_non_exhaustive_errors.sh (issue #9273).
#
# Why: a gate whose matcher silently stopped matching reports OK exactly like a
#   clean tree (#4618, #5440). Its ability to fail is tested here before CI
#   trusts its pass.
# What: one throwaway git repo per case, with the gate copied into its
#   `scripts/` so the gate's repo root resolves inside the fixture, and one
#   Rust file under `crates/trusty-memory/src/`. Each case asserts the exit
#   status and, where it matters, the message. Failing cases pin each way an
#   enum counts as an error enum (a bare, path-qualified and multi-line
#   `derive(Error)`, an `...Error` name, a hand-written `impl Error for`) and
#   an attribute that must not leak from the item above. Passing cases pin the
#   marked forms, a non-error enum, a `pub(crate)` enum, test files and a file
#   outside the scan root. The scan floor and a bad argument close the set.
#
# Usage: bash scripts/check_non_exhaustive_errors_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_non_exhaustive_errors.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
SRC='crates/trusty-memory/src'

# new_fixture <name>: a git repo holding the gate and a clean lib.rs.
new_fixture() {
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir/scripts" "$dir/$SRC"
  cp "$GATE" "$dir/scripts/check_non_exhaustive_errors.sh"
  printf 'pub mod a;\n' > "$dir/$SRC/lib.rs"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# check <name> <want-exit> <needle-or-empty> <output> <status>
check() {
  local name="$1" want="$2" needle="$3" out="$4" status="$5" missing
  if [ "$status" -ne "$want" ]; then
    echo "FAIL $name: exit $status, want $want"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  # A `case` match, not `printf | grep -q`: grep -q exits early and pipefail
  # then reads the printf SIGPIPE as a failed match.
  missing=0
  if [ -n "$needle" ]; then case "$out" in *"$needle"*) ;; *) missing=1 ;; esac; fi
  if [ "$missing" -eq 1 ]; then
    echo "FAIL $name: output lacks '$needle'"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  echo "ok   $name (exit $status)"
  PASSED=$((PASSED + 1))
}

# run_case <name> <want-exit> <needle> <dir> [min-files] [arg]
run_case() {
  local name="$1" want="$2" needle="$3" dir="$4" min="${5:-1}" arg="${6:-}" out status
  git -C "$dir" add -A
  set +e
  if [ -n "$arg" ]; then
    out="$(NONEXH_MIN_FILES="$min" bash "$dir/scripts/check_non_exhaustive_errors.sh" "$arg" 2>&1)"
  else
    out="$(NONEXH_MIN_FILES="$min" bash "$dir/scripts/check_non_exhaustive_errors.sh" 2>&1)"
  fi
  status=$?
  set -e
  check "$name" "$want" "$needle" "$out" "$status"
}

# one_file <case> <want-exit> <needle> <rust-source>: the source as a.rs.
one_file() {
  local d
  d="$(new_fixture "$1")"
  printf '%b' "$4" > "$d/$SRC/a.rs"
  run_case "$1" "$2" "$3" "$d"
}

# --- an unmarked error enum fails ---------------------------------------------
one_file derive_bare 1 'a.rs:2: pub enum Gone' \
  '#[derive(Debug, Error)]\npub enum Gone {\n    A,\n}\n'
one_file derive_path 1 'a.rs:3: pub enum Gone' \
  '/// Doc.\n#[derive(Debug, Clone, thiserror::Error)]\npub enum Gone {\n    A,\n}\n'
one_file derive_multiline 1 'a.rs:6: pub enum Gone' \
  '#[derive(\n    Debug,\n    thiserror::Error,\n)]\n#[allow(dead_code)]\npub enum Gone<T> {\n    A(T),\n}\n'
one_file name_only 1 'a.rs:2: pub enum ParseError' \
  '#[derive(Debug)]\npub enum ParseError {\n    A,\n}\n'
one_file attr_no_leak 1 'a.rs:4: pub enum Gone' \
  '#[non_exhaustive]\npub struct S;\n#[derive(Debug, Error)]\npub enum Gone {\n    A,\n}\n'

d="$(new_fixture manual_impl)"
printf '#[derive(Debug)]\npub enum Refused {\n    A,\n}\n' > "$d/$SRC/a.rs"
printf 'impl std::error::Error for crate::a::Refused {}\n' > "$d/$SRC/b.rs"
run_case manual_impl 1 'a.rs:2: pub enum Refused' "$d"

# --- accepted forms pass ------------------------------------------------------
one_file marked_with_comments 0 'OK:' \
  '/// Doc.\n#[derive(Debug, Error)]\n// #9273: why.\n\n#[non_exhaustive]\npub enum Gone {\n    A,\n}\n'
one_file marked_first 0 'OK:' \
  '#[non_exhaustive]\n#[derive(\n    Debug,\n    Error,\n)]\npub enum StoreError {\n    A,\n}\n'
one_file not_an_error 0 'OK:' \
  '#[derive(Debug, Clone, Copy)]\npub enum Mode {\n    Read,\n}\n'
one_file crate_visible 0 'OK:' \
  '#[derive(Debug, Error)]\npub(crate) enum LocalError {\n    A,\n}\n'
one_file error_in_other_derive_name 0 'OK:' \
  '#[derive(Debug, ErrorCode)]\npub enum Kind {\n    A,\n}\n'

d="$(new_fixture test_files)"
mkdir -p "$d/$SRC/tests"
UNMARKED='#[derive(Debug, Error)]\npub enum TestOnlyError {\n    A,\n}\n'
printf '%b' "$UNMARKED" > "$d/$SRC/a_tests.rs"
printf '%b' "$UNMARKED" > "$d/$SRC/tests.rs"
printf '%b' "$UNMARKED" > "$d/$SRC/tests/fixture.rs"
run_case test_files 0 'OK:' "$d"

d="$(new_fixture outside_root)"
mkdir -p "$d/crates/trusty-mpm/src"
printf '%b' "$UNMARKED" > "$d/crates/trusty-mpm/src/a.rs"
run_case outside_root 0 'OK:' "$d"

# --- fail closed --------------------------------------------------------------
d="$(new_fixture scan_floor)"
run_case scan_floor 1 'SCAN FLOOR' "$d" 5
d="$(new_fixture bad_arg)"
run_case bad_arg 2 'unknown argument' "$d" 1 --nope

echo "check_non_exhaustive_errors_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]

#!/usr/bin/env bash
#
# check_doc_paths_selftest.sh — fixture cases for the backtick path-citation
# gate, scripts/check_doc_paths.sh (issue #5147).
#
# Why: the gate's whole value is what it REJECTS, and a path checker that has
#   only ever been observed passing is indistinguishable from one that returns 0
#   unconditionally. Its exclusion rules carry the same risk in the other
#   direction: every rule that suppresses a false positive is also a rule that
#   can suppress a real finding, so each one is pinned to a fixture line rather
#   than left to a reader's confidence. The issue names the false-positive class
#   explicitly (a doc that CONSTRUCTS a name instead of quoting one), which is
#   what excluded/ exists to hold.
#
# What: points the gate at each fixture tree under scripts/test-data/doc-paths/
#   via --root and asserts the exit status plus the exact substrings the run must
#   produce. Asserting the substrings, not just non-zero, is what stops a fixture
#   from passing for the wrong reason — crate-relative/ must report the ONE
#   missing `src/…` path and stay silent about the one that resolves, and a
#   fixture that reported both would look identical on exit status alone.
#
# Test: this IS the test. Run directly:
#   bash scripts/check_doc_paths_selftest.sh
#
# Portability: same constraints as check_doc_paths.sh — POSIX tools only,
#   bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_doc_paths.sh"
FIXTURE_DIR="$SCRIPT_DIR/test-data/doc-paths"

TAB="$(printf '\t')"

# tree<TAB>expected_exit<TAB>required substrings (comma-separated; "-" for none)
CASES="broken${TAB}1${TAB}FAIL BROKEN,broken.md:3: crates/trusty-installer/src/commands/gone.rs
clean${TAB}0${TAB}-
excluded${TAB}0${TAB}-
crate-relative${TAB}1${TAB}src/core/migration/m001.rs
line-overrun${TAB}0${TAB}WARN LINE,target.md:900
out-of-scope${TAB}1${TAB}SCAN FLOOR"

# Substrings that must NOT appear. A gate can satisfy every positive assertion
# above and still be wrong by ALSO reporting something it was told to ignore:
# the fenced sample in clean/, the resolving half of crate-relative/, or any of
# excluded/'s ten classes.
NEGATIVE="clean${TAB}nowhere.rs
crate-relative${TAB}src/lib.rs
excluded${TAB}FAIL BROKEN
line-overrun${TAB}FAIL BROKEN"

fail=0

run_gate() { # $1 = fixture tree, $2 = output file; prints exit status
  local rc=0
  bash "$GATE" --root "$FIXTURE_DIR/$1" >"$2" 2>&1 || rc=$?
  printf '%s' "$rc"
}

while IFS="$TAB" read -r tree expected_exit expected_subs; do
  [ -n "$tree" ] || continue
  if [ ! -d "$FIXTURE_DIR/$tree" ]; then
    echo "FAIL: fixture tree missing: $FIXTURE_DIR/$tree" >&2
    fail=1
    continue
  fi

  out="$(mktemp "${TMPDIR:-/tmp}/doc-paths-selftest.XXXXXX")"
  rc="$(run_gate "$tree" "$out")"

  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: $tree -> exit $rc (expected $expected_exit)" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
    rm -f "$out"
    continue
  fi

  missing=""
  if [ "$expected_subs" != "-" ]; then
    # Comma-split without arrays, so this stays bash 3.2 clean.
    rest="$expected_subs"
    while [ -n "$rest" ]; do
      case "$rest" in
        *,*)
          sub="${rest%%,*}"
          rest="${rest#*,}"
          ;;
        *)
          sub="$rest"
          rest=""
          ;;
      esac
      grep -qF -- "$sub" "$out" || missing="${missing}'${sub}' "
    done
  fi

  if [ -n "$missing" ]; then
    echo "FAIL: $tree -> exit $rc but output never mentions ${missing}" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
    rm -f "$out"
    continue
  fi

  if [ "$expected_subs" = "-" ]; then
    echo "PASS: $tree -> exit $rc (clean)"
  else
    echo "PASS: $tree -> exit $rc, reported ${expected_subs}"
  fi
  rm -f "$out"
done <<EOF
$CASES
EOF

while IFS="$TAB" read -r tree forbidden; do
  [ -n "$tree" ] || continue
  out="$(mktemp "${TMPDIR:-/tmp}/doc-paths-selftest.XXXXXX")"
  run_gate "$tree" "$out" >/dev/null
  if grep -qF -- "$forbidden" "$out"; then
    echo "FAIL: $tree -> output mentions '$forbidden', which it must not" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
  else
    echo "PASS: $tree -> silent about '$forbidden'"
  fi
  rm -f "$out"
done <<EOF
$NEGATIVE
EOF

# The committed tree must pass. A green fixture suite over a red checkout is the
# failure this catches — and it is the only case that exercises the tracked-file
# enumeration, which fixture mode replaces with `find`.
out="$(mktemp "${TMPDIR:-/tmp}/doc-paths-selftest.XXXXXX")"
if bash "$GATE" >"$out" 2>&1; then
  echo "PASS: committed tree -> exit 0 (clean)"
else
  echo "FAIL: the committed tree does not pass the gate" >&2
  sed 's/^/       /' "$out" >&2
  fail=1
fi
rm -f "$out"

if [ "$fail" -ne 0 ]; then
  echo "check_doc_paths_selftest: one or more cases FAILED." >&2
  exit 1
fi

echo "check_doc_paths_selftest: all cases passed."
exit 0

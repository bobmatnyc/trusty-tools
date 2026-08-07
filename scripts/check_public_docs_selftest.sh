#!/usr/bin/env bash
#
# check_public_docs_selftest.sh — failing-case fixtures for the public
# documentation allowlist gate, scripts/check_public_docs.sh (issue #5096).
#
# Why: the gate's whole value is what it REJECTS, and a boundary check that has
#   only ever been observed passing is indistinguishable from one that returns 0
#   unconditionally. Both cases the issue names explicitly are pinned here:
#     - forbidden-specs.tsv points at docs/specs/README.md, a file that REALLY
#       EXISTS. It must fail anyway — proving FORBIDDEN is a boundary check, not
#       a dressed-up existence check.
#     - missing-source.tsv points at a docs/ path that does not exist, which is
#       what a rename or deletion in docs/ looks like to the manifest.
#
# What: runs the gate against each fixture in scripts/test-data/public-docs/ and
#   asserts both the exit status and, for failures, that the expected finding
#   code appears on stderr. Asserting the CODE and not just non-zero is what
#   stops a fixture from passing for the wrong reason (e.g. a traversal fixture
#   that fails as MISSING because the escaped path happens not to exist).
#
# Test: this IS the test. Run directly:
#   bash scripts/check_public_docs_selftest.sh
#
# Portability: same constraints as check_public_docs.sh — POSIX tools only,
#   bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GATE="$SCRIPT_DIR/check_public_docs.sh"
FIXTURE_DIR="$SCRIPT_DIR/test-data/public-docs"

# fixture<TAB>expected_exit<TAB>expected_code_on_stderr ("-" when exit 0)
TAB="$(printf '\t')"
CASES="clean.tsv${TAB}0${TAB}-
missing-source.tsv${TAB}1${TAB}MISSING
forbidden-specs.tsv${TAB}1${TAB}FORBIDDEN
forbidden-nested.tsv${TAB}1${TAB}FORBIDDEN
escapes-docs.tsv${TAB}1${TAB}ESCAPES-DOCS
dup-route.tsv${TAB}1${TAB}DUP-ROUTE
bad-record.tsv${TAB}1${TAB}BAD-RECORD
empty.tsv${TAB}1${TAB}SCAN FLOOR"

fail=0
while IFS="$TAB" read -r fixture expected_exit expected_code; do
  [ -n "$fixture" ] || continue
  path="$FIXTURE_DIR/$fixture"
  if [ ! -f "$path" ]; then
    echo "FAIL: fixture missing: $path" >&2
    fail=1
    continue
  fi

  err="$(mktemp "${TMPDIR:-/tmp}/public-docs-selftest.XXXXXX")"
  rc=0
  bash "$GATE" --manifest "$path" --root "$REPO_ROOT" >/dev/null 2>"$err" || rc=$?

  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: $fixture -> exit $rc (expected $expected_exit)" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
    rm -f "$err"
    continue
  fi

  if [ "$expected_code" != "-" ] && ! grep -qF -- "$expected_code" "$err"; then
    echo "FAIL: $fixture -> exit $rc but stderr never mentions '$expected_code'" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
    rm -f "$err"
    continue
  fi

  if [ "$expected_code" = "-" ]; then
    echo "PASS: $fixture -> exit $rc (clean)"
  else
    echo "PASS: $fixture -> exit $rc, reported $expected_code"
  fi
  rm -f "$err"
done <<EOF
$CASES
EOF

# The committed manifest itself must pass. A green fixture suite over a red
# manifest is the failure this catches.
if bash "$GATE" >/dev/null 2>&1; then
  echo "PASS: docs/public-manifest.tsv -> exit 0 (clean)"
else
  echo "FAIL: the committed docs/public-manifest.tsv does not pass the gate" >&2
  bash "$GATE" >&2 || true
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "check_public_docs_selftest: one or more cases FAILED." >&2
  exit 1
fi

echo "check_public_docs_selftest: all cases passed."
exit 0

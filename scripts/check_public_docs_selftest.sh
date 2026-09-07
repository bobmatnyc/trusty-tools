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
#   The STALE cases (issue #5125) run against
#   scripts/test-data/public-docs/fakeroot/ via --root, so they assert on pages
#   this file controls rather than on whatever the real docs/ tree says this
#   week. Since #5134 turned the pass on by default they come in two kinds, and
#   neither substitutes for the other:
#     - the `--stale-terms` cases prove the LOGIC — a hit fires, a waiver holds,
#       an unexplained waiver is refused;
#     - the DEFAULT-INVOCATION cases prove the WIRING. They pass no --stale flag
#       of any kind, which is exactly how the pre-commit hook and
#       public-docs.yml call the gate, so a change that made the pass opt-in
#       again fails here. A flag-bearing case cannot notice that.
#   The `--no-stale` cases pin the one escape hatch: it is refused without
#   --manifest, so the committed manifest can never be checked without the
#   content pass.
#
#   The `unreadable page` case pins the grep-status read the STALE scan gained
#   in #5134. The `|| true` it replaced swallowed grep exit 2 identically to
#   exit 1, so a page the gate could not open was reported clean while it
#   carried every retired term.
#
#   forbidden-internal-suffix.tsv covers the third case, which has no tree rule
#   behind it: docs/reference/ is mixed-audience, so the internal half of a split
#   (docs/reference/config-convention-internal.md, PR #5107) sits one row away
#   from its published half. It must fail as FORBIDDEN, not as MISSING — the
#   suffix rule has to run BEFORE the existence check, or the boundary would
#   quietly start depending on which branch happens to be checked out.
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
forbidden-internal-suffix.tsv${TAB}1${TAB}FORBIDDEN
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
  # --no-stale: these fixtures assert on manifest SHAPE, and the real
  # docs/public-stale-terms.tsv waives a page none of them publishes, so the
  # STALE pass would report a dead waiver on every one of them. Allowed here
  # only because --manifest is present; the default-invocation cases below cover
  # what this one switches off.
  bash "$GATE" --manifest "$path" --root "$REPO_ROOT" --no-stale >/dev/null 2>"$err" || rc=$?

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

# --- STALE pass (issue #5125) ------------------------------------------------
#
# manifest<TAB>terms<TAB>expected_exit<TAB>expected_substrings (comma-separated,
# ALL required; "-" when exit 0). Requiring several substrings is what makes
# case 1 prove BOTH that a retired literal fires AND that the derived
# @FORMER_REPO_CLONE_URLS@ term fires — a single "STALE" assertion would pass on
# either alone, and the derivation is the half that can silently match nothing.
FAKEROOT="$FIXTURE_DIR/fakeroot"
STALE_CASES="stale-hit.tsv${TAB}stale-terms.tsv${TAB}1${TAB}FAIL STALE,trusty-mpmd,@FORMER_REPO_CLONE_URLS@
stale-waived.tsv${TAB}stale-terms-waived.tsv${TAB}0${TAB}-
stale-waived.tsv${TAB}stale-terms-empty-reason.tsv${TAB}1${TAB}BAD-WAIVER
stale-unpublished.tsv${TAB}stale-terms.tsv${TAB}0${TAB}-"

while IFS="$TAB" read -r fixture terms expected_exit expected_subs; do
  [ -n "$fixture" ] || continue
  label="$fixture + $terms"
  mpath="$FIXTURE_DIR/$fixture"
  tpath="$FIXTURE_DIR/$terms"
  if [ ! -f "$mpath" ] || [ ! -f "$tpath" ]; then
    echo "FAIL: stale fixture missing: $mpath / $tpath" >&2
    fail=1
    continue
  fi

  err="$(mktemp "${TMPDIR:-/tmp}/public-docs-selftest.XXXXXX")"
  rc=0
  bash "$GATE" --manifest "$mpath" --root "$FAKEROOT" \
    --stale-terms "$tpath" >/dev/null 2>"$err" || rc=$?

  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: $label -> exit $rc (expected $expected_exit)" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
    rm -f "$err"
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
      grep -qF -- "$sub" "$err" || missing="${missing}'${sub}' "
    done
  fi

  if [ -n "$missing" ]; then
    echo "FAIL: $label -> exit $rc but stderr never mentions ${missing}" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
    rm -f "$err"
    continue
  fi

  if [ "$expected_subs" = "-" ]; then
    echo "PASS: $label -> exit $rc (clean)"
  else
    echo "PASS: $label -> exit $rc, reported ${expected_subs}"
  fi
  rm -f "$err"
done <<EOF
$STALE_CASES
EOF

# --- default invocation: the STALE pass runs with NO flag (issue #5134) -------
#
# Every case above names a --stale flag, so all of them would still pass if the
# pass went back to being opt-in. These two do not, and that is the whole point:
# they invoke the gate exactly as the pre-commit hook and public-docs.yml do.
#
# The FAKEROOT carries its own docs/public-stale-terms.tsv at the default path,
# so a no-flag run there searches fixture pages for fixture terms.
run_default_case() {
  # $1 fixture manifest, $2 expected exit, $3 required substring in stdout+stderr
  local fixture="$1" expected_exit="$2" needle="$3" out rc=0
  out="$(mktemp "${TMPDIR:-/tmp}/public-docs-selftest.XXXXXX")"
  bash "$GATE" --manifest "$FIXTURE_DIR/$fixture" --root "$FAKEROOT" >"$out" 2>&1 || rc=$?
  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: default invocation on $fixture -> exit $rc (expected $expected_exit)" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
  elif ! grep -qF -- "$needle" "$out"; then
    echo "FAIL: default invocation on $fixture -> exit $rc but output never mentions '$needle'" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
  else
    echo "PASS: default invocation on $fixture -> exit $rc, reported '$needle' with no --stale flag"
  fi
  rm -f "$out"
}

# A page carrying a retired term makes a no-flag run fail. This is the case that
# proves the gate BITES where it is wired.
run_default_case "stale-hit.tsv" 1 "FAIL STALE"

# ... and the mirror: a clean page passes, with the success line naming the term
# count. Without that assertion an exit-0 case cannot tell "searched and found
# nothing" from "never searched", which is the failure #5134 removed.
run_default_case "stale-unpublished.tsv" 0 "retired term(s)"

# --- --no-stale is refused wherever it could weaken a real run ---------------
run_refusal_case() {
  # $1 label, then the gate's arguments
  local label="$1" out rc=0
  shift
  out="$(mktemp "${TMPDIR:-/tmp}/public-docs-selftest.XXXXXX")"
  bash "$GATE" "$@" >"$out" 2>&1 || rc=$?
  if [ "$rc" -ne 2 ]; then
    echo "FAIL: $label -> exit $rc (expected 2, a usage refusal)" >&2
    sed 's/^/       /' "$out" >&2
    fail=1
  else
    echo "PASS: $label -> exit 2 (refused)"
  fi
  rm -f "$out"
}

# Without --manifest, --no-stale would disable the content check on the
# COMMITTED manifest — the one run that must never be weakened.
run_refusal_case "--no-stale on the default manifest" --no-stale
run_refusal_case "--no-stale alongside --stale" --manifest "$FIXTURE_DIR/clean.tsv" --no-stale --stale

# --- an unsearchable page is a finding, not a pass (issue #5134) -------------
#
# `hits="$(grep -nE … || true)"` reported grep's exit 2 (unreadable file) as
# cleanly as its exit 1 (no match), so making the only stale-bearing page
# unreadable produced exit 0 and a line claiming the page carried none of the
# terms. Root can read a 000 file, which would make this case pass for the wrong
# reason, so it is skipped there rather than asserted.
if [ "$(id -u)" = "0" ]; then
  echo "SKIP: unreadable page -> running as root, which can read a mode-000 file"
else
  UNREADABLE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/public-docs-unreadable.XXXXXX")"
  cp -R "$FAKEROOT/docs" "$UNREADABLE_ROOT/docs"
  chmod 000 "$UNREADABLE_ROOT/docs/pages/retired-binary.md"
  err="$(mktemp "${TMPDIR:-/tmp}/public-docs-selftest.XXXXXX")"
  rc=0
  bash "$GATE" --manifest "$FIXTURE_DIR/stale-hit.tsv" --root "$UNREADABLE_ROOT" \
    >/dev/null 2>"$err" || rc=$?
  if [ "$rc" -ne 1 ]; then
    echo "FAIL: unreadable page -> exit $rc (expected 1)" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
  elif ! grep -qF -- "UNREADABLE" "$err"; then
    echo "FAIL: unreadable page -> exit $rc but stderr never mentions 'UNREADABLE'" >&2
    sed 's/^/       /' "$err" >&2
    fail=1
  else
    echo "PASS: unreadable page -> exit $rc, reported UNREADABLE"
  fi
  rm -f "$err"
  chmod 644 "$UNREADABLE_ROOT/docs/pages/retired-binary.md"
  rm -rf "$UNREADABLE_ROOT"
fi

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

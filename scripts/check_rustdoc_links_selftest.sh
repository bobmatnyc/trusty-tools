#!/usr/bin/env bash
#
# check_rustdoc_links_selftest.sh — fail-closed fixtures for the broken
# intra-doc link gate, scripts/check_rustdoc_links.sh.
#
# Why: the gate printed
#     SUMMARY  25 crate(s) documented, 0 broken link(s), baseline 0, 0 examined
#   and exited 0 on a tree carrying 26 broken links. Nothing in the gate was
#   observed rejecting a vacuous run, and a scan that has only ever been seen
#   passing is indistinguishable from one that returns 0 unconditionally. That
#   is the #5620 shape for the third time in this repo, so the case that pins
#   it — `cached-no-op` — is the reason this file exists.
#
#   The three vacuous fixtures differ only in what the old guard would have
#   made of them, which is the point:
#     - cached-no-op       cargo exit 0, zero diagnostics, every doc artifact
#                          `fresh: true`. The old guard needed cargo_rc != 0,
#                          so it passed this. THE REGRESSION CASE.
#     - partial-cache      one crate re-documented, one served from cache. The
#                          cached crate's zero is not evidence and must be named.
#     - rlib-only          artifacts exist for workspace crates but none is a
#                          rustdoc output. Counting these is what produced
#                          "25 crate(s) documented" from 3 real rustdoc runs.
#
#   `clean-fresh` is the necessary counterweight: a genuinely clean tree has
#   ZERO diagnostics too, so a gate that demanded diagnostics as proof-of-life
#   could never go green. Positive evidence has to come from the artifact, and
#   this case proves the distinction is actually drawn.
#
# What: feeds synthetic cargo JSON streams to the gate's `--json` entry point
#   (with `--cargo-rc` to set the exit status being scored) against an empty
#   baseline, asserting both the exit status and the finding code on stdout.
#   Asserting the CODE stops a fixture from passing for the wrong reason — a
#   vacuous fixture that fails as UNBASELINED rather than VACUOUS-SCAN is not
#   testing what it claims to.
#
#   No cargo doc build is involved, so this runs in well under a second.
#
# Test: this IS the test. Run directly:
#   bash scripts/check_rustdoc_links_selftest.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_rustdoc_links.sh"
FIXTURE_DIR="$SCRIPT_DIR/test-data/rustdoc-links"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rustdoc-links-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# An empty baseline: every crate must have zero findings.
EMPTY_BASELINE="$WORK/empty-baseline.tsv"
printf '# empty baseline for the self-test\n' > "$EMPTY_BASELINE"

TAB="$(printf '\t')"

# fixture<TAB>cargo_rc<TAB>expected_exit<TAB>expected_code ("-" when exit 0)
CASES="cached-no-op.json${TAB}0${TAB}3${TAB}VACUOUS-SCAN
cached-no-op.json${TAB}101${TAB}3${TAB}VACUOUS-SCAN
rlib-only.json${TAB}0${TAB}3${TAB}VACUOUS-SCAN
partial-cache.json${TAB}0${TAB}3${TAB}NOT-EXAMINED
clean-fresh.json${TAB}0${TAB}0${TAB}-
broken-link.json${TAB}101${TAB}1${TAB}UNBASELINED
build-error.json${TAB}101${TAB}3${TAB}BUILD-ERROR
unattributable.json${TAB}101${TAB}3${TAB}UNATTRIBUTABLE"

fail=0
run=0

while IFS="$TAB" read -r fixture cargo_rc expected_exit expected_code; do
  [ -n "$fixture" ] || continue
  run=$((run + 1))
  fixture_path="$FIXTURE_DIR/$fixture"
  if [ ! -f "$fixture_path" ]; then
    echo "FAIL  $fixture (rc=$cargo_rc): fixture not found at $fixture_path"
    fail=1
    continue
  fi

  out="$WORK/out.txt"
  actual_exit=0
  BASELINE_OVERRIDE="$EMPTY_BASELINE" \
    bash "$GATE" --json "$fixture_path" --cargo-rc "$cargo_rc" \
    > "$out" 2>&1 || actual_exit=$?

  if [ "$actual_exit" != "$expected_exit" ]; then
    echo "FAIL  $fixture (cargo_rc=$cargo_rc): expected exit $expected_exit, got $actual_exit"
    sed 's/^/        /' "$out"
    fail=1
    continue
  fi

  if [ "$expected_code" != "-" ]; then
    if ! grep -q "$expected_code" "$out"; then
      echo "FAIL  $fixture (cargo_rc=$cargo_rc): exit $actual_exit correct, but '$expected_code' not reported"
      sed 's/^/        /' "$out"
      fail=1
      continue
    fi
  fi

  echo "ok    $fixture (cargo_rc=$cargo_rc) -> exit $actual_exit ${expected_code}"
done <<EOF
$CASES
EOF

echo
if [ "$fail" -ne 0 ]; then
  echo "check_rustdoc_links_selftest: FAILED ($run case(s) run)"
  exit 1
fi
echo "check_rustdoc_links_selftest: all $run case(s) passed"

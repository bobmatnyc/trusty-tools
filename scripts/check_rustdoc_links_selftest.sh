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
#   `clean-fresh` runs TWICE, and the pair is the #7577 case. The stream is the
#   same both times; only cargo's exit status differs. At 0 it is the clean pass
#   above. At 101 it is a DEFAULT-lane cargo that died with nothing in the
#   stream to explain it — a crate outside EXCLUDES that fails to build under
#   default features — and that must fail LANE-ERROR. It exited 0 until #7577,
#   because the gate's FAIL CLOSED 9 loop skipped any lane with no declared
#   members and `default` never has any: it is the one lane run unconditionally
#   rather than declared in the lane file.
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
#   #7466 added the feature-lane cases: the gate documented DEFAULT features
#   only, so a broken link inside a default-off feature was invisible until
#   release. Those cases drive the declared-lane phase over a fixture world —
#   a link found only in a feature lane must reach the verdict, an unbuildable
#   lane must FAIL rather than skip, an unplaced feature must fail, a NO-CFG
#   exemption is re-verified, and one link counts once across lanes.
#
# Test: this IS the test. Run directly:
#   bash scripts/check_rustdoc_links_selftest.sh
#   bash scripts/check_rustdoc_links_selftest.sh --gate /path/to/gate.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_rustdoc_links.sh"
FIXTURE_DIR="$SCRIPT_DIR/test-data/rustdoc-links"

# --gate runs the cases against an ALTERNATE copy of the gate. Pointing it at
# the pre-#7466 script is how the mutation is demonstrated: that run must FAIL
# here, which proves these cases are not passing by construction.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --gate)
      [ -n "${2:-}" ] || { echo "ERROR: --gate needs a path" >&2; exit 2; }
      GATE="$2"
      shift 2
      ;;
    *) echo "ERROR: unknown argument '$1'" >&2; exit 2 ;;
  esac
done
[ -f "$GATE" ] || { echo "ERROR: no gate script at '$GATE'" >&2; exit 2; }

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
unattributable.json${TAB}101${TAB}3${TAB}UNATTRIBUTABLE
clean-fresh.json${TAB}101${TAB}3${TAB}LANE-ERROR"

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

# ===========================================================================
# FEATURE LANES (#7466). The default-features-only run reported 0 broken links
# for every crate while a link inside a default-off feature failed the release
# gate. These cases drive the declaration and the per-lane examination check
# over a two-crate fixture world (metadata-mini.json + lanes-*.tsv), so they
# need no cargo at all.
#
# lane_case: name, lanes file, expected exit, expected code ("-" for none),
# then the gate's arguments.
# ===========================================================================
DEMO_BASELINE="$WORK/demo-1-baseline.tsv"
printf '# one known link in demo\ndemo\t1\n' > "$DEMO_BASELINE"

lane_case() {
  name="$1"; lanes="$2"; expected_exit="$3"; expected_code="$4"
  shift 4
  run=$((run + 1))
  out="$WORK/out-$name.txt"
  actual_exit=0
  BASELINE_OVERRIDE="${LANE_BASELINE:-$EMPTY_BASELINE}" \
    LANES_OVERRIDE="$FIXTURE_DIR/$lanes" \
    METADATA_OVERRIDE="$FIXTURE_DIR/metadata-mini.json" \
    bash "$GATE" "$@" > "$out" 2>&1 || actual_exit=$?

  if [ "$actual_exit" != "$expected_exit" ]; then
    echo "FAIL  $name: expected exit $expected_exit, got $actual_exit"
    sed 's/^/        /' "$out"
    fail=1
    return
  fi
  if [ "$expected_code" != "-" ] && ! grep -q "$expected_code" "$out"; then
    echo "FAIL  $name: exit $actual_exit correct, but '$expected_code' not reported"
    sed 's/^/        /' "$out"
    fail=1
    return
  fi
  echo "ok    $name -> exit $actual_exit ${expected_code}"
}

# THE REGRESSION CASE. A broken link that exists ONLY in a feature-gated file
# reaches the verdict, because the lane documented the crate that owns it.
# Pre-#7466 there was no second pass at all, so this link was never seen.
lane_case feature-lane-finds-link lanes-mini.tsv 1 UNBASELINED \
  --json "$FIXTURE_DIR/lane-documented.json" --lane default \
  --json "$FIXTURE_DIR/lane-feature-link.json" --lane features --cargo-rc 101

# AN UNBUILDABLE LANE IS A FAILURE, NOT A SKIP. cargo died before invoking
# rustdoc, so the lane's stream carries no artifact — the shape a mutually
# exclusive or mis-declared feature set produces. Scoring the default lane's
# clean result as the verdict is exactly the #5620 fail-open.
lane_case lane-unbuildable-fails lanes-mini.tsv 3 LANE-NOT-EXAMINED \
  --json "$FIXTURE_DIR/lane-documented.json" --lane default \
  --json "$FIXTURE_DIR/lane-empty.json" --lane features --cargo-rc 101

# A lane served from cache examined nothing either, even at cargo exit 0.
lane_case lane-cached-is-not-evidence lanes-mini.tsv 3 LANE-NOT-EXAMINED \
  --json "$FIXTURE_DIR/lane-documented.json" --lane default \
  --json "$FIXTURE_DIR/lane-cached.json" --lane features

# A feature no row places fails the gate. This is what makes a NEW default-off
# module impossible to add without deciding how it gets documented.
lane_case feature-uncovered-fails lanes-missing-heavy.tsv 3 FEATURE-UNCOVERED \
  --json "$FIXTURE_DIR/lane-documented.json" --lane default \
  --json "$FIXTURE_DIR/lane-documented.json" --lane features

# A NO-CFG exemption is re-verified, never trusted: demo/config does carry a
# cfg site, so the row claiming it carries none is rejected.
lane_case no-cfg-stale-fails lanes-nocfg-stale.tsv 3 NO-CFG-STALE \
  --json "$FIXTURE_DIR/lane-documented.json" --lane default \
  --json "$FIXTURE_DIR/lane-documented.json" --lane features

# ONE LINK, ONE COUNT. A link in unconditional code is re-reported by every
# lane. Counting it per lane would read as a REGRESSION the moment a lane is
# added, so the same (crate, file:line, message) is scored once: two lanes
# reporting it must still measure 1 against a baseline of 1.
LANE_BASELINE="$DEMO_BASELINE" \
  lane_case dedup-across-lanes lanes-mini.tsv 0 'broken link(s), baseline 1' \
  --json "$FIXTURE_DIR/lane-feature-link.json" --lane default --cargo-rc 101 \
  --json "$FIXTURE_DIR/lane-feature-link.json" --lane features --cargo-rc 101

echo
if [ "$fail" -ne 0 ]; then
  echo "check_rustdoc_links_selftest: FAILED ($run case(s) run)"
  exit 1
fi
echo "check_rustdoc_links_selftest: all $run case(s) passed"

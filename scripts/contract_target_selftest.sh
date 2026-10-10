#!/usr/bin/env bash
#
# contract_target_selftest.sh — cases for scripts/resolve_contract_target.sh and
#   for the two pre-publish.yml gate steps that consume it (#9539).
#
# Why: pre-publish.yml's Gates 5-6 hardcoded trusty-common. A run for any other
#   crate (trusty-mpm 1.8.0; trusty-review in run 37920566051) checked and
#   diffed trusty-common's contracts and labelled the verdict with the other
#   crate's name. The first fix then made Gate 5 SKIP whenever no crate was
#   named, which is every documented dispatch, so trusty-common's drift check
#   stopped running. This file pins both: Gate 5 checks every contracts.json
#   on every run, and Gate 6 diffs the resolved crate.
#
# What: builds a fixture tree it owns — crates/trusty-common with a
#   contracts.json, crates/fixture-no-contracts without one, a copy of the
#   resolver and a stub scripts/check_contracts.sh that logs its arguments.
#   Then:
#     1. drives the resolver with synthetic INPUT_CRATE / REF_TYPE / REF_NAME,
#        asserting the exit code, the key=value lines and the annotation;
#     2. extracts the Gate 5 and Gate 6 `run:` blocks from pre-publish.yml and
#        executes them in the fixture, asserting the exit code and which
#        crates reached check_contracts.sh;
#     3. asserts the wiring: both steps call the resolver, the `*) … exit 1`
#        arm of their resolver case is present, no other arm lists exit code 1,
#        and no step hardcodes a trusty-common crate, path or default.
#
# Usage: bash scripts/contract_target_selftest.sh
#   CONTRACT_TARGET_RESOLVER / CONTRACT_TARGET_WORKFLOW point the cases at a
#   mutant copy, to show they can fail.
# Exit: 0 when every case matches; 1 otherwise, naming each mismatch.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOLVER="${CONTRACT_TARGET_RESOLVER:-${REPO_ROOT}/scripts/resolve_contract_target.sh}"
PREPUB="${CONTRACT_TARGET_WORKFLOW:-${REPO_ROOT}/.github/workflows/pre-publish.yml}"

FAILURES=0
CASES=0
FIX="$(mktemp -d "${TMPDIR:-/tmp}/contract-target-selftest.XXXXXX")"
trap 'rm -rf "$FIX"' EXIT
ERRF="${FIX}/stderr"
STUB_LOG="${FIX}/check_contracts.log"

assert_eq() {   # <label> <expected> <actual>
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-60s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# Here-string, not a pipe into `grep -q`: see rustdoc-links-scope-selftest.sh.
has() { if grep -qF -- "$2" <<<"$1"; then echo yes; else echo no; fi; }

# --- fixture tree ----------------------------------------------------------
# fixture_crate <name> <version>: a crate dir whose [package] version differs
# from a later table's `version`, so the resolver must read the right table.
fixture_crate() {
  mkdir -p "${FIX}/crates/$1"
  printf '[package]\nname = "%s"\nversion = "%s"\n\n[dependencies]\nversion = "0.0.0"\n' \
    "$1" "$2" >"${FIX}/crates/$1/Cargo.toml"
}
mkdir -p "${FIX}/scripts" "${FIX}/tmp"
fixture_crate trusty-common 9.9.9
fixture_crate fixture-no-contracts 1.8.0
printf '{}\n' >"${FIX}/crates/trusty-common/contracts.json"
cp "$RESOLVER" "${FIX}/scripts/resolve_contract_target.sh" 2>/dev/null
: >"${FIX}/scripts/diff_contracts.py"
# The stub logs each call. --diff answers the "no baseline" NO VERDICT Gate 6
# expects; --crate exits STUB_RC (0 = no drift).
# shellcheck disable=SC2016  # the stub's own variables, written literally
printf '%s\n' '#!/usr/bin/env bash' \
  'echo "$*" >>"$STUB_LOG"' \
  'if [ "$1" = "--diff" ]; then' \
  '  echo "NO VERDICT: baseline artifact $2 could not be read"; exit 3' \
  'fi' \
  'exit "${STUB_RC:-0}"' >"${FIX}/scripts/check_contracts.sh"

# resolve <input_crate> <ref_type> <ref_name>
#   Sets OUT (stdout), ERR (stderr) and RC.
resolve() {
  OUT="$(env -u GITHUB_OUTPUT INPUT_CRATE="$1" REF_TYPE="$2" REF_NAME="$3" \
    bash "${FIX}/scripts/resolve_contract_target.sh" --gate selftest-gate 2>"$ERRF")"
  RC=$?
  ERR="$(cat "$ERRF")"
}

field() { sed -n "s/^$1=//p" <<<"$OUT"; }

assert_eq "resolver script exists" yes "$( [ -f "$RESOLVER" ] && echo yes || echo no)"

echo "contract-target: crate input"
resolve fixture-no-contracts '' ''
assert_eq "no-contracts input: exit is no-contracts (10)" 10 "$RC"
assert_eq "no-contracts input: crate" fixture-no-contracts "$(field crate)"
assert_eq "no-contracts input: its [package] version" 1.8.0 "$(field version)"
assert_eq "no-contracts input: its own contracts path" \
  crates/fixture-no-contracts/contracts.json "$(field contracts)"
assert_eq "no-contracts input: SKIP annotation" yes "$(has "$ERR" '::warning::[SKIP] selftest-gate:')"
assert_eq "no-contracts input: SKIP names its path" yes \
  "$(has "$ERR" 'crates/fixture-no-contracts/contracts.json')"
assert_eq "no-contracts input: no trusty-common anywhere" no \
  "$(has "${OUT}${ERR}" 'trusty-common')"

resolve trusty-common '' ''
assert_eq "trusty-common input: exit 0" 0 "$RC"
assert_eq "trusty-common input: crate" trusty-common "$(field crate)"
assert_eq "trusty-common input: its [package] version" 9.9.9 "$(field version)"
assert_eq "trusty-common input: its own contracts path" \
  crates/trusty-common/contracts.json "$(field contracts)"
assert_eq "trusty-common input: no SKIP" no "$(has "$ERR" '[SKIP]')"

echo "contract-target: tag push"
resolve '' tag fixture-no-contracts-v1.8.0
assert_eq "tag fixture-no-contracts-v1.8.0: exit 10" 10 "$RC"
assert_eq "tag fixture-no-contracts-v1.8.0: crate" fixture-no-contracts "$(field crate)"
assert_eq "tag fixture-no-contracts-v1.8.0: version" 1.8.0 "$(field version)"

resolve '' tag trusty-common-v9.9.9
assert_eq "tag trusty-common-v9.9.9: exit 0" 0 "$RC"
assert_eq "tag trusty-common-v9.9.9: crate" trusty-common "$(field crate)"

resolve '' tag fixture-no-contracts-v1.8.0-rc.1
assert_eq "prerelease tag: crate" fixture-no-contracts "$(field crate)"

resolve trusty-common tag fixture-no-contracts-v1.8.0
assert_eq "input wins over the tag" trusty-common "$(field crate)"

echo "contract-target: unresolved"
resolve '' tag not-a-release-tag
assert_eq "unparseable tag: exit is unresolved (11)" 11 "$RC"
assert_eq "unparseable tag: loud SKIP" yes "$(has "$ERR" '::warning::[SKIP] selftest-gate:')"
assert_eq "unparseable tag: says could not be resolved" yes "$(has "$ERR" 'could not be resolved')"
assert_eq "unparseable tag: names the tag" yes "$(has "$ERR" 'not-a-release-tag')"
assert_eq "unparseable tag: no crate line" '' "$(field crate)"

resolve '' tag fixture-no-contracts-v1.8
assert_eq "two-part version tag: unresolved" 11 "$RC"

resolve '' branch main
assert_eq "resolver, dispatch with no input: unresolved" 11 "$RC"
assert_eq "resolver, dispatch with no input: no trusty-common default" no \
  "$(has "$OUT" 'trusty-common')"

echo "contract-target: unknown crate"
resolve no-such-crate '' ''
assert_eq "unknown crate input: exit 1" 1 "$RC"
assert_eq "unknown crate input: error annotation" yes "$(has "$ERR" '::error::')"
assert_eq "unknown crate input: names it" yes "$(has "$ERR" 'no-such-crate')"
assert_eq "unknown crate input: no SKIP" no "$(has "$ERR" '[SKIP]')"

resolve '' tag no-such-crate-v1.0.0
assert_eq "unknown crate from tag: exit 1" 1 "$RC"

resolve ../crates/trusty-common '' ''
assert_eq "path-shaped input: exit 1" 1 "$RC"

# --- the gate steps themselves ---------------------------------------------
# step <name-prefix>: one step's text, from its `- name:` to the next step or
# job, with comment lines dropped so prose about trusty-common does not count.
step() {
  awk -v want="      - name: \"$1" '
    index($0, want) == 1 { on = 1; print; next }
    on && (/^      - / || /^  [A-Za-z0-9_-]+:/) { exit }
    on' "$PREPUB" | grep -vE '^[[:space:]]*#'
}

# run_block <name-prefix>: the step's `run: |` body, de-indented.
run_block() {
  awk -v want="      - name: \"$1" '
    index($0, want) == 1 { on = 1; next }
    on && (/^      - / || /^  [A-Za-z0-9_-]+:/) { exit }
    on && /^        run: [|]/ { inrun = 1; next }
    inrun && /^          / { print substr($0, 11); next }
    inrun && /^[[:space:]]*$/ { print ""; next }
    inrun { inrun = 0 }' "$PREPUB"
}

# run_gate <name-prefix> <input_crate> <ref_type> <ref_name> [stub_rc]
#   Runs the step body in the fixture. Sets GOUT (stdout+stderr), GRC and
#   GLOG (the check_contracts.sh calls, one per line).
run_gate() {
  run_block "$1" >"${FIX}/gate.sh"
  : >"$STUB_LOG"
  : >"${FIX}/github_output"
  GOUT="$(cd "$FIX" && env INPUT_CRATE="$2" REF_TYPE="$3" REF_NAME="$4" \
    STUB_RC="${5:-0}" STUB_LOG="$STUB_LOG" RUNNER_TEMP="${FIX}/tmp" \
    GITHUB_OUTPUT="${FIX}/github_output" bash gate.sh 2>&1)"
  GRC=$?
  GLOG="$(cat "$STUB_LOG")"
}

G5="Gate 5"
G6="Gate 6"

echo "contract-target: Gate 5 checks every contracts.json"
# #9539: every documented dispatch passes no crate input. Gate 5 must still
# check trusty-common's drift, never SKIP.
run_gate "$G5" '' branch main
assert_eq "Gate 5, no crate input: exit 0" 0 "$GRC"
assert_eq "Gate 5, no crate input: checks trusty-common" \
  "--crate trusty-common" "$GLOG"
assert_eq "Gate 5, no crate input: no SKIP" no "$(has "$GOUT" '[SKIP]')"

run_gate "$G5" fixture-no-contracts '' ''
assert_eq "Gate 5, crate without contracts: still checks trusty-common" \
  "--crate trusty-common" "$GLOG"

run_gate "$G5" '' tag not-a-release-tag
assert_eq "Gate 5, unparseable tag: still checks trusty-common" \
  "--crate trusty-common" "$GLOG"

run_gate "$G5" no-such-crate '' ''
assert_eq "Gate 5, unknown crate input: exit 1" 1 "$GRC"
assert_eq "Gate 5, unknown crate input: names it" yes "$(has "$GOUT" 'no-such-crate')"
assert_eq "Gate 5, unknown crate input: checks nothing" '' "$GLOG"

run_gate "$G5" '' branch main 1
assert_eq "Gate 5, drift in trusty-common: exit 1" 1 "$GRC"

mv "${FIX}/crates/trusty-common/contracts.json" "${FIX}/contracts.json.aside"
run_gate "$G5" '' branch main
assert_eq "Gate 5, no contracts.json anywhere: exit 1" 1 "$GRC"
mv "${FIX}/contracts.json.aside" "${FIX}/crates/trusty-common/contracts.json"

echo "contract-target: Gate 6 diffs the resolved crate"
run_gate "$G6" trusty-common '' ''
assert_eq "Gate 6, trusty-common input: exit 0 (no-baseline SKIP)" 0 "$GRC"
assert_eq "Gate 6, trusty-common input: diffs its contracts.json" yes \
  "$(has "$GLOG" 'crates/trusty-common/contracts.json')"
assert_eq "Gate 6, trusty-common input: labelled trusty-common" yes \
  "$(has "$GOUT" 'baseline for trusty-common (current 9.9.9)')"

run_gate "$G6" '' branch main
assert_eq "Gate 6, no crate input: exit 0" 0 "$GRC"
assert_eq "Gate 6, no crate input: annotated SKIP" yes \
  "$(has "$GOUT" '::warning::[SKIP] contract-diff:')"
assert_eq "Gate 6, no crate input: diffs nothing" '' "$GLOG"

run_gate "$G6" no-such-crate '' ''
assert_eq "Gate 6, unknown crate input: exit 1" 1 "$GRC"

echo "contract-target: workflow wiring"
# arms_listing_1 <case line>: "yes" when an arm other than `*)` lists exit
# code 1 among its patterns — that would turn an unknown crate into a pass.
arms_listing_1() {
  awk '{
    sub(/.*case "\$rc" in /, ""); sub(/esac.*/, "")
    n = split($0, arms, ";;")
    for (i = 1; i <= n; i++) {
      p = arms[i]; sub(/^[[:space:]]+/, "", p); sub(/\).*/, "", p)
      m = split(p, toks, "|")
      for (j = 1; j <= m; j++) { t = toks[j]; gsub(/[[:space:]]/, "", t); if (t == "1") hit = 1 }
    }
  } END { print (hit ? "yes" : "no") }' <<<"$1"
}
for gate in "$G5" "$G6"; do
  text="$(step "$gate")"
  assert_eq "${gate} step found" yes "$( [ -n "$text" ] && echo yes || echo no)"
  assert_eq "${gate} calls the resolver" yes \
    "$(has "$text" 'scripts/resolve_contract_target.sh')"
  # shellcheck disable=SC2016  # a literal `$rc` in the workflow text
  case_line="$(grep -F 'case "$rc" in' <<<"$text" | head -n 1)"
  assert_eq "${gate} resolver case has a '*) … exit 1' arm" yes \
    "$(if grep -qE '[*]\)[^)]*exit 1[[:space:]]*;;' <<<"$case_line"; then echo yes; else echo no; fi)"
  assert_eq "${gate} no other arm lists exit code 1" no "$(arms_listing_1 "$case_line")"
  assert_eq "${gate} has no crates/trusty-common path" no "$(has "$text" 'crates/trusty-common')"
  assert_eq "${gate} has no --crate trusty-common" no "$(has "$text" '--crate trusty-common')"
  assert_eq "${gate} has no trusty-common default" no "$(has "$text" "'trusty-common'")"
done

echo
if [ "$FAILURES" -ne 0 ]; then
  echo "contract_target_selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "contract_target_selftest: all ${CASES} case(s) passed"

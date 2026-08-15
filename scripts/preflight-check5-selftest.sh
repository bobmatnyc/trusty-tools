#!/usr/bin/env bash
#
# preflight-check5-selftest.sh — the decision half of preflight CHECK 5 (#5620).
#
# Why: scripts/check_semver.sh has had a self-test since #5050, and it has been
#   catching real fail-opens ever since. What had none was the DECISION laid over
#   its output — preflight-publish.sh CHECK 5, which reads the gate's result and
#   answers the only question that matters at that moment: publish, or stop.
#   That half was untested because running the real gate costs four minutes of
#   rustdoc per case, and it is the half that was wrong. On 2026-08-12 the
#   trusty-review 0.16.0 publish printed
#
#       [PASS] semver: semver gate: scanned (explicit); 0 crate(s) checked,
#              0 skipped, 1 inventory NOT computed — OK.
#
#   and proceeded. cargo-semver-checks had exited 101 without comparing
#   anything: trusty-review 0.15.0 cannot be documented, so rustdoc never built
#   the baseline. The gate said so on its own line. CHECK 5 read the exit status
#   alone, and exit 0 was PASS.
#
# What: drives semver_decide() — preflight-publish.sh's whole CHECK 5 decision —
#   over captured check_semver.sh output, asserting the label AND the permit/stop
#   for every way that gate can conclude. No network, no cargo, no rustdoc; the
#   file runs in under a second.
#
# THE GOVERNING ASSERTION, which every case is a special case of: a reader of
#   CHECK 5's output can tell "nothing was wrong" from "nothing was examined".
#   Mechanically, `0 crate(s) compared` and `[PASS]` are unreachable together.
#
#   Cases, and the arm each pins:
#     1.  checked clean       real tga 2.18.0 -> 2.19.0, 196 pass. [PASS], and
#                             the line states how many crates it compared. This
#                             is what proves the rest fail on classification
#                             rather than because every path now stops.
#     2.  inventory clean     the advisory arm RAN. An inventory is a
#                             comparison, so [PASS] — the fix must not turn the
#                             already-breaking arm into a blanket stop.
#     3.  inventory blind     THE DEFECT, verbatim from the trusty-review
#                             0.16.0 run. Gate exit 0, nothing compared.
#                             Must [FAIL] and must NOT print PASS.
#     4.  recorded skip       real trusty-mpm, excluded by
#                             semver-checks-crate-exclusions.tsv. Nothing was
#                             comparable, which is a fact about the crate and
#                             already recorded in a reviewable file — so it
#                             permits, and still must not print PASS.
#     5.  no verdict (exit 3) real registry-unreachable run. Already stopped
#                             before this change; pinned so the other arm's fix
#                             does not quietly loosen it.
#     6.  break (exit 1)      a computed verdict. Must [FAIL] with the
#                             version-bump remedy.
#     7.  no summary          gate exit 0 with a summary line this script cannot
#                             parse. Must [FAIL]: a reworded summary makes CHECK
#                             5 red, never green.
#     8.  gate malfunction    an undocumented exit status. Must [FAIL].
#
#   Override cases, all against case 3's blind fixture:
#     9.  reason given        [WARN], permits, and echoes the reason VERBATIM —
#                             the reason is the entire disclosure, so a run that
#                             swallowed it would record that a publish was
#                             allowed without recording why.
#     10. empty reason        set with nothing in it is REFUSED, not honoured.
#     11. break + override    a computed break is NOT override-able. The
#                             override covers a gate that could not run; exit 1
#                             is the gate running and saying no.
#     12. skip is unforced    the recorded-skip arm permits with NO override set,
#                             so trusty-mpm does not need one on every publish.
#                             An override that is always set is not an override.
#
#   Type-differ cases, driving semver_types_decide(). CHECK 5 now also runs
#   scripts/check_semver_types.sh, which compares the types cargo-semver-checks
#   does not read. It is ADVISORY: none of these may change the publish decision.
#     13. differ clean        ran, compared >= 1 position, found nothing.
#                             [PASS] semver-types:, and the line states the count.
#     14. differ found        real tga Vec<T> -> Result<Vec<T>>. [WARN], lists the
#                             items, and STILL PERMITS — an advisory check that
#                             blocks is a different decision than the one taken.
#     15. differ no verdict   the arm this split exists for. A differ that could
#                             not run must be legible as "did not examine", never
#                             borrow [PASS], and never fail the publish. #5620 in
#                             an advisory costume: a check nobody is blocked by is
#                             the cheapest place for a silent skip to hide.
#     16. differ no marker    exit 0 with no `compared:` count. Positive evidence
#                             is required for the clean arm, so a malfunctioning
#                             differ lands in NO VERDICT rather than in [PASS].
#
# HOW IT DRIVES THE REAL DECISION: the two functions are lifted out of
#   preflight-publish.sh BY PATTERN (the same awk-extraction
#   check_semver_selftest.sh uses for release_type), so this exercises the
#   shipped definitions rather than a copy that can drift. check5_semver's own
#   `bash "${REPO_ROOT}/scripts/check_semver.sh"` call is satisfied by pointing
#   REPO_ROOT at a scratch directory whose scripts/check_semver.sh replays a
#   fixture at a chosen exit status — so the run under test goes through the
#   real invocation path, not a shortcut around it.
#
#   PREFLIGHT_SELFTEST_SCRIPT points this at a different preflight-publish.sh.
#   Its purpose is the red-then-green proof: run against
#   `git show <pre-fix-commit>:scripts/preflight-publish.sh` and case 3 FAILS,
#   because that revision prints [PASS] over the trusty-review run. A regression
#   test that passes on both sides of the fix is not testing the fix.
#
# Usage:  bash scripts/preflight-check5-selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). No cargo, no
# network, no python3.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_TOP="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
UNDER_TEST="${PREFLIGHT_SELFTEST_SCRIPT:-${REPO_TOP}/scripts/preflight-publish.sh}"
FIXTURES="${REPO_TOP}/scripts/test-data/preflight-check5"

if [[ ! -f "$UNDER_TEST" ]]; then
  echo "SELF-TEST FAIL: no script to test at ${UNDER_TEST}" >&2
  exit 1
fi

PASSED=0
FAILED=0

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# ---------------------------------------------------------------------------
# Scratch repo root. check5_semver invokes
# "${REPO_ROOT}/scripts/check_semver.sh"; this one replays a fixture.
# ---------------------------------------------------------------------------
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check5.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p "${SCRATCH}/scripts"
cat > "${SCRATCH}/scripts/check_semver.sh" <<'STUB'
#!/usr/bin/env bash
# Stub gate for preflight-check5-selftest.sh: replays a captured check_semver.sh
# run at a chosen exit status. The DECISION is what is under test, not the gate.
cat "$SELFTEST_FIXTURE"
exit "${SELFTEST_GATE_RC:-0}"
STUB
chmod +x "${SCRATCH}/scripts/check_semver.sh"

# check5_semver also runs the type differ now. Same replay shape, separate
# fixture and status, so a case can hold the gate fixed and vary the differ.
# Defaulting to a clean differ run keeps the cases above about semver_decide.
cat > "${SCRATCH}/scripts/check_semver_types.sh" <<'STUB'
#!/usr/bin/env bash
# Stub type differ for preflight-check5-selftest.sh: replays captured
# check_semver_types.sh output at a chosen exit status.
if [[ -n "${SELFTEST_TYPES_FIXTURE:-}" ]]; then
  cat "$SELFTEST_TYPES_FIXTURE"
else
  echo "compared: 100 public item(s); 0 changed, 0 removed, 0 added"
  echo "semver type differ: 100 public item position(s) compared, 0 type change(s) — OK."
fi
exit "${SELFTEST_TYPES_RC:-0}"
STUB
chmod +x "${SCRATCH}/scripts/check_semver_types.sh"

# ---------------------------------------------------------------------------
# run_decision <fixture> <gate-rc> — run the shipped CHECK 5 end to end and
# print `<return-status>` on the first line, then everything it wrote.
#
# The subshell is what lets a case set PREFLIGHT_SEMVER_UNVERIFIED (or not) and
# have the next case see a clean environment.
# ---------------------------------------------------------------------------
# shellcheck disable=SC2034  # PKG_NAME/VERSION/MANIFEST/REPO_ROOT/TMP_SEMVER are read
# by the functions eval'd below, which shellcheck cannot see into.
run_decision() {
  local fixture="$1" gate_rc="$2"
  (
    set +e
    # Globals the extracted functions read. MANIFEST appears only in the
    # break-remedy text; PKG_NAME/VERSION are the crate under test.
    PKG_NAME="stub-crate"
    VERSION="9.9.9"
    MANIFEST="crates/stub-crate/Cargo.toml"
    REPO_ROOT="$SCRATCH"
    TMP_SEMVER="$(mktemp "${SCRATCH}/log.XXXXXX")"
    SELFTEST_FIXTURE="${FIXTURES}/${fixture}"
    SELFTEST_GATE_RC="$gate_rc"
    export SELFTEST_FIXTURE SELFTEST_GATE_RC

    # The shipped definitions, lifted by pattern so a drifted copy cannot be
    # what passes. A missing function is a loud failure, not a silent skip.
    eval "$(awk '/^semver_decide\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^semver_types_decide\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^semver_types_advisory\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^check5_semver\(\) \{/,/^\}/' "$UNDER_TEST")"
    if ! declare -f check5_semver > /dev/null; then
      echo "127"
      echo "SELF-TEST HARNESS: ${UNDER_TEST} defines no check5_semver()"
      exit 0
    fi

    out="$(check5_semver 2>&1)"
    rc=$?
    echo "$rc"
    printf '%s\n' "$out"
  )
}

# ---------------------------------------------------------------------------
# assert_case <name> <fixture> <gate-rc> <want-status> <want-label>
#             <must-contain> <must-not-contain, or "-">
# ---------------------------------------------------------------------------
assert_case() {
  local name="$1" fixture="$2" gate_rc="$3" want_status="$4" want_label="$5"
  local must_have="$6" must_not="$7"
  local raw status body

  raw="$(run_decision "$fixture" "$gate_rc")"
  status="$(printf '%s\n' "$raw" | sed -n 1p)"
  body="$(printf '%s\n' "$raw" | sed '1d')"

  if [[ "$status" != "$want_status" ]]; then
    fail_case "${name}: expected the decision to return ${want_status} (0=permit, 1=stop), got ${status}" "$body"
  elif [[ "$body" != *"$want_label"* ]]; then
    fail_case "${name}: expected a ${want_label} line" "$body"
  elif [[ "$body" != *"$must_have"* ]]; then
    fail_case "${name}: output never said '${must_have}'" "$body"
  elif [[ "$must_not" != "-" && "$body" == *"$must_not"* ]]; then
    fail_case "${name}: output wrongly said '${must_not}'" "$body"
  else
    pass_case "${name} -> ${want_label}, decision returns ${status}"
  fi
}

# ===========================================================================
# 1-8. Every way check_semver.sh can conclude.
#
# The `[PASS]` in the must-not column of cases 3-8 is the governing assertion:
# whatever else those arms print, they must not be readable as a verified pass.
# ===========================================================================
assert_case "checked clean" \
  checked-clean.out 0 0 "[PASS] semver:" "1 crate(s) compared" "NOT VERIFIED"

assert_case "inventory clean (advisory arm ran)" \
  inventory-clean.out 0 0 "[PASS] semver:" "1 crate(s) compared" "NOT VERIFIED"

assert_case "inventory blind (the trusty-review 0.16.0 defect)" \
  inventory-blind.out 0 1 "[FAIL]" "0 crate(s) were compared" "[PASS] semver:"

assert_case "recorded skip (excluded crate)" \
  recorded-skip.out 0 0 "[SKIP]" "NOT VERIFIED" "[PASS] semver:"

assert_case "no verdict (gate exit 3)" \
  no-verdict.out 3 1 "[FAIL]" "0 crate(s) were compared" "[PASS] semver:"

assert_case "computed break (gate exit 1)" \
  break.out 1 1 "[FAIL]" "without a breaking" "[PASS] semver:"

assert_case "summary line unparsable" \
  no-summary.out 0 1 "[FAIL]" "no summary line this script could read" "[PASS] semver:"

assert_case "gate malfunction (undocumented exit)" \
  checked-clean.out 42 1 "[FAIL]" "not one of its documented statuses" "[PASS] semver:"

# ===========================================================================
# 9-12. The override.
# ===========================================================================
REASON="0.15.0 baseline references the profile module removed in #5611"

# --- 9. A reason permits, warns, and is echoed verbatim.
raw="$(PREFLIGHT_SEMVER_UNVERIFIED="$REASON" run_decision inventory-blind.out 0)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "override/reason: an explicit reason must permit the publish (got ${status})" "$body"
elif [[ "$body" != *"[WARN]"* ]]; then
  fail_case "override/reason: expected a [WARN] line" "$body"
elif [[ "$body" == *"[PASS] semver:"* ]]; then
  fail_case "override/reason: an overridden publish printed PASS — it verified nothing" "$body"
elif [[ "$body" != *"$REASON"* ]]; then
  fail_case "override/reason: the reason was not echoed verbatim, so the run records THAT a publish was allowed but not WHY" "$body"
else
  pass_case "an override with a reason -> [WARN], permits, echoes the reason verbatim"
fi

# --- 10. Set with nothing in it is refused. A bare flag records no why.
raw="$(PREFLIGHT_SEMVER_UNVERIFIED="   " run_decision inventory-blind.out 0)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "1" ]]; then
  fail_case "override/empty: an override with no reason must be refused, not honoured (got ${status})" "$body"
elif [[ "$body" != *"set but empty"* ]]; then
  fail_case "override/empty: stopped without saying the override was empty" "$body"
else
  pass_case "an override set with no reason is refused"
fi

# --- 11. A computed break is not override-able.
raw="$(PREFLIGHT_SEMVER_UNVERIFIED="$REASON" run_decision break.out 1)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "1" ]]; then
  fail_case "override/break: the override cleared a COMPUTED break — it covers a gate that could not run, not one that ran and said no (got ${status})" "$body"
elif [[ "$body" != *"[FAIL]"* ]]; then
  fail_case "override/break: expected a [FAIL] line" "$body"
else
  pass_case "a computed break is not override-able"
fi

# --- 12. The recorded-skip arm needs no override. trusty-mpm is excluded and
#         publishes routinely; if that arm demanded a reason string, the variable
#         would be set on every one of its publishes and stop being deliberate.
raw="$(run_decision recorded-skip.out 0)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "skip/unforced: a recorded skip must permit with NO override set (got ${status})" "$body"
elif [[ "$body" == *"PREFLIGHT_SEMVER_UNVERIFIED"* ]]; then
  fail_case "skip/unforced: the skip arm asked for an override, which would make the variable permanent for every excluded crate" "$body"
else
  pass_case "a recorded skip permits without an override"
fi

# ===========================================================================
# 13-16. The type differ's advisory line. It runs from CHECK 5 and cannot fail
#        the publish, which is exactly why its three outcomes have to stay
#        legible: a check that never blocks is one whose silence costs nothing,
#        so "did not run" must not be able to wear "found nothing"'s label.
# ===========================================================================
# run_types <types-fixture> <types-rc> — hold the gate at a clean pass and vary
# only the differ, so what these cases read is the differ's own arm.
run_types() {
  local types_fixture="$1" types_rc="$2"
  (
    SELFTEST_TYPES_FIXTURE="${FIXTURES}/${types_fixture}"
    SELFTEST_TYPES_RC="$types_rc"
    export SELFTEST_TYPES_FIXTURE SELFTEST_TYPES_RC
    run_decision checked-clean.out 0
  )
}

# --- 13. Ran, compared a real number of positions, found nothing.
raw="$(run_types types-clean.out 0)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "types/clean: the advisory must not change the decision (got ${status})" "$body"
elif [[ "$body" != *"[PASS] semver-types:"* ]]; then
  fail_case "types/clean: expected a [PASS] semver-types line" "$body"
elif [[ "$body" != *"7628 public item position(s) compared"* ]]; then
  fail_case "types/clean: the compared count was not reported" "$body"
else
  pass_case "differ ran clean -> [PASS] naming the compared count"
fi

# --- 14. Ran and found type changes. WARNs, lists them, still permits.
raw="$(run_types types-changed.out 1)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "types/changed: a type change must NOT block the publish (got ${status})" "$body"
elif [[ "$body" != *"[WARN] semver-types: 2 TYPE CHANGE(S)"* ]]; then
  fail_case "types/changed: expected a [WARN] naming the change count" "$body"
elif [[ "$body" != *"fetch_referenced_issues"* ]]; then
  fail_case "types/changed: the changed items were not listed" "$body"
elif [[ "$body" == *"[PASS] semver-types:"* ]]; then
  fail_case "types/changed: found changes and still printed a semver-types [PASS]" "$body"
else
  pass_case "differ found changes -> [WARN] listing them, publish still permitted"
fi

# --- 15. Could not answer. The outcome this whole split exists for: it must be
#         distinguishable from case 13 at a glance and must never say [PASS].
raw="$(run_types types-no-verdict.out 3)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "types/no-verdict: a differ that could not run must not fail the publish (got ${status})" "$body"
elif [[ "$body" == *"[PASS] semver-types:"* ]]; then
  fail_case "types/no-verdict: a differ that compared NOTHING printed [PASS] — this is #5620's shape" "$body"
elif [[ "$body" != *"[WARN] semver-types: NO VERDICT"* ]]; then
  fail_case "types/no-verdict: expected a [WARN] semver-types NO VERDICT line" "$body"
elif [[ "$body" != *"did not run to a conclusion"* ]]; then
  fail_case "types/no-verdict: did not say the differ failed to RUN, so it reads like a clean result" "$body"
else
  pass_case "differ could not answer -> [WARN] NO VERDICT, never [PASS], never blocking"
fi

# --- 16. Exit 0 with no 'compared:' marker. A malfunctioning differ that says
#         nothing must land in NO VERDICT, not in the clean arm — the marker is
#         positive evidence and its absence is not agreement.
raw="$(run_types no-summary.out 0)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "types/no-marker: must not block (got ${status})" "$body"
elif [[ "$body" == *"[PASS] semver-types:"* ]]; then
  fail_case "types/no-marker: exit 0 with no compared: marker printed [PASS] on no evidence" "$body"
elif [[ "$body" != *"[WARN] semver-types: NO VERDICT"* ]]; then
  fail_case "types/no-marker: expected NO VERDICT when the differ printed no count" "$body"
else
  pass_case "differ exit 0 with no compared: marker -> NO VERDICT, not a pass"
fi

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "preflight-check5-selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "preflight-check5-selftest: ${PASSED} passed, 0 failed."

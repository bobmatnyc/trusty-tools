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
#                             0.16.0 run. Gate exit 0, nothing compared. This
#                             is an INFRASTRUCTURE fault (the comparison itself
#                             never ran), not a break — must [FAIL] and must
#                             NOT print PASS.
#     4.  recorded skip       real trusty-mpm, excluded by
#                             semver-checks-crate-exclusions.tsv. Nothing was
#                             comparable, which is a fact about the crate and
#                             already recorded in a reviewable file — so it
#                             permits, and still must not print PASS.
#     5.  no verdict (exit 3) real registry-unreachable run. An infrastructure
#                             fault, same as case 3 — must [FAIL].
#     6.  break (exit 1)      a computed verdict (real trusty-mpm break-lints.out).
#                             Owner ruling 2026-09-26: this is a REPORT, not a
#                             stop — must permit (status 0), print [WARN] …
#                             RECORDED BREAK, and must NOT print [PASS]. See the
#                             "record break" cases below for what gets written.
#     7.  no summary          gate exit 0 with a summary line this script cannot
#                             parse. Must [FAIL]: a reworded summary makes CHECK
#                             5 red, never green.
#     8.  gate malfunction    an undocumented exit status. Must [FAIL].
#
#   Override cases, all against case 3's blind fixture unless noted:
#     9.  reason given        [WARN], permits, and echoes the reason VERBATIM —
#                             the reason is the entire disclosure, so a run that
#                             swallowed it would record that a publish was
#                             allowed without recording why.
#     10. empty reason        set with nothing in it is REFUSED, not honoured.
#     11. break + override    a computed break needs no override and the
#                             override changes nothing about it — the run still
#                             takes the RECORDED BREAK arm (owner ruling
#                             2026-09-26), never the UNVERIFIED one.
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
#   "record break" cases (owner ruling 2026-09-26; #8699), driving
#   semver_record_break() over the real trusty-mpm 1.6.3 -> 1.6.4 break
#   (break-lints.out, 25 distinct entries across 7 lints). Records go to
#   PREFLIGHT_SEMVER_RECORD_DIR, a temp dir OUTSIDE the scratch REPO_ROOT:
#     (r1) structured break     prints the record on stdout and writes
#                               <pkg>-<version>/scripts/semver-accepted-breaks/
#                               <pkg>-<version>.txt and .../changelog.d/ fragment
#                               under the record dir — nothing under REPO_ROOT;
#                               [WARN] RECORDED BREAK; permits.
#     (r2) idempotent re-run    running it twice over the same gate output
#                               produces BYTE-IDENTICAL files.
#     (r3) unparseable list     break.out carries no `--- failure <lint>:`
#                               blocks (semver_break_entries's ERROR arm). A gate
#                               malfunction, not a break: [FAIL], stops, writes
#                               nothing.
#     (r4) unresolved crate dir MANIFEST that names no crates/<dir>/Cargo.toml
#                               still permits and says there is no fragment.
#     (r5) check 5 + check 9    CHECK 5, CHECK 9 and CHECK 3 run in a scratch GIT
#                               repo over break-lints.out. All three permit, the
#                               tree stays clean, a committed declaration is
#                               reported on and left byte-identical, and the
#                               final summary lists the breaks and never says
#                               "Safe to publish".
#     (r6) NO VERDICT + exit 1  break-no-verdict.out: a break whose output also
#                               says part of the API was never compared. [FAIL].
#                               Same for an exit 1 with no `VERDICT: BREAK`.
#     (r7) unwritable record    a record dir under a regular file, one inside the
#                               working tree, a relative path, a `..` component,
#                               and a symlink into the tree: [FAIL] "break
#                               computed but record could not be written", stops,
#                               and creates nothing.
#   The unrelated (i) full_mode_version_is_manifest cases below are unchanged
#   by this ruling: a version argument that disagrees with the manifest is
#   still refused on a full run, independent of what CHECK 5 decides.
#
#   The pre-2026-09-26 declare-first accepted-breaks flow
#   (scripts/lib/semver_accepted_breaks.sh's semver_accept_present /
#   semver_accept_decide) is UNCHANGED and still governs the pull-request-time
#   `Public API / SemVer` check (scripts/semver_ci_accept.sh) — its own
#   coverage lives in scripts/check_semver_selftest.sh's `ci-accept/` cases,
#   out of scope here. preflight-publish.sh's CHECK 5 calls neither decision
#   function; semver_record_break only READS a committed declaration (r5).
#
# HOW IT DRIVES THE REAL DECISION: the functions are lifted out of
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
# The sourced accepted-breaks library, overridable for the same red-then-green proof.
LIB_UNDER_TEST="${PREFLIGHT_SELFTEST_LIB:-${REPO_TOP}/scripts/lib/semver_accepted_breaks.sh}"
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
# Break records must land OUTSIDE REPO_ROOT (#8699), so they get their own dir.
RECORDS="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check5-records.XXXXXX")"
trap 'rm -rf "$SCRATCH" "$RECORDS"' EXIT
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

# The scratch root doubles as REPO_ROOT; semver_record_break must write nothing
# under it. Case (r5) builds a real git repo of its own.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

# lift_functions — eval the shipped definitions, by pattern, so a drifted copy
# cannot be what passes. A function missing from an older script is simply
# absent; the case that needs it fails.
lift_functions() {
  local fn
  for fn in semver_record_root semver_record_outside_repo semver_record_write \
    semver_record_declaration_note semver_record_break semver_decide \
    semver_types_decide semver_types_advisory check5_semver \
    check3_clean_tree check9_changelog_assembled preflight_ok_summary; do
    eval "$(awk -v start="${fn}() {" 'index($0, start) == 1, /^\}/' "$UNDER_TEST")"
  done
}

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
    # Globals the extracted functions read. PKG_NAME/VERSION are the crate
    # under test; MANIFEST feeds semver_record_break's crates/<dir>/Cargo.toml
    # -> <dir> resolution, so it tracks PKG_NAME by default. Case (r4) overrides
    # it to a path with no crates/<dir>/Cargo.toml shape.
    PKG_NAME="${SELFTEST_PKG:-stub-crate}"
    VERSION="${SELFTEST_VERSION:-9.9.9}"
    # Mirrors the shipped globals: semver_decide sets this, semver_types_advisory
    # reads it. Initialised to 0 here for the same reason it is there — a run
    # that never reached the count must refuse, not inherit a stale number.
    SEMVER_GATE_COMPARED=0
    MANIFEST="${SELFTEST_MANIFEST_OVERRIDE:-crates/${PKG_NAME}/Cargo.toml}"
    CHECK_ONLY="${SELFTEST_CHECK_ONLY:-0}"
    REPO_ROOT="$SCRATCH"
    PREFLIGHT_SEMVER_RECORD_DIR="${SELFTEST_RECORD_DIR:-$RECORDS}"
    TMP_SEMVER="$(mktemp "${TMPDIR:-/tmp}/preflight-check5-log.XXXXXX")"
    SELFTEST_FIXTURE="${FIXTURES}/${fixture}"
    SELFTEST_GATE_RC="$gate_rc"
    export SELFTEST_FIXTURE SELFTEST_GATE_RC

    # The accepted-breaks library preflight-publish.sh sources, from this tree.
    # shellcheck source=lib/semver_accepted_breaks.sh
    . "$LIB_UNDER_TEST"

    lift_functions
    if ! declare -f check5_semver > /dev/null; then
      echo "127"
      echo "SELF-TEST HARNESS: ${UNDER_TEST} defines no check5_semver()"
      exit 0
    fi

    out="$(check5_semver 2>&1)"
    rc=$?
    rm -f "$TMP_SEMVER"
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

SELFTEST_PKG=trusty-mpm SELFTEST_VERSION=1.6.4 \
  assert_case "computed break (gate exit 1) — owner ruling 2026-09-26: reports, never blocks" \
  break-lints.out 1 0 "[WARN] semver: RECORDED BREAK" "break entry" "[PASS] semver:"

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

# --- 11. A computed break needs no override, and the override changes
#         nothing about it (owner ruling 2026-09-26): the run still takes the
#         RECORDED BREAK arm, never the UNVERIFIED one — the override answers
#         "the gate could not run", and this is the gate running and answering.
raw="$(PREFLIGHT_SEMVER_UNVERIFIED="$REASON" SELFTEST_PKG=trusty-mpm SELFTEST_VERSION=1.6.4 \
  run_decision break-lints.out 1)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "override/break: a computed break must permit regardless of the override (got ${status})" "$body"
elif [[ "$body" != *"[WARN] semver: RECORDED BREAK"* ]]; then
  fail_case "override/break: expected the RECORDED BREAK arm, not the override's own UNVERIFIED wording" "$body"
elif [[ "$body" == *"UNVERIFIED"* ]]; then
  fail_case "override/break: a computed break took the override's UNVERIFIED arm instead of its own" "$body"
else
  pass_case "a computed break needs no override, and the override changes nothing about it"
fi

# --- 12. The recorded-skip arm needs no override. The fixture is a trusty-mpm
#         run from when that crate was excluded (the row went in #8341); a crate
#         with no baseline or no library target still reaches the same arm, and
#         if it demanded a reason string the variable would be set on every one
#         of those publishes and stop being deliberate.
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
  local types_fixture="$1" types_rc="$2" gate_fixture="${3:-checked-clean.out}"
  (
    SELFTEST_TYPES_FIXTURE="${FIXTURES}/${types_fixture}"
    SELFTEST_TYPES_RC="$types_rc"
    export SELFTEST_TYPES_FIXTURE SELFTEST_TYPES_RC
    run_decision "$gate_fixture" 0
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

# --- 17. A gate run that compared NOTHING must not let the differ read a cache.
#         Every SKIP branch in check_semver.sh continues before cargo-semver-
#         checks runs, so a skipped crate gets no fresh rustdoc — and a stale
#         directory left at the same version string by an earlier out-of-band
#         run would be diffed against source that is not HEAD. trusty-mpm is the
#         live case: TSV-excluded and published through this path. The stub
#         differ here would happily return a clean verdict, which is the point:
#         what must stop it is the call-site condition, not the differ.
raw="$(run_types types-clean.out 0 recorded-skip.out)"
status="$(printf '%s\n' "$raw" | sed -n 1p)"
body="$(printf '%s\n' "$raw" | sed '1d')"
if [[ "$status" != "0" ]]; then
  fail_case "types/gate-skipped: must not block (got ${status})" "$body"
elif [[ "$body" == *"[PASS] semver-types:"* ]]; then
  fail_case "types/gate-skipped: read a cache this run did not build and called it a PASS" "$body"
elif [[ "$body" != *"[WARN] semver-types: NOT RUN"* ]]; then
  fail_case "types/gate-skipped: expected a NOT RUN line naming the uncompared gate" "$body"
elif [[ "$body" != *"left over from"* ]]; then
  fail_case "types/gate-skipped: did not say why the on-disk cache cannot be trusted" "$body"
else
  pass_case "gate compared nothing -> differ NOT RUN, no cache read"
fi

# ===========================================================================
# (r1)-(r4). "record break" — semver_record_break, owner ruling 2026-09-26.
#            break-lints.out is the real trusty-mpm 1.6.3 -> 1.6.4 break: 7
#            failed lints, 25 distinct entries.
# ===========================================================================

# check_raw <name> <want-status> <must-not, or -> <must-have>... — over $raw.
check_raw() {
  local name="$1" want="$2" must_not="$3" status body needle
  shift 3
  status="$(printf '%s\n' "$raw" | sed -n 1p)"
  body="$(printf '%s\n' "$raw" | sed '1d')"
  if [[ "$status" != "$want" ]]; then
    fail_case "${name}: expected the decision to return ${want}, got ${status}" "$body"
    return
  fi
  if [[ "$must_not" != "-" && "$body" == *"$must_not"* ]]; then
    fail_case "${name}: output wrongly said '${must_not}'" "$body"
    return
  fi
  for needle in "$@"; do
    if [[ "$body" != *"$needle"* ]]; then
      fail_case "${name}: output never said '${needle}'" "$body"
      return
    fi
  done
  pass_case "${name} -> decision returns ${status}"
}

run_mpm() {
  SELFTEST_PKG=trusty-mpm SELFTEST_VERSION=1.6.4 run_decision "$1" "$2"
}

# in_tree_files — every file under the scratch REPO_ROOT that a case left
# behind. semver_record_break must never write there (#8699).
in_tree_files() {
  find "$SCRATCH" -type f ! -path "${SCRATCH}/scripts/check_semver.sh" \
    ! -path "${SCRATCH}/scripts/check_semver_types.sh" | LC_ALL=C sort
}

# --- (r1) A structured break list prints the record, writes it OUTSIDE the
#          working tree, and permits.
DECL_REL="scripts/semver-accepted-breaks/trusty-mpm-1.6.4.txt"
FRAG_REL="crates/trusty-mpm/changelog.d/8699-semver-break-1.6.4.md"
REC_DIR="${RECORDS}/trusty-mpm-1.6.4"
raw="$(run_mpm break-lints.out 1)"
check_raw "record/(r1) structured break list" 0 "[PASS] semver:" \
  "[WARN] semver: RECORDED BREAK — trusty-mpm 1.6.4" \
  "--- semver break record: trusty-mpm 1.6.4 (lands as ${DECL_REL}) ---" \
  "accept  constructible_struct_adds_field field BuilderSlotResponse.slot_refused" \
  "Record written OUTSIDE the working tree: ${REC_DIR}" \
  "25 break entry(ies)"
if [[ -n "$(in_tree_files)" ]]; then
  fail_case "record/(r1) wrote inside the working tree (REPO_ROOT=${SCRATCH})" "$(in_tree_files)"
elif [[ ! -f "${REC_DIR}/${DECL_REL}" ]]; then
  fail_case "record/(r1) ${REC_DIR}/${DECL_REL} was not written" "$(find "$RECORDS" -type f 2>&1)"
elif ! grep -q '^crate   trusty-mpm$' "${REC_DIR}/${DECL_REL}" \
    || ! grep -q '^version 1.6.4$' "${REC_DIR}/${DECL_REL}" \
    || ! grep -q 'owner ruling 2026-09-26' "${REC_DIR}/${DECL_REL}" \
    || ! grep -q '^accept  constructible_struct_adds_field field BuilderSlotResponse.slot_refused$' "${REC_DIR}/${DECL_REL}"; then
  fail_case "record/(r1) the record does not follow the README row format" "$(cat "${REC_DIR}/${DECL_REL}")"
elif [[ "$(sed -n 1p "${REC_DIR}/${FRAG_REL}" 2>/dev/null)" != "Breaking" ]] \
    || ! grep -q '^- ' "${REC_DIR}/${FRAG_REL}"; then
  fail_case "record/(r1) ${REC_DIR}/${FRAG_REL} is not a Breaking fragment" "$(cat "${REC_DIR}/${FRAG_REL}" 2>&1)"
else
  pass_case "record/(r1) record and fragment written outside the working tree, none inside"
fi

# --- (r2) Idempotent: a second run over the same gate output writes the SAME
#          bytes — no duplicated accept rows, no duplicated bullets.
DECL_BEFORE="$(cat "${REC_DIR}/${DECL_REL}" 2>/dev/null)"
FRAG_BEFORE="$(cat "${REC_DIR}/${FRAG_REL}" 2>/dev/null)"
raw="$(run_mpm break-lints.out 1)"
check_raw "record/(r2) idempotent re-run" 0 "[PASS] semver:" "[WARN] semver: RECORDED BREAK"
if [[ -z "$DECL_BEFORE" || "$(cat "${REC_DIR}/${DECL_REL}")" != "$DECL_BEFORE" ]]; then
  fail_case "record/(r2) the record changed (or was never written) on a re-run over identical gate output"
elif [[ "$(cat "${REC_DIR}/${FRAG_REL}")" != "$FRAG_BEFORE" ]]; then
  fail_case "record/(r2) the fragment changed on a re-run over identical gate output"
else
  pass_case "record/(r2) a re-run over the same gate output writes byte-identical files"
fi

# --- (r3) A break list that does not parse (break.out: 9 failed lints, no
#          "--- failure <lint>:" block) is a gate malfunction, not a break: it
#          stops and records nothing.
rm -rf "${RECORDS:?}"/*
raw="$(run_decision break.out 1)"
check_raw "record/(r3) unparseable break list stops" 1 "RECORDED BREAK" \
  "[FAIL] semver: check_semver.sh exited 1 for stub-crate 9.9.9" "break list does not parse"
if [[ -n "$(in_tree_files)$(find "$RECORDS" -type f)" ]]; then
  fail_case "record/(r3) a malfunction wrote a record" "$(in_tree_files)" "$(find "$RECORDS" -type f)"
else
  pass_case "record/(r3) a malfunction writes no record anywhere"
fi

# --- (r4) A MANIFEST that resolves to no crates/<dir>/Cargo.toml still
#          permits and records; it just has no changelog fragment to propose.
raw="$(SELFTEST_VERSION=9.9.8 SELFTEST_MANIFEST_OVERRIDE=Cargo.toml run_mpm break-lints.out 1)"
check_raw "record/(r4) unresolved crate dir still permits" 0 "[PASS] semver:" \
  "[WARN] semver: RECORDED BREAK" "no changelog fragment — MANIFEST did not resolve"

# --- (r5) CHECK 5, CHECK 9 and CHECK 3 together, in a real git repo, over the
#          real break. Before #8699, CHECK 5 wrote a changelog.d/ fragment into
#          the checkout, CHECK 9 failed on it as STRANDED-FRAGMENTS, and CHECK 3
#          on the next run saw a dirty tree. A committed declaration is present
#          and covers only one of the 25 entries: it must be reported on and
#          left byte-identical. The final summary must list the breaks.
PREPO="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check5-repo.XXXXXX")"
mkdir -p "${PREPO}/scripts/semver-accepted-breaks" "${PREPO}/crates/trusty-mpm/changelog.d"
cp "${SCRATCH}/scripts/check_semver.sh" "${SCRATCH}/scripts/check_semver_types.sh" "${PREPO}/scripts/"
cp "${REPO_TOP}/scripts/check-changelog-assembled.sh" "${PREPO}/scripts/"
printf '[package]\nname = "trusty-mpm"\nversion = "1.6.4"\n' > "${PREPO}/crates/trusty-mpm/Cargo.toml"
printf '# Changelog\n\n## [1.6.4]\n\n### Fixed\n\n- a fix\n' > "${PREPO}/crates/trusty-mpm/CHANGELOG.md"
printf 'Fragments go here.\n' > "${PREPO}/crates/trusty-mpm/changelog.d/README.md"
printf 'crate   trusty-mpm\nversion 1.6.4\nreason  owner accepted the new field only\naccept  constructible_struct_adds_field BuilderSlotResponse.slot_refused\n' \
  > "${PREPO}/${DECL_REL}"
cp "${PREPO}/${DECL_REL}" "${RECORDS}/committed-decl.txt"
git -C "$PREPO" init -q
git -C "$PREPO" add -A
git -C "$PREPO" -c user.name=selftest -c user.email=selftest@example.invalid \
  -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -q -m fixture
raw="$(
  cd "$PREPO" || exit 1
  set +e
  PKG_NAME=trusty-mpm VERSION=1.6.4 MANIFEST="${PREPO}/crates/trusty-mpm/Cargo.toml"
  # shellcheck disable=SC2034  # read by the lifted functions
  CHECK_ONLY=0 REPO_ROOT="$PREPO" SEMVER_GATE_COMPARED=0
  # shellcheck disable=SC2034  # read by the lifted functions
  PREFLIGHT_SEMVER_RECORD_DIR="${RECORDS}/r5"
  TMP_SEMVER="$(mktemp "${TMPDIR:-/tmp}/preflight-check5-log.XXXXXX")"
  TMP_CHANGELOG="$(mktemp "${TMPDIR:-/tmp}/preflight-check5-cl.XXXXXX")"
  SELFTEST_FIXTURE="${FIXTURES}/break-lints.out" SELFTEST_GATE_RC=1
  export SELFTEST_FIXTURE SELFTEST_GATE_RC
  # shellcheck source=lib/semver_accepted_breaks.sh
  . "$LIB_UNDER_TEST"
  lift_functions
  # Into a file, not "$(...)": the summary reads what check5_semver set, and a
  # command substitution would run the checks in a subshell and drop it.
  run_log="$(mktemp "${TMPDIR:-/tmp}/preflight-check5-run.XXXXXX")"
  {
    check5_semver; echo "rc5=$?"
    check9_changelog_assembled; echo "rc9=$?"
    check3_clean_tree; echo "rc3=$?"
    if declare -f preflight_ok_summary > /dev/null; then
      preflight_ok_summary
    else
      echo "SELF-TEST HARNESS: ${UNDER_TEST} defines no preflight_ok_summary()"
    fi
  } > "$run_log" 2>&1
  cat "$run_log"
  rm -f "$TMP_SEMVER" "$TMP_CHANGELOG" "$run_log"
)"
if [[ "$raw" != *"rc5=0"* || "$raw" != *"rc9=0"* || "$raw" != *"rc3=0"* ]]; then
  fail_case "record/(r5) CHECK 5 + CHECK 9 + CHECK 3 over a computed break must all permit" "$raw"
elif [[ -n "$(git -C "$PREPO" status --porcelain --untracked-files=all)" ]]; then
  fail_case "record/(r5) the working tree is dirty after CHECK 5" "$(git -C "$PREPO" status --porcelain --untracked-files=all)"
elif ! cmp -s "${PREPO}/${DECL_REL}" "${RECORDS}/committed-decl.txt"; then
  fail_case "record/(r5) the committed declaration was modified" "$(cat "${PREPO}/${DECL_REL}")"
elif [[ "$raw" != *"Committed declaration ${DECL_REL} does NOT cover"* ]]; then
  fail_case "record/(r5) did not report that the committed declaration misses entries" "$raw"
elif [[ ! -f "${RECORDS}/r5/trusty-mpm-1.6.4/${DECL_REL}" ]]; then
  fail_case "record/(r5) no record outside the working tree" "$raw"
elif [[ "$raw" == *"Safe to publish"* ]]; then
  fail_case "record/(r5) the summary said 'Safe to publish' over a recorded break" "$raw"
elif [[ "$raw" != *"SHIPS"*"A PUBLIC-API BREAK: 25 break entry(ies)"* ]] \
    || ! grep -q '^    enum_variant_added: ' <<<"$raw"; then
  fail_case "record/(r5) the summary does not list the recorded breaks" "$raw"
else
  pass_case "record/(r5) check 5 + check 9 + check 3 permit, tree clean, declaration untouched, summary lists breaks"
fi
rm -rf "$PREPO"

# --- (r6) Exit 1 that is not a readable BREAK verdict stops as a malfunction:
#          a break that also reports NO VERDICT (part of the API was never
#          compared), and an exit 1 with no `VERDICT: BREAK` line at all.
rm -rf "${RECORDS:?}"/*
raw="$(run_mpm break-no-verdict.out 1)"
check_raw "record/(r6) NO VERDICT + exit 1 stops" 1 "RECORDED BREAK" \
  "[FAIL] semver: check_semver.sh exited 1 for trusty-mpm 1.6.4" "part of the API was never compared"
raw="$(run_mpm checked-clean.out 1)"
check_raw "record/(r6) exit 1 without VERDICT: BREAK stops" 1 "RECORDED BREAK" \
  "no 'VERDICT: BREAK' line"
if [[ -n "$(find "$RECORDS" -type f)" ]]; then
  fail_case "record/(r6) a malfunction wrote a record" "$(find "$RECORDS" -type f)"
fi

# --- (r7) A record that cannot be written stops the publish: a record dir
#          under a regular file (mkdir fails), and one inside the working tree.
: > "${RECORDS}/blocker"
raw="$(SELFTEST_RECORD_DIR="${RECORDS}/blocker/sub" run_mpm break-lints.out 1)"
check_raw "record/(r7) unwritable record location stops" 1 "RECORDED BREAK" \
  "[FAIL] semver: break computed but record could not be written" \
  "--- semver break record: trusty-mpm 1.6.4"
raw="$(SELFTEST_RECORD_DIR="${SCRATCH}/records-in-tree" run_mpm break-lints.out 1)"
check_raw "record/(r7) record location inside the working tree stops" 1 "RECORDED BREAK" \
  "[FAIL] semver: break computed but record could not be written" \
  "not an absolute path outside the working tree"
if [[ -e "${SCRATCH}/records-in-tree" ]]; then
  fail_case "record/(r7) created ${SCRATCH}/records-in-tree inside the working tree"
fi

# The other refusal arms of semver_record_outside_repo: a relative path, a `..`
# component, and a symlink under the record root that points into the tree.
# Each must refuse before any mkdir, so nothing appears in the tree or in cwd.
# The cases run from inside RECORDS, so a regression's relative write lands
# there and never in the checkout this self-test runs from.
ln -s "$SCRATCH" "${RECORDS}/into-tree"
for spec in "relative path|rel/dir" \
  "dot-dot component|${RECORDS}/x/../y" \
  "symlink into the working tree|${RECORDS}/into-tree/recs"; do
  raw="$(cd "$RECORDS" && SELFTEST_RECORD_DIR="${spec#*|}" run_mpm break-lints.out 1)"
  check_raw "record/(r7) ${spec%%|*} stops" 1 "RECORDED BREAK" \
    "[FAIL] semver: break computed but record could not be written" \
    "not an absolute path outside the working tree"
done
if [[ -n "$(find "$SCRATCH" -mindepth 1 ! -path "${SCRATCH}/scripts" ! -path "${SCRATCH}/scripts/*")" ]] \
    || [[ -e "${RECORDS}/rel" ]] || [[ -e "${RECORDS}/x" ]] || [[ -e "${RECORDS}/y" ]]; then
  fail_case "record/(r7) a refused record location still created something" \
    "$(find "$SCRATCH" -mindepth 1 ! -path "${SCRATCH}/scripts/*")"
else
  pass_case "record/(r7) refused locations write nothing in the tree, cwd or record root"
fi
rm -f "${RECORDS}/into-tree"

# --- (i) A full run whose version argument is not the manifest version is
#         refused; --check-only keeps the hypothetical-version preview.
# shellcheck disable=SC2034  # read by the function eval'd below
run_version_guard() {
  (
    set +e
    CHECK_ONLY="$1" VERSION="$2" MANIFEST_VERSION="$3"
    PKG_NAME="trusty-mpm" MANIFEST="crates/trusty-mpm/Cargo.toml"
    eval "$(awk '/^full_mode_version_is_manifest\(\) \{/,/^\}/' "$UNDER_TEST")"
    if ! declare -f full_mode_version_is_manifest > /dev/null; then
      echo "127"
      echo "SELF-TEST HARNESS: ${UNDER_TEST} defines no full_mode_version_is_manifest()"
      exit 0
    fi
    out="$(full_mode_version_is_manifest 2>&1)"
    echo "$?"
    printf '%s\n' "$out"
  )
}
raw="$(run_version_guard 0 1.99.0 1.6.4)"
check_raw "version/(i) full run, argument != manifest" 1 "-" \
  "[FAIL] version-arg: full mode was asked to certify trusty-mpm 1.99.0" \
  "declares version '1.6.4'"
raw="$(run_version_guard 1 1.99.0 1.6.4)"
check_raw "version/(i) --check-only keeps the preview" 0 "[FAIL]"
raw="$(run_version_guard 0 1.6.4 1.6.4)"
check_raw "version/(i) full run, argument == manifest" 0 "[FAIL]"

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "preflight-check5-selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "preflight-check5-selftest: ${PASSED} passed, 0 failed."

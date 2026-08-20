#!/usr/bin/env bash
#
# preflight-check8-selftest.sh — the decision half of preflight CHECK 8 (#5755).
#
# Why: CHECK 8 answers "did the release gate actually run, and pass, against the
#   commit I am about to publish". It used to answer a different question —
#   "does a green run exist whose `head_sha` equals this commit" — and since
#   #5741 added SHA targeting those two questions have different answers.
#   Demonstrated live on run 31874835425: dispatched with `-f sha=020c139d`, it
#   checked out and gated 020c139d, and the Actions API records its `head_sha`
#   as 3f39b79f — the ref tip at dispatch. Preflight at HEAD=3f39b79f would have
#   counted that run as evidence for a commit it never examined.
#
#   That is CHECK 6's failure in a new place, and CHECK 6 exists because
#   `tga-v2.17.0` shipped mis-tagged with every gate green. The decision had no
#   test because exercising it needs a release-shaped run history nobody can
#   conjure on demand — the same reason CHECK 5's decision went untested until
#   it was wrong (#5620).
#
# What: drives gate_decide() — preflight-publish.sh's whole CHECK 8 decision —
#   over captured Actions-API output, asserting the label AND the permit/stop
#   for every way the evidence can land. No network, no gh, no cargo; the file
#   runs in well under a second.
#
# THE GOVERNING ASSERTION, which every case is a special case of: a [PASS] means
#   some run said, in its OWN output, that it gated this exact commit. No
#   property of a run other than its self-reported target can earn a pass, and
#   in particular `head_sha` cannot.
#
#   Cases, and the arm each pins:
#     1.  green, attributed     a run that reports HEAD and concluded success.
#                               [PASS]. This is what proves the rest fail on
#                               classification rather than because every path
#                               now stops.
#     2.  THE DEFECT            verbatim run 31874835425 — green, but its
#                               resolve-sha job reports 020c139d while preflight
#                               sits at 3f39b79f. Must NOT pass. This is the
#                               whole reason the file exists.
#     3.  the mirror direction  the run that DID gate HEAD, carrying a later
#                               tip as head_sha. The old head_sha query could
#                               not see it; this one must. [PASS].
#     4.  red, attributed       a run that gated HEAD and concluded failure.
#                               [FAIL] "red gate", never "never ran" — the two
#                               are different facts.
#     5.  still running         attributed to HEAD, status in_progress. A
#                               pending gate is not a green one: [FAIL].
#     6.  nothing attributed    runs exist, none names HEAD. [FAIL] "never ran".
#     7.  empty table           no runs at all. [FAIL] "never ran".
#     8.  NOGATE ignored        runs whose resolve-sha never succeeded gated no
#                               commit. They must not count as evidence in
#                               either direction — with a green attributed run
#                               present, they must not suppress the [PASS].
#     9.  NOGATE only           the same rows alone must still [FAIL] "never
#                               ran", NOT route to the override-able unverified
#                               arm — a run that died before resolving is an
#                               answer ("it gated nothing"), not a missing one.
#     10. UNREADABLE            resolve-sha SUCCEEDED but its target could not
#                               be read. Neither red nor absent: unknown, so it
#                               routes to the unverified arm and [FAIL]s with no
#                               override set.
#     11. green beats unknown   an unreadable row alongside a green attributed
#                               run must not downgrade the [PASS]. The green run
#                               is a positive answer; the unknown one cannot
#                               subtract from it.
#     12. capped scan           the scan hit its run cap without attributing
#                               everything, so "no run gated HEAD" is a limit of
#                               the search, not a finding. Unverified, [FAIL].
#
#   Override cases, all against case 10's unreadable fixture:
#     13. reason given          [WARN], permits, and echoes the reason VERBATIM
#                               — the reason is the entire disclosure.
#     14. empty reason          set with nothing in it is REFUSED, not honoured.
#     15. red is unforced       the override does NOT cover a gate that ran and
#                               said no. Case 4 with the override set must still
#                               [FAIL] — an override that can suppress a red
#                               result is not an override, it is a bypass.
#     16. absent is unforced    same for case 6. "Never ran" is an answer this
#                               check obtained, so the could-not-read override
#                               must not reach it.
#
# THE SECOND HALF drives gate_run_target — the ATTRIBUTION, where gate_decide is
#   the DECISION — over captured Actions-API bodies, with gate_api replaced by a
#   fixture server. It exists for #6113: `gh run rerun <id> --failed` leaves the
#   jobs API's default-attempt view holding a carried-over COPY of every job that
#   did not re-execute — a fresh job id, `conclusion: success`, and `[]` where
#   the annotation should be. CHECK 8 read that copy, found nothing, and called a
#   genuinely green gate unreadable; trusty-audit 0.7.0 published on an
#   owner-authorized PREFLIGHT_GATE_UNVERIFIED override because of it.
#
#     17. THE #6113 DEFECT      run 32355453111 verbatim: attempt-2 copy job
#                               96395891848 answers `[]`, attempt-1 job
#                               96383547325 carries "Verified commit e0dfd8d7b…".
#                               gate_run_target must print that commit. Against
#                               the pre-fix script this case prints UNREADABLE.
#     18. …and it then passes   the recovered target, fed to gate_decide at that
#                               HEAD, must [PASS] — the whole point is a publish
#                               that no longer needs the override.
#     19. latest attempt wins   when the latest attempt DOES answer, an earlier
#                               attempt naming a different commit must not be
#                               consulted. The walk is a fallback, not a vote.
#     20. one attempt, no notice a run never rerun whose annotation is missing
#                               stays UNREADABLE. There is no attempt 0, and the
#                               pre-#6113 behaviour is unchanged here.
#     21. failed resolve        latest attempt's resolve job did not succeed:
#                               NOGATE, even with a green annotation on attempt
#                               1. That run gated nothing on its current attempt,
#                               and an earlier attempt cannot rescue it.
#     22. job absent            no "Resolve target commit" job at all: NOGATE.
#     23. unreadable jobs body  a body that is not a jobs list is UNREADABLE, not
#                               NOGATE — an API error must never read as "this
#                               run gated nothing".
#     24. unreadable annotations a non-list annotations body on the latest
#                               attempt routes to the walk, same as `[]`.
#     25. attempts disagree     two earlier attempts naming DIFFERENT commits is
#                               UNREADABLE. Attempts of one run share its `sha`
#                               input, so a disagreement means the walk is
#                               reading something other than what it thinks.
#     26. the walk is bounded   GATE_ATTEMPT_SCAN_CAP attempts back and no
#                               further; the answer beyond it stays UNREADABLE
#                               rather than costing an unbounded API budget.
#     27. unreachable API       the latest attempt's jobs read failing outright
#                               is UNREADABLE, with no attempt number to walk
#                               from. Unchanged by #6113.
#     28. annotations fetch     the ANNOTATIONS read failing outright, on a run
#         fails hard            with no earlier attempt: UNREADABLE. A different
#                               branch from case 24 — that one is a body that
#                               parsed to nothing, this one is gate_api itself
#                               exiting nonzero — and the branch a review found
#                               untested on PR #6115.
#     29. …and on the walk      the same hard failure on the latest attempt of a
#                               RERUN still walks back and recovers the commit
#                               attempt 1 recorded.
#
# Usage: bash scripts/preflight-check8-selftest.sh
# Exit: 0 when every case matches; 1 on the first mismatch, printing both sides.
#
# The functions under test are EXTRACTED from scripts/preflight-publish.sh BY
#   PATTERN (the same awk-extraction preflight-check5-selftest.sh uses), so this
#   file tests the shipped code rather than a copy that can drift.
#   PREFLIGHT_SELFTEST_SCRIPT points this at a different preflight-publish.sh.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_TOP="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
UNDER_TEST="${PREFLIGHT_SELFTEST_SCRIPT:-${REPO_TOP}/scripts/preflight-publish.sh}"

if [ ! -r "$UNDER_TEST" ]; then
  echo "preflight-check8-selftest: cannot read ${UNDER_TEST}" >&2
  exit 1
fi

FAILURES=0
CASES=0

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $*"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-56s -> %s\n' "$1" "$3"
  else
    fail "$1: expected '$2', got '$3'"
  fi
}

# assert_contains <label> <needle> <haystack>
assert_contains() {
  CASES=$((CASES + 1))
  case "$3" in
    *"$2"*) printf '  ok   %-56s -> contains %s\n' "$1" "$2" ;;
    *) fail "$1: output does not contain '$2'. Got: $3" ;;
  esac
}

# assert_absent <label> <needle> <haystack>
assert_absent() {
  CASES=$((CASES + 1))
  case "$3" in
    *"$2"*) fail "$1: output must NOT contain '$2'. Got: $3" ;;
    *) printf '  ok   %-56s -> lacks %s\n' "$1" "$2" ;;
  esac
}

# The two real commits from the live demonstration, at full length — CHECK 8
# matches on the 40-hex form the annotation carries, so a truncated fixture
# would test a comparison the shipped code never makes.
HEAD_SHA="3f39b79f0990506496d0e298454df6c9b7cac61d"
OTHER_SHA="020c139d4c6f4491b80e5edeb7a90f4e79ff5733"
LATER_SHA="eeca2a959424e9f0cc45ebf6ca16f16167674ed0"

# ---------------------------------------------------------------------------
# run_decide <table> [capped] — drive gate_decide in a clean subshell with the
# shipped functions extracted into it. Prints "<exit>|<stderr collapsed>".
#
# GATE_NOT_VERIFIED and PKG_NAME are the globals the extracted functions read
# and write; they are declared here exactly as the shipped script declares them.
# ---------------------------------------------------------------------------
run_decide() {
  local table="$1" capped="${2:-0}"
  local out rc
  out="$(
    set +e
    PKG_NAME="trusty-selftest"
    GATE_NOT_VERIFIED=""
    GATE_SCAN_CAP=40
    # shellcheck disable=SC2034
    eval "$(awk '/^gate_unverified\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^gate_decide\(\) \{/,/^\}/' "$UNDER_TEST")"
    # Here-string, matching how check8_prepublish_gate feeds it in production —
    # a pipe would run gate_decide in a subshell and lose the GATE_NOT_VERIFIED
    # assignment the override arm depends on.
    gate_decide "${DECIDE_HEAD:-$HEAD_SHA}" "$capped" 2>&1 >/dev/null <<< "$table"
    printf '\nEXIT=%s' "$?"
  )"
  rc="$(printf '%s' "$out" | sed -n 's/^EXIT=//p' | tail -n1)"
  out="$(printf '%s' "$out" | grep -v '^EXIT=' | tr '\n' ' ' | tr -s ' ')"
  printf '%s|%s' "${rc:-?}" "$out"
}

status_of() { printf '%s' "$1" | cut -d'|' -f1; }
text_of() { printf '%s' "$1" | cut -d'|' -f2-; }

# tbl <target> <status> <conclusion> <url> [<target> <status> ...] — one row per
# group of four. printf reuses its format for leftover arguments, which is what
# lets a multi-run fixture stay one readable call.
#
# PIPE-separated, matching the shipped table. Tab would be wrong here for the
# same reason it was wrong there: it is IFS whitespace, so `read` would collapse
# the empty <conclusion> of case 5's in-progress row and shift the url into it —
# the exact defect measured on run 31878535281.
tbl() { printf '%s|%s|%s|%s\n' "$@"; }

echo "gate_decide — attribution:"

# --- 1. green, attributed to HEAD -------------------------------------------
raw="$(run_decide "$(tbl "$HEAD_SHA" completed success https://gh/run/1)")"
assert_eq "1 green attributed: permits" "0" "$(status_of "$raw")"
assert_contains "1 green attributed: label" "[PASS]" "$(text_of "$raw")"

# --- 2. THE DEFECT: green run that gated a DIFFERENT commit ------------------
# Verbatim run 31874835425. Under the old head_sha query this row was the
# evidence CHECK 8 accepted; it must now be invisible to HEAD.
raw="$(run_decide "$(tbl "$OTHER_SHA" completed success https://gh/run/31874835425)")"
assert_eq "2 DEFECT green-elsewhere: stops" "1" "$(status_of "$raw")"
assert_absent "2 DEFECT green-elsewhere: no PASS" "[PASS]" "$(text_of "$raw")"
assert_contains "2 DEFECT green-elsewhere: never ran" "NO 'Pre-publish gate' run gated" "$(text_of "$raw")"

# --- 3. the mirror direction: a run dispatched from a LATER tip --------------
# Its head_sha is LATER_SHA, so the old query could not see it at all. Its own
# report names HEAD, so this one must.
raw="$(run_decide "$(tbl "$HEAD_SHA" completed success https://gh/run/late)")"
assert_eq "3 later-tip dispatch: permits" "0" "$(status_of "$raw")"

# --- 4. red, attributed ------------------------------------------------------
raw="$(run_decide "$(tbl "$HEAD_SHA" completed failure https://gh/run/4)")"
assert_eq "4 red attributed: stops" "1" "$(status_of "$raw")"
assert_contains "4 red attributed: names the red gate" "HAS run against" "$(text_of "$raw")"
assert_absent "4 red attributed: not 'never ran'" "NO 'Pre-publish gate' run gated" "$(text_of "$raw")"

# --- 5. still running --------------------------------------------------------
raw="$(run_decide "$(tbl "$HEAD_SHA" in_progress "" https://gh/run/5)")"
assert_eq "5 in-progress: stops" "1" "$(status_of "$raw")"
assert_contains "5 in-progress: says wait" "still-running gate is not a" "$(text_of "$raw")"
# An in-progress run is the ONLY row with an empty <conclusion>, which makes it
# the row that catches a delimiter regression: under a tab delimiter `read`
# collapses the empty field and the url shifts into $concl, printing
# "in_progress/https://…" with no url of its own (run 31878535281).
# (run_decide collapses runs of spaces, so this is the single-space form.)
assert_contains "5 in-progress: empty conclusion not collapsed" "in_progress/<none> https://gh/run/5" "$(text_of "$raw")"

# --- 6. runs exist, none attributed to HEAD ----------------------------------
raw="$(run_decide "$(tbl "$OTHER_SHA" completed success https://gh/a "$LATER_SHA" completed failure https://gh/b)")"
assert_eq "6 none attributed: stops" "1" "$(status_of "$raw")"
assert_contains "6 none attributed: never ran" "NO 'Pre-publish gate' run gated" "$(text_of "$raw")"
assert_contains "6 none attributed: prints -f sha remedy" "-f sha=" "$(text_of "$raw")"

# --- 7. empty table ----------------------------------------------------------
raw="$(run_decide "")"
assert_eq "7 empty table: stops" "1" "$(status_of "$raw")"
assert_contains "7 empty table: never ran" "NO 'Pre-publish gate' run gated" "$(text_of "$raw")"

echo "gate_decide — non-evidence rows:"

# --- 8. NOGATE rows must not suppress a real green ---------------------------
raw="$(run_decide "$(tbl NOGATE completed failure https://gh/x "$HEAD_SHA" completed success https://gh/y)")"
assert_eq "8 NOGATE beside green: permits" "0" "$(status_of "$raw")"

# --- 9. NOGATE alone is an answer, not a missing one -------------------------
raw="$(run_decide "$(tbl NOGATE completed failure https://gh/x NOGATE cancelled "" https://gh/z)")"
assert_eq "9 NOGATE only: stops" "1" "$(status_of "$raw")"
assert_contains "9 NOGATE only: never ran" "NO 'Pre-publish gate' run gated" "$(text_of "$raw")"
# Asserted as "does not take the WARN arm", NOT as "the string UNVERIFIED is
# absent" — the never-ran remedy prints PREFLIGHT_GATE_UNVERIFIED="<why>" as
# guidance, so that substring is present on the correct path too.
assert_absent "9 NOGATE only: not the unverified arm" "[WARN]" "$(text_of "$raw")"
raw="$(PREFLIGHT_GATE_UNVERIFIED="a reason" run_decide "$(tbl NOGATE completed failure https://gh/x NOGATE cancelled "" https://gh/z)")"
assert_eq "9 NOGATE only + override: still stops" "1" "$(status_of "$raw")"
assert_absent "9 NOGATE only + override: no WARN" "[WARN]" "$(text_of "$raw")"

# --- 10. UNREADABLE is unknown, not absent and not red -----------------------
raw="$(run_decide "$(tbl UNREADABLE completed success https://gh/u)")"
assert_eq "10 unreadable: stops" "1" "$(status_of "$raw")"
assert_contains "10 unreadable: could not determine" "could not determine" "$(text_of "$raw")"

# --- 11. a green attributed run outranks an unknown one ----------------------
raw="$(run_decide "$(tbl UNREADABLE completed success https://gh/u "$HEAD_SHA" completed success https://gh/y)")"
assert_eq "11 unreadable beside green: permits" "0" "$(status_of "$raw")"
assert_contains "11 unreadable beside green: label" "[PASS]" "$(text_of "$raw")"

# --- 12. a capped scan has not finished looking ------------------------------
raw="$(run_decide "$(tbl "$OTHER_SHA" completed success https://gh/a)" 1)"
assert_eq "12 capped scan: stops" "1" "$(status_of "$raw")"
assert_contains "12 capped scan: names the cap" "cap" "$(text_of "$raw")"

echo "gate_decide — the override:"

# --- 13. a reason permits, and is echoed verbatim ----------------------------
REASON="actions API is down for maintenance; gate dispatched and read by hand"
raw="$(PREFLIGHT_GATE_UNVERIFIED="$REASON" run_decide "$(tbl UNREADABLE completed success https://gh/u)")"
assert_eq "13 reason given: permits" "0" "$(status_of "$raw")"
assert_contains "13 reason given: WARN not PASS" "[WARN]" "$(text_of "$raw")"
assert_absent "13 reason given: never PASS" "[PASS]" "$(text_of "$raw")"
assert_contains "13 reason given: echoes verbatim" "$REASON" "$(text_of "$raw")"

# --- 14. an empty reason is refused -----------------------------------------
raw="$(PREFLIGHT_GATE_UNVERIFIED="" run_decide "$(tbl UNREADABLE completed success https://gh/u)")"
assert_eq "14 empty reason: stops" "1" "$(status_of "$raw")"
assert_contains "14 empty reason: says why" "takes a reason" "$(text_of "$raw")"

# --- 15. the override cannot suppress a RED gate -----------------------------
raw="$(PREFLIGHT_GATE_UNVERIFIED="$REASON" run_decide "$(tbl "$HEAD_SHA" completed failure https://gh/4)")"
assert_eq "15 red + override: still stops" "1" "$(status_of "$raw")"
assert_absent "15 red + override: no WARN" "[WARN]" "$(text_of "$raw")"

# --- 16. the override cannot manufacture a run that never happened -----------
raw="$(PREFLIGHT_GATE_UNVERIFIED="$REASON" run_decide "$(tbl "$OTHER_SHA" completed success https://gh/a)")"
assert_eq "16 absent + override: still stops" "1" "$(status_of "$raw")"
assert_absent "16 absent + override: no WARN" "[WARN]" "$(text_of "$raw")"

# ===========================================================================
# gate_run_target — the ATTRIBUTION half (#6113)
# ===========================================================================
# The three commits below are run 32355453111's, at full length: the release
# that hit this, its two job ids, and the commit only attempt 1 records.
RERUN_RUN=32355453111
RERUN_SHA="e0dfd8d7b5db2383e62cbf949ed462b6dfc5ef19"
RERUN_JOB_A2=96395891848   # the carried-over copy — success, `[]` annotations
RERUN_JOB_A1=96383547325   # the job that actually ran, and reported the commit

FIXTURE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check8-fixtures.XXXXXX")"
trap 'rm -rf "$FIXTURE_DIR"' EXIT

# fixture_key <api-path> — the filename a path is stored under. Query strings
# and slashes flatten; nothing else in these paths varies.
fixture_key() { printf '%s' "$1" | tr '/?=&' '____'; }

# fx <api-path> <json-body> — record what the API answers for one path. A path
# with NO fixture is a FAILED read (gate_api exits nonzero), which is how an
# attempt that does not exist, and an unreachable API, are both expressed.
fx() { printf '%s' "$2" > "${FIXTURE_DIR}/$(fixture_key "$1")"; }
fx_reset() { rm -f "${FIXTURE_DIR:?}"/*; }

# fixture_api <path> — stands in for gate_api. Exit 22 (curl's HTTP-error code,
# which `gh api` also returns) when the path was never recorded.
fixture_api() {
  local f
  f="${FIXTURE_DIR}/$(fixture_key "$1")"
  [ -f "$f" ] || return 22
  cat "$f"
}

# jobs_body <job-id> <conclusion> <run-attempt> — a jobs-API body shaped like
# the real one, including a second job so the name match is doing real work.
jobs_body() {
  printf '{"total_count":2,"jobs":[{"id":11,"name":"Rust tests (shard 1)","conclusion":"success","run_attempt":%s},{"id":%s,"name":"Resolve target commit","conclusion":"%s","run_attempt":%s}]}' \
    "$3" "$1" "$2" "$3"
}

# ann_body [<sha>] — a check-run annotations body. With no sha it is `[]`, which
# is verbatim what the carried-over copy answers.
ann_body() {
  if [ -z "${1:-}" ]; then printf '[]'; return; fi
  printf '[{"path":".github","start_line":1,"end_line":1,"annotation_level":"notice","title":"Pre-publish gate target","message":"Verified commit %s"}]' "$1"
}

# jobs_path <run-id> [<attempt>] / ann_path <job-id> — the two paths CHECK 8
# asks for, written once so a fixture and the code under test cannot disagree
# about the shape.
jobs_path() {
  if [ -n "${2:-}" ]; then
    printf 'actions/runs/%s/attempts/%s/jobs?per_page=100' "$1" "$2"
  else
    printf 'actions/runs/%s/jobs?per_page=100' "$1"
  fi
}
ann_path() { printf 'check-runs/%s/annotations' "$1"; }

# ---------------------------------------------------------------------------
# run_target <run-id> — drive gate_run_target in a clean subshell with the
# shipped functions extracted into it, over the fixtures recorded above.
#
# gate_api is DELIBERATELY NOT extracted: fixture_api takes its place, which is
# the whole reason the shipped code has that one-line seam. Everything below it
# — the attempt walk, the job match, the annotation parse — is the shipped code.
# ---------------------------------------------------------------------------
run_target() {
  (
    set +e
    # GATE_REPO is deliberately NOT set: only gate_api reads it, and gate_api is
    # the one function the fixture server replaces. Should a future extracted
    # function reach for it, `set -u` aborts here loudly rather than letting this
    # file test a path that quietly addressed the wrong repo.
    #
    # Read the constants from the script rather than restating them, so a rename
    # of the resolve job or a change to the cap cannot leave this file testing a
    # value the gate no longer uses.
    eval "$(grep -E '^GATE_(JOB_NAME|ATTEMPT_SCAN_CAP)=' "$UNDER_TEST")"
    eval "$(awk '/^gate_pick_job\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^gate_annotation_sha\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^gate_attempt_target\(\) \{/,/^\}/' "$UNDER_TEST")"
    eval "$(awk '/^gate_run_target\(\) \{/,/^\}/' "$UNDER_TEST")"
    gate_api() { fixture_api "$@"; }
    gate_run_target "$1"
  )
}

echo "gate_run_target — a partially-rerun run (#6113):"

# --- 17. THE DEFECT: the answer lives on an earlier attempt -------------------
# Verbatim run 32355453111. Pre-fix, gate_run_target read only the default
# attempt's copy, got `[]`, and printed UNREADABLE — which routed a green gate
# to the override arm.
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body "$RERUN_JOB_A2" success 2)"
fx "$(ann_path "$RERUN_JOB_A2")"    "$(ann_body)"
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body "$RERUN_JOB_A1" success 1)"
fx "$(ann_path "$RERUN_JOB_A1")"    "$(ann_body "$RERUN_SHA")"
assert_eq "17 rerun carryover: reads attempt 1" "$RERUN_SHA" "$(run_target "$RERUN_RUN")"

# --- 18. and the recovered target then earns a PASS --------------------------
raw="$(DECIDE_HEAD="$RERUN_SHA" run_decide "$(tbl "$(run_target "$RERUN_RUN")" completed success https://gh/run/${RERUN_RUN})")"
assert_eq "18 rerun carryover: publish permitted" "0" "$(status_of "$raw")"
assert_contains "18 rerun carryover: label" "[PASS]" "$(text_of "$raw")"
assert_absent "18 rerun carryover: no override needed" "[WARN]" "$(text_of "$raw")"

# --- 19. the latest attempt's own answer is not put to a vote ----------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body "$RERUN_JOB_A2" success 2)"
fx "$(ann_path "$RERUN_JOB_A2")"    "$(ann_body "$RERUN_SHA")"
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body "$RERUN_JOB_A1" success 1)"
fx "$(ann_path "$RERUN_JOB_A1")"    "$(ann_body "$OTHER_SHA")"
assert_eq "19 latest answers: earlier attempt ignored" "$RERUN_SHA" "$(run_target "$RERUN_RUN")"

# --- 20. a run that was never rerun has no earlier attempt to ask ------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body 7001 success 1)"
fx "$(ann_path 7001)"               "$(ann_body)"
assert_eq "20 single attempt, no notice: unreadable" "UNREADABLE" "$(run_target "$RERUN_RUN")"

echo "gate_run_target — non-evidence and unreadable states:"

# --- 21. a resolve job that did not succeed gated nothing --------------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body "$RERUN_JOB_A2" failure 2)"
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body "$RERUN_JOB_A1" success 1)"
fx "$(ann_path "$RERUN_JOB_A1")"    "$(ann_body "$RERUN_SHA")"
assert_eq "21 latest resolve failed: NOGATE not rescued" "NOGATE" "$(run_target "$RERUN_RUN")"

# --- 22. no resolve job at all -----------------------------------------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")" '{"total_count":1,"jobs":[{"id":11,"name":"Rust tests (shard 1)","conclusion":"success","run_attempt":1}]}'
assert_eq "22 no resolve job: NOGATE" "NOGATE" "$(run_target "$RERUN_RUN")"

# --- 23. an API error body is not a finding ----------------------------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")" '{"message":"Not Found","status":"404"}'
assert_eq "23 unreadable jobs body: not NOGATE" "UNREADABLE" "$(run_target "$RERUN_RUN")"

# --- 24. an unreadable annotations body routes to the walk, same as [] -------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body "$RERUN_JOB_A2" success 2)"
fx "$(ann_path "$RERUN_JOB_A2")"    '{"message":"Not Found"}'
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body "$RERUN_JOB_A1" success 1)"
fx "$(ann_path "$RERUN_JOB_A1")"    "$(ann_body "$RERUN_SHA")"
assert_eq "24 unreadable annotations: walks back" "$RERUN_SHA" "$(run_target "$RERUN_RUN")"

# --- 25. attempts that disagree are not an answer ----------------------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body 7003 success 3)"
fx "$(ann_path 7003)"               "$(ann_body)"
fx "$(jobs_path "$RERUN_RUN" 2)"    "$(jobs_body 7002 success 2)"
fx "$(ann_path 7002)"               "$(ann_body "$RERUN_SHA")"
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body 7001 success 1)"
fx "$(ann_path 7001)"               "$(ann_body "$OTHER_SHA")"
assert_eq "25 attempts disagree: unreadable" "UNREADABLE" "$(run_target "$RERUN_RUN")"

# --- 26. the walk stops at its cap, and stopping is not a green --------------
# run_attempt 9 with the annotation only on attempt 1: the cap is reached first.
fx_reset
fx "$(jobs_path "$RERUN_RUN")" "$(jobs_body 7009 success 9)"
fx "$(ann_path 7009)"          "$(ann_body)"
for n in 8 7 6 5 4 3 2; do
  fx "$(jobs_path "$RERUN_RUN" "$n")" "$(jobs_body "700${n}" success "$n")"
  fx "$(ann_path "700${n}")"          "$(ann_body)"
done
fx "$(jobs_path "$RERUN_RUN" 1)" "$(jobs_body 7001 success 1)"
fx "$(ann_path 7001)"            "$(ann_body "$RERUN_SHA")"
assert_eq "26 beyond the attempt cap: unreadable" "UNREADABLE" "$(run_target "$RERUN_RUN")"

# --- 27. the API not answering at all ----------------------------------------
fx_reset
assert_eq "27 unreachable API: unreadable" "UNREADABLE" "$(run_target "$RERUN_RUN")"

# --- 28. the ANNOTATIONS read failing outright -------------------------------
# The jobs list answers, the resolve job succeeded, and the annotations call
# itself exits nonzero — a transient API failure, not a body that parsed to
# nothing. Case 24's malformed body reaches UNREADABLE through the parser;
# this reaches it through gate_api's own exit, which is the other branch.
# Omitting the ann_path fixture is what makes the fetch fail hard.
fx_reset
fx "$(jobs_path "$RERUN_RUN")" "$(jobs_body 7001 success 1)"
assert_eq "28 annotations fetch fails hard: unreadable" "UNREADABLE" "$(run_target "$RERUN_RUN")"

# --- 29. the same failure on a rerun still walks back ------------------------
fx_reset
fx "$(jobs_path "$RERUN_RUN")"      "$(jobs_body "$RERUN_JOB_A2" success 2)"
fx "$(jobs_path "$RERUN_RUN" 1)"    "$(jobs_body "$RERUN_JOB_A1" success 1)"
fx "$(ann_path "$RERUN_JOB_A1")"    "$(ann_body "$RERUN_SHA")"
assert_eq "29 annotations fetch fails hard on rerun: walks back" "$RERUN_SHA" "$(run_target "$RERUN_RUN")"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "preflight-check8-selftest: ${CASES} assertion(s) passed."
  exit 0
fi
echo "preflight-check8-selftest: ${FAILURES} of ${CASES} assertion(s) FAILED." >&2
exit 1

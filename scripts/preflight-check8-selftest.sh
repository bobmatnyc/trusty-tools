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
    gate_decide "$HEAD_SHA" "$capped" 2>&1 >/dev/null <<< "$table"
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

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "preflight-check8-selftest: ${CASES} assertion(s) passed."
  exit 0
fi
echo "preflight-check8-selftest: ${FAILURES} of ${CASES} assertion(s) FAILED." >&2
exit 1

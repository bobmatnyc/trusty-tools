#!/usr/bin/env bash
#
# rerun_bundle_pr_current_selftest.sh — fixtures for
# scripts/rerun_bundle_pr_current.sh (issue #11342).
#
# Why: the re-run job must not go green while a PR's check still judges the old
#   main, and must not go red when nothing is stale; a script that always exits
#   0, or that re-runs a running run at once, passes the happy cases.
# What: puts a fake `gh` first on PATH. FAKE_PRS is the PR list it prints;
#   FAKE_NORUN a head SHA with no run; FAKE_OLD one whose run was created in
#   2020; FAKE_NULLSTART one whose run is queued with a null start time;
#   FAKE_BEFORE / FAKE_AFTER a head SHA whose
#   run is in progress and started before / after the push; FAKE_POLLS how many
#   status polls an in-progress run takes to complete (it refuses a re-run until
#   then); FAKE_REFUSE a run id whose re-run fails; FAKE_LIST_FAIL=1 fails the
#   PR listing. Run ids are "9<n>" for head "sha<n>". Asserts exit status and
#   output.
# Test: this IS the test. Run: bash scripts/rerun_bundle_pr_current_selftest.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${RERUN_BUNDLE_PR_CURRENT:-${HERE}/rerun_bundle_pr_current.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

mkdir -p "${TMP}/bin" "${TMP}/state"
cat > "${TMP}/bin/gh" <<'GH'
#!/usr/bin/env bash
polls() { cat "${FAKE_DIR}/polls_$1" 2>/dev/null || echo 0; }
running() { [ "$1" = "${FAKE_RUNNING:-}" ] && [ "$(polls "$1")" -lt "${FAKE_POLLS:-1}" ]; }
case "$*" in
  *"/pulls?"*)
    [ "${FAKE_LIST_FAIL:-0}" = 1 ] && { echo "HTTP 401" >&2; exit 1; }
    printf '%s' "${FAKE_PRS:-}" ;;
  *"/runs?"*)
    url="$2"; sha="${url##*head_sha=}"; sha="${sha%%&*}"
    [ "${sha}" = "${FAKE_NORUN:-}" ] && exit 0
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if [ "${sha}" = "${FAKE_BEFORE:-}" ]; then row="9${sha#sha} in_progress 2026-01-01T00:00:00Z ${now}"
    elif [ "${sha}" = "${FAKE_AFTER:-}" ]; then row="9${sha#sha} in_progress 2026-12-01T00:00:00Z ${now}"
    elif [ "${sha}" = "${FAKE_NULLSTART:-}" ]; then row="9${sha#sha} queued null ${now}"
    elif [ "${sha}" = "${FAKE_OLD:-}" ]; then row="9${sha#sha} completed 2020-01-01T00:00:00Z 2020-01-01T00:00:00Z"
    else row="9${sha#sha} completed 2026-01-01T00:00:00Z ${now}"; fi
    # Honour the --jq the caller asked for: the full row, or the id alone.
    case "$*" in *run_started_at*) echo "${row}" ;; *) echo "${row%% *}" ;; esac ;;
  *"-X POST"*)
    url="$4"; id="${url##*runs/}"; id="${id%%/*}"
    if [ "${id}" = "${FAKE_REFUSE:-}" ] || running "${id}"; then
      echo "HTTP 403: This workflow is already running" >&2; exit 1
    fi ;;
  *"/actions/runs/"*)
    url="$2"; id="${url##*runs/}"
    n=$(( $(polls "${id}") + 1 )); echo "${n}" > "${FAKE_DIR}/polls_${id}"
    if running "${id}"; then echo in_progress; else echo completed; fi ;;
  *) echo "fake gh: unexpected $*" >&2; exit 1 ;;
esac
GH
chmod +x "${TMP}/bin/gh"
# The push is mid-year, given with an offset, between the fake start times.
export PATH="${TMP}/bin:${PATH}" GITHUB_REPOSITORY=o/r PUSHED_AT=2026-06-01T00:00:00-04:00
export FAKE_DIR="${TMP}/state" WAIT_POLL=0 WAIT_MAX=60

PASS=0
FAIL=0
OUT="${TMP}/out"
# expect <case> <want-exit> <needle-or-empty>; FAKE_* set by the caller.
expect() {
  local rc=0
  rm -f "${FAKE_DIR}"/polls_*
  bash "${SCRIPT}" > "${OUT}" 2>&1 || rc=$?
  if [ "${rc}" -ne "$2" ]; then
    echo "FAIL $1: exit ${rc}, want $2"; sed 's/^/    /' "${OUT}"; FAIL=$((FAIL + 1)); return
  fi
  if [ -n "$3" ] && ! grep -qF -- "$3" "${OUT}"; then
    echo "FAIL $1: output lacks '$3'"; sed 's/^/    /' "${OUT}"; FAIL=$((FAIL + 1)); return
  fi
  echo "ok   $1"; PASS=$((PASS + 1))
}

TWO="$(printf '1 sha1\n2 sha2\n')"
export FAKE_PRS="" FAKE_NORUN="" FAKE_BEFORE="" FAKE_AFTER="" FAKE_RUNNING="" FAKE_POLLS=1
export FAKE_OLD="" FAKE_NULLSTART=""
export FAKE_REFUSE="" FAKE_LIST_FAIL=0
expect "no open PR -> pass"                       0 "0 of 0"
FAKE_PRS="${TWO}"
expect "two PRs re-run -> pass"                   0 "2 of 2"
FAKE_NORUN=sha2
expect "a PR with no run is not counted -> pass"  0 "1 of 1"
FAKE_PRS="1 sha1" FAKE_NORUN=sha1
expect "the only PR has no run -> pass"           0 "PR #1: no bundle-pr-current.yml run"
FAKE_PRS="${TWO}" FAKE_NORUN="" FAKE_REFUSE=92
expect "one completed run refused -> fail"        1 "PR #2: re-run of run 92 refused"
FAKE_PRS="1 sha1" FAKE_REFUSE=91
expect "every re-run refused -> fail"             1 "could not be re-run"
# #11342: a run that started before the push judged the old main; wait, re-run.
FAKE_REFUSE="" FAKE_BEFORE=sha1 FAKE_RUNNING=91 FAKE_POLLS=3
expect "in progress, started before push -> waits, re-runs" 0 "PR #1: re-ran bundle-pr-current.yml run 91"
WAIT_MAX=0 expect "in progress before push, past the bound -> fail" 1 "still not completed after 0s"
FAKE_BEFORE="" FAKE_AFTER=sha1
expect "in progress, started after push -> current" 0 "started after the push"
FAKE_AFTER="" FAKE_NULLSTART=sha1
expect "queued, null start time -> waits, re-runs" 0 "PR #1: re-ran bundle-pr-current.yml run 91"
FAKE_NULLSTART="" FAKE_RUNNING="" FAKE_PRS="${TWO}" FAKE_OLD=sha2 FAKE_REFUSE=92
expect "run too old to re-run -> warned, not counted" 0 "too old for GitHub to re-run"
FAKE_OLD="" FAKE_REFUSE=""
FAKE_AFTER="" FAKE_RUNNING="" FAKE_LIST_FAIL=1
expect "PR list fails -> error"                   2 "cannot list open PRs"
FAKE_LIST_FAIL=0
GITHUB_REPOSITORY="" expect "no repository -> error" 2 "GITHUB_REPOSITORY"
PUSHED_AT="" expect "no push time -> error"       2 "PUSHED_AT"

echo "rerun_bundle_pr_current selftest: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]

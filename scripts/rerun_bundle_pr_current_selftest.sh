#!/usr/bin/env bash
#
# rerun_bundle_pr_current_selftest.sh — fixtures for
# scripts/rerun_bundle_pr_current.sh (issue #11342).
#
# Why: the re-run job must not go green when it re-ran nothing; a script that
#   always exits 0 would pass every happy case.
# What: puts a fake `gh` first on PATH. FAKE_PRS is the PR list it prints,
#   FAKE_NORUN a head SHA with no run, FAKE_REFUSE a run id whose re-run fails,
#   FAKE_LIST_FAIL=1 fails the PR listing. Asserts exit status and output.
# Test: this IS the test. Run: bash scripts/rerun_bundle_pr_current_selftest.sh

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${RERUN_BUNDLE_PR_CURRENT:-${HERE}/rerun_bundle_pr_current.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

mkdir -p "${TMP}/bin"
# Run ids are "9<PR number>"; the fake answers the three calls the script makes.
cat > "${TMP}/bin/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
  *"/pulls?"*)
    [ "${FAKE_LIST_FAIL:-0}" = 1 ] && { echo "HTTP 401" >&2; exit 1; }
    printf '%s' "${FAKE_PRS:-}" ;;
  *"/runs?"*)
    url="$2"; sha="${url##*head_sha=}"; sha="${sha%%&*}"
    [ "${sha}" = "${FAKE_NORUN:-}" ] || echo "9${sha#sha}" ;;
  *"-X POST"*)
    url="$4"; id="${url##*runs/}"; id="${id%%/*}"
    [ "${id}" = "${FAKE_REFUSE:-}" ] && { echo "HTTP 403: This workflow is already running" >&2; exit 1; }
    exit 0 ;;
  *) echo "fake gh: unexpected $*" >&2; exit 1 ;;
esac
GH
chmod +x "${TMP}/bin/gh"
export PATH="${TMP}/bin:${PATH}" GITHUB_REPOSITORY=o/r

PASS=0
FAIL=0
OUT="${TMP}/out"
# expect <case> <want-exit> <needle-or-empty>; FAKE_* set by the caller.
expect() {
  local rc=0
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
export FAKE_PRS="" FAKE_NORUN="" FAKE_REFUSE="" FAKE_LIST_FAIL=0
expect "no open PR -> pass"                       0 "re-ran 0 of 0"
FAKE_PRS="${TWO}"
expect "two PRs re-run -> pass"                   0 "re-ran 2 of 2"
FAKE_NORUN=sha2
expect "one PR has no run -> pass, reported"      0 "PR #2: no bundle-pr-current.yml run"
FAKE_NORUN="" FAKE_REFUSE=92
expect "one re-run refused -> pass, reported"     0 "PR #2: re-run of run 92 refused"
FAKE_PRS="1 sha1" FAKE_REFUSE=91
expect "every re-run refused -> fail"             1 "no re-run was accepted"
FAKE_REFUSE="" FAKE_NORUN=sha1
expect "the only PR has no run -> fail"           1 "re-ran 0 of 1"
FAKE_NORUN="" FAKE_LIST_FAIL=1
expect "PR list fails -> error"                   2 "cannot list open PRs"
FAKE_LIST_FAIL=0
GITHUB_REPOSITORY="" expect "no repository -> error" 2 "GITHUB_REPOSITORY"

echo "rerun_bundle_pr_current selftest: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]

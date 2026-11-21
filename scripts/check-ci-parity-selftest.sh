#!/usr/bin/env bash
#
# check-ci-parity-selftest.sh — fixtures for scripts/check-ci-parity.sh (#11486).
#
# Why: the pre-publish gate is only dispatched by hand, so a wrong verdict is
#   seen first on a release. These fixtures run on every PR instead.
# What: puts a stub `gh` on PATH that serves canned API pages (and applies the
#   real `--jq` filter), runs the gate once per case, and asserts the exit code
#   and the message. Covers the #11486 shape (shards done, run still in
#   progress), the self-run exclusion, and every fail-closed arm.
# Test: this file is the test; ci.yml `changes` job runs it.
#
# Usage: ./scripts/check-ci-parity-selftest.sh
# Exit: 0 when every case matches; 1 otherwise.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${REPO_ROOT}/scripts/check-ci-parity.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

mkdir -p "${WORK}/bin"
cat > "${WORK}/bin/gh" <<'STUB'
#!/usr/bin/env bash
# Stub: gh api [--paginate] <path> --jq <filter>. Serves ${FIX}/<key>.json.
path="" filter="."
while [ $# -gt 0 ]; do
  case "$1" in
    api|--paginate) ;;
    --jq) filter="$2"; shift ;;
    *) path="$1" ;;
  esac
  shift
done
case "${path}" in
  */workflows/ci.yml/runs*) key="runs" ;;
  */runs/*/jobs*) key="jobs-$(printf '%s' "${path}" | sed -E 's#.*/runs/([0-9]+)/jobs.*#\1#')" ;;
  *) echo "stub gh: unexpected path ${path}" >&2; exit 2 ;;
esac
if [ ! -f "${FIX}/${key}.json" ]; then
  echo "stub gh: HTTP 502 for ${path}" >&2
  exit 1
fi
jq -c "${filter}" "${FIX}/${key}.json"
STUB
chmod +x "${WORK}/bin/gh"

FAILURES=0
CASES=0

# run_case <label> <expected-exit> <expected-message-substring>
run_case() {
  CASES=$((CASES + 1))
  local out rc
  out="$(PATH="${WORK}/bin:${PATH}" FIX="${FIX}" SHA=abc GITHUB_REPOSITORY=o/r \
    SELF_RUN_ID=900 "${GATE}" 2>&1)"
  rc=$?
  if [ "${rc}" -eq "$2" ] && printf '%s' "${out}" | grep -qF -- "$3"; then
    printf '  ok   %-52s -> exit %s\n' "$1" "${rc}"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected exit $2 with '$3', got exit ${rc}:"
    printf '%s\n' "${out}" | sed 's/^/        /'
  fi
}

new_case() {
  FIX="${WORK}/$1"
  mkdir -p "${FIX}"
}
runs() { printf '{"workflow_runs":[%s]}\n' "$1" > "${FIX}/runs.json"; }
jobs() { printf '{"jobs":[%s]}\n' "$2" > "${FIX}/jobs-$1.json"; }
run() { printf '{"id":%s,"name":"%s","status":"%s","conclusion":%s}' "$1" "$2" "$3" "$4"; }
job() { printf '{"name":"%s","status":"%s","conclusion":%s}' "$1" "$2" "$3"; }
REQ="Rust tests (pre-publish gate)"
# The other required jobs, all green; cases override one of them by name.
# others_green [skip-name]
others_green() {
  local out="" n
  for n in "Clippy" "MSRV check" "Format check"; do
    [ "${n}" = "${1:-}" ] && continue
    out="${out},$(job "${n}" completed '"success"')"
  done
  printf '%s' "${out}"
}
ALL_GREEN="$(job "${REQ}" completed '"success"')$(others_green)"

echo "check-ci-parity.sh"

# Name drift: every required job name in the gate must be a ci.yml `name:`.
CASES=$((CASES + 1))
names_out="$(sed -n 's/^REQUIRED_JOBS=(\(.*\))$/\1/p' "${GATE}" | grep -oE '"[^"]+"' | tr -d '"')"
if [ -z "${names_out}" ]; then
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: could not read REQUIRED_JOBS from ${GATE}"
else
  drift=0
  while IFS= read -r n; do
    if ! grep -qxF "    name: ${n}" "${REPO_ROOT}/.github/workflows/ci.yml"; then
      FAILURES=$((FAILURES + 1))
      drift=1
      echo "  FAIL: required job '${n}' matches no 'name:' line in ci.yml"
    fi
  done <<< "${names_out}"
  [ "${drift}" -eq 0 ] && printf '  ok   %-52s -> %s\n' "required job names match ci.yml" "$(printf '%s' "${names_out}" | paste -sd ',' -)"
fi

# #11486: required jobs done, rustdoc and the roll-up still running -> green.
new_case early
runs "$(run 1 CI in_progress null)"
jobs 1 "${ALL_GREEN},$(job "Rustdoc intra-doc links" in_progress null),$(job "CI gate" queued null)"
run_case "required jobs green, run still in progress" 0 "OK — 1 green"

# #11486 AC1: this workflow's own run is never counted, even if it is named CI.
new_case self
runs "$(run 900 CI in_progress null),$(run 1 CI completed '"success"')"
jobs 1 "${ALL_GREEN}"
run_case "own run excluded" 0 "OK — 1 green"

new_case self-only
runs "$(run 900 CI in_progress null)"
run_case "own run is the only run -> none" 1 "No CI workflow run exists"

new_case none
runs ""
run_case "no run" 1 "No CI workflow run exists"

new_case not-ci
runs "$(run 1 "Pre-publish gate" completed '"success"')"
jobs 1 "${ALL_GREEN}"
run_case "run not named CI is rejected" 1 "No CI workflow run exists"

new_case pending
runs "$(run 1 CI in_progress null)"
jobs 1 "$(job "${REQ}" in_progress null)$(others_green)"
run_case "test job still running" 1 "dispatched too early"

new_case clippy-running
runs "$(run 1 CI in_progress null)"
jobs 1 "$(job "${REQ}" completed '"success"')$(others_green Clippy),$(job Clippy in_progress null)"
run_case "Clippy still running" 1 "Clippy=pending"

new_case msrv-red
runs "$(run 1 CI completed '"failure"')"
jobs 1 "$(job "${REQ}" completed '"success"')$(others_green "MSRV check"),$(job "MSRV check" completed '"failure"')"
run_case "MSRV red" 1 "MSRV check=failure"

new_case not-created
runs "$(run 1 CI queued null)"
jobs 1 ""
run_case "required jobs not created yet" 1 "dispatched too early"

new_case missing
runs "$(run 1 CI completed '"success"')"
jobs 1 "$(job "${REQ}" completed '"success"')$(others_green "MSRV check")"
run_case "required job missing from finished run" 1 "MSRV check=absent"

new_case skipped
runs "$(run 1 CI completed '"success"')"
jobs 1 "$(job "${REQ}" completed '"skipped"')$(others_green)"
run_case "required job skipped" 1 "=skipped"

new_case cancelled
runs "$(run 1 CI completed '"cancelled"')"
jobs 1 "$(job "${REQ}" completed '"cancelled"')$(others_green)"
run_case "required job cancelled" 1 "red job(s)"

new_case other-red
runs "$(run 1 CI in_progress null)"
jobs 1 "${ALL_GREEN},$(job "Docs build" completed '"failure"')"
run_case "required green but another job red" 1 "Docs build=failure"

new_case rerun
runs "$(run 1 CI completed '"failure"'),$(run 2 CI completed '"success"')"
jobs 1 "$(job "${REQ}" completed '"failure"')$(others_green)"
jobs 2 "${ALL_GREEN}"
run_case "red run plus green re-run" 0 "OK — 1 green"

new_case runs-api-down
run_case "run list unreadable" 1 "cannot count"

new_case jobs-api-down
runs "$(run 1 CI completed '"success"')"
run_case "job list unreadable" 1 "cannot count"

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check-ci-parity-selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "check-ci-parity-selftest: all ${CASES} cases passed"

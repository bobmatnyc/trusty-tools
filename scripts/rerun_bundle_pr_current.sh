#!/usr/bin/env bash
#
# rerun_bundle_pr_current.sh — after a bundle change lands on main, re-run the
# `Architect bundle PR is current with main` check of every open PR (#11342).
#
# Why: bundle-pr-current.yml runs when a PR changes, not when main moves. A
#   bundle PR that passed stays green after another bundle PR merges first, and
#   its own merge then turns main red (#11333 + #11334). A re-run re-fetches
#   main, so it fails the PR that now lacks main's bundle commit.
# What: lists the open PRs that target BASE_BRANCH (default main) and finds the
#   latest `pull_request` run of BUNDLE_PR_WORKFLOW (default
#   bundle-pr-current.yml) for each PR's head SHA.
#   - No run: nothing stale to fail; logged, not counted.
#   - Completed run: re-run through the Actions API.
#   - Queued or in-progress run that started after PUSHED_AT: it fetches the
#     new main itself; counted as current.
#   - Queued or in-progress run that started before PUSHED_AT: it may have
#     judged the old main, and the API refuses to re-run a running run. Poll
#     until it completes (WAIT_MAX seconds in total for the whole job, default
#     180, every WAIT_POLL seconds, default 15), then re-run it.
#   A refused re-run, or a run still not completed at the bound, is a
#   ::warning:: line and fails the job.
# Exit: 0 when every counted PR is re-run or current (or none is counted);
#   1 when any counted PR could not be re-run, so a stale green is never
#   silent; 2 on missing input or when the open PRs cannot be listed.
# Not guaranteed: GitHub re-runs only runs from the last 30 days; an older run
#   is refused and fails the job, and a push to that PR is then the remedy.
# Env: GITHUB_REPOSITORY (owner/repo) and PUSHED_AT (the push's
#   head_commit.timestamp, ISO 8601) required; GH_TOKEN for gh; BASE_BRANCH,
#   BUNDLE_PR_WORKFLOW, WAIT_MAX, WAIT_POLL.
# Test: scripts/rerun_bundle_pr_current_selftest.sh

set -uo pipefail

REPO="${GITHUB_REPOSITORY:-}"
BASE="${BASE_BRANCH:-main}"
WORKFLOW="${BUNDLE_PR_WORKFLOW:-bundle-pr-current.yml}"
WAIT_MAX="${WAIT_MAX:-180}"
WAIT_POLL="${WAIT_POLL:-15}"
[ -n "${REPO}" ] || { echo "ERROR: GITHUB_REPOSITORY is not set" >&2; exit 2; }

# epoch <iso8601>: seconds since the epoch; accepts `Z` and `+hh:mm` offsets.
epoch() {
  python3 -c 'import sys, datetime; print(int(datetime.datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00")).timestamp()))' "$1" 2>/dev/null
}
pushed="$(epoch "${PUSHED_AT:-}")" || { echo "ERROR: PUSHED_AT '${PUSHED_AT:-}' is not an ISO 8601 time" >&2; exit 2; }
deadline=$(( $(date +%s) + WAIT_MAX ))

prs="$(gh api --paginate "repos/${REPO}/pulls?state=open&base=${BASE}&per_page=100" \
  --jq '.[] | "\(.number) \(.head.sha)"')" \
  || { echo "ERROR: cannot list open PRs targeting ${BASE}" >&2; exit 2; }

# wait_done <run>: 0 once <run> is completed, 1 at the deadline.
wait_done() {
  local st
  while :; do
    st="$(gh api "repos/${REPO}/actions/runs/$1" --jq '.status' 2>/dev/null)" || st=""
    [ "${st}" = "completed" ] && return 0
    [ "$(date +%s)" -lt "${deadline}" ] || return 1
    sleep "${WAIT_POLL}"
  done
}

total=0
ok=0
while read -r num sha; do
  [ -n "${num}" ] || continue
  run="$(gh api "repos/${REPO}/actions/workflows/${WORKFLOW}/runs?event=pull_request&head_sha=${sha}&per_page=1" \
    --jq '.workflow_runs[0] // empty | "\(.id) \(.status) \(.run_started_at)"' 2>&1)" || {
    total=$((total + 1)); echo "::warning::PR #${num}: cannot list ${WORKFLOW} runs: ${run}"; continue; }
  if [ -z "${run}" ]; then
    echo "PR #${num}: no ${WORKFLOW} run for head ${sha}; nothing to re-run, not counted"
    continue
  fi
  total=$((total + 1))
  read -r id status started <<EOF
${run}
EOF
  if [ "${status}" != "completed" ]; then
    st_epoch="$(epoch "${started}")" || st_epoch=0
    if [ "${st_epoch}" -ge "${pushed}" ]; then
      echo "PR #${num}: run ${id} is ${status} and started after the push; it fetches the new ${BASE}"
      ok=$((ok + 1)); continue
    fi
    echo "PR #${num}: run ${id} is ${status} and started before the push; waiting for it to complete"
    if ! wait_done "${id}"; then
      echo "::warning::PR #${num}: run ${id} still not completed after ${WAIT_MAX}s; it may have judged the old ${BASE}, re-run it by hand"
      continue
    fi
  fi
  if err="$(gh api -X POST "repos/${REPO}/actions/runs/${id}/rerun" 2>&1)"; then
    echo "PR #${num}: re-ran ${WORKFLOW} run ${id}"
    ok=$((ok + 1))
  else
    echo "::warning::PR #${num}: re-run of run ${id} refused: ${err}"
  fi
done <<EOF
${prs}
EOF

echo "re-ran or found current ${ok} of ${total} open PR(s) with a ${WORKFLOW} run, targeting ${BASE}"
if [ "${ok}" -lt "${total}" ]; then
  echo "ERROR: $((total - ok)) PR(s) could not be re-run and may still judge an old ${BASE}" >&2
  exit 1
fi
exit 0

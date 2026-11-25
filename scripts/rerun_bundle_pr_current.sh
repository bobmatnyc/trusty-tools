#!/usr/bin/env bash
#
# rerun_bundle_pr_current.sh — after a bundle change lands on main, re-run the
# `Architect bundle PR is current with main` check of every open PR (#11342).
#
# Why: bundle-pr-current.yml runs when a PR changes, not when main moves. A
#   bundle PR that passed stays green after another bundle PR merges first, and
#   its own merge then turns main red (#11333 + #11334). A re-run re-fetches
#   main, so it fails the PR that now lacks main's bundle commit.
# What: lists the open PRs that target BASE_BRANCH (default main), finds the
#   latest `pull_request` run of BUNDLE_PR_WORKFLOW (default
#   bundle-pr-current.yml) for each PR's head SHA, and asks the Actions API to
#   re-run it. A PR with no run, or a re-run the API refuses (for example a run
#   still in progress), is reported with a ::warning:: line.
# Exit: 0 when there is no open PR or at least one re-run was accepted;
#   1 when every PR failed to re-run, so a broken token or API is never a
#   silent green; 2 when the open PRs cannot be listed.
# Env: GITHUB_REPOSITORY (owner/repo, required), GH_TOKEN for gh,
#   BASE_BRANCH, BUNDLE_PR_WORKFLOW.
# Test: scripts/rerun_bundle_pr_current_selftest.sh

set -uo pipefail

REPO="${GITHUB_REPOSITORY:-}"
BASE="${BASE_BRANCH:-main}"
WORKFLOW="${BUNDLE_PR_WORKFLOW:-bundle-pr-current.yml}"
[ -n "${REPO}" ] || { echo "ERROR: GITHUB_REPOSITORY is not set" >&2; exit 2; }

prs="$(gh api --paginate "repos/${REPO}/pulls?state=open&base=${BASE}&per_page=100" \
  --jq '.[] | "\(.number) \(.head.sha)"')" \
  || { echo "ERROR: cannot list open PRs targeting ${BASE}" >&2; exit 2; }

total=0
ok=0
while read -r num sha; do
  [ -n "${num}" ] || continue
  total=$((total + 1))
  run="$(gh api "repos/${REPO}/actions/workflows/${WORKFLOW}/runs?event=pull_request&head_sha=${sha}&per_page=1" \
    --jq '.workflow_runs[0].id // empty' 2>&1)" || {
    echo "::warning::PR #${num}: cannot list ${WORKFLOW} runs: ${run}"; continue; }
  if [ -z "${run}" ]; then
    echo "::warning::PR #${num}: no ${WORKFLOW} run for head ${sha}; nothing to re-run"
    continue
  fi
  if err="$(gh api -X POST "repos/${REPO}/actions/runs/${run}/rerun" 2>&1)"; then
    echo "PR #${num}: re-ran ${WORKFLOW} run ${run}"
    ok=$((ok + 1))
  else
    echo "::warning::PR #${num}: re-run of run ${run} refused: ${err}"
  fi
done <<EOF
${prs}
EOF

echo "re-ran ${ok} of ${total} open PR(s) targeting ${BASE}"
if [ "${total}" -gt 0 ] && [ "${ok}" -eq 0 ]; then
  echo "ERROR: no re-run was accepted; the open bundle PRs still judge an old ${BASE}" >&2
  exit 1
fi
exit 0

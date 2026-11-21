#!/usr/bin/env bash
#
# check-ci-parity.sh — the pre-publish gate's "green CI for this exact commit"
# check (pre-publish.yml, job `ci-parity`). See #11486, #9626.
#
# Why: the gate used to read the whole CI run's `status`. That run stays
#   `in_progress` for minutes after the test shards finish (rustdoc links,
#   the `CI gate` roll-up, the red-main notifier), so a dispatch made once the
#   shards settled still failed "dispatched too early" (run 44365083664).
# What: lists the ci.yml runs for SHA, excluding this workflow's own run
#   (SELF_RUN_ID), and reads their jobs. A run counts green when the required
#   job "Rust tests (pre-publish gate)" concluded `success` and no other job
#   concluded red. Fails closed: an API read that fails, no run, a missing,
#   skipped or cancelled required job, or any red job is a failure. Jobs still
#   running that are not required are listed as a warning, not waited on.
# Test: scripts/check-ci-parity-selftest.sh
#
# Usage: SHA=<sha> GITHUB_REPOSITORY=<owner/repo> [SELF_RUN_ID=<id>] \
#          ./scripts/check-ci-parity.sh
# Exit: 0 green; 1 anything else.

set -uo pipefail

REQUIRED_JOB="Rust tests (pre-publish gate)"
: "${SHA:?SHA is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
SELF_RUN_ID="${SELF_RUN_ID:-${GITHUB_RUN_ID:-0}}"

die() {
  echo "::error::$*"
  exit 1
}

echo "Looking for a CI workflow run against ${SHA} (excluding run ${SELF_RUN_ID})"

# #9626: scoped to ci.yml; a repo-wide list missed the CI run on release SHAs.
if ! raw="$(gh api --paginate \
  "repos/${GITHUB_REPOSITORY}/actions/workflows/ci.yml/runs?head_sha=${SHA}&per_page=100" \
  --jq '.workflow_runs[] | {id, name, status, conclusion}')"; then
  die "Could not list CI runs for ${SHA}; the gate cannot count, so it fails."
fi
# #11486: never count this workflow's own run.
if ! runs="$(printf '%s' "${raw}" | jq -s --argjson self "${SELF_RUN_ID}" \
  '[.[] | select(.name == "CI" and .id != $self)]')"; then
  die "Could not parse the CI run list for ${SHA}; the gate cannot count, so it fails."
fi
echo "CI runs for this SHA: ${runs}"

count="$(printf '%s' "${runs}" | jq 'length')"
if [ "${count}" -eq 0 ]; then
  die "No CI workflow run exists for ${SHA}. The nextest shards have never executed against this tree, so nothing here can claim they passed. Push this commit to main (or dispatch CI against it) and re-run."
fi

green=0
pending=0
red=0
for id in $(printf '%s' "${runs}" | jq -r '.[].id'); do
  run_status="$(printf '%s' "${runs}" | jq -r --argjson id "${id}" '.[] | select(.id == $id) | .status')"
  if ! jobs_raw="$(gh api --paginate \
    "repos/${GITHUB_REPOSITORY}/actions/runs/${id}/jobs?per_page=100" \
    --jq '.jobs[] | {name, status, conclusion}')"; then
    die "Could not list jobs for CI run ${id}; the gate cannot count, so it fails."
  fi
  if ! jobs="$(printf '%s' "${jobs_raw}" | jq -s '.')"; then
    die "Could not parse jobs for CI run ${id}; the gate cannot count, so it fails."
  fi

  bad="$(printf '%s' "${jobs}" | jq -r '[.[] | select(.status == "completed"
      and (.conclusion | IN("failure", "cancelled", "timed_out", "startup_failure", "action_required")))
      | "\(.name)=\(.conclusion)"] | join(", ")')"
  req="$(printf '%s' "${jobs}" | jq -r --arg n "${REQUIRED_JOB}" \
    '[.[] | select(.name == $n)] | if length == 0 then "absent"
     elif any(.[]; .status == "completed" and .conclusion == "success") then "success"
     elif any(.[]; .status != "completed") then "pending"
     else (map(.conclusion // "none") | join(",")) end')"
  running="$(printf '%s' "${jobs}" | jq -r --arg n "${REQUIRED_JOB}" \
    '[.[] | select(.status != "completed" and .name != $n) | .name] | join(", ")')"

  if [ -n "${bad}" ]; then
    echo "::error::CI run ${id}: red job(s): ${bad}"
    red=$((red + 1))
  elif [ "${req}" = "success" ]; then
    echo "CI run ${id}: '${REQUIRED_JOB}' succeeded"
    if [ -n "${running}" ]; then
      echo "::warning::CI run ${id}: not waited on, still running: ${running}"
    fi
    green=$((green + 1))
  elif [ "${req}" = "pending" ] || { [ "${req}" = "absent" ] && [ "${run_status}" != "completed" ]; }; then
    echo "CI run ${id}: '${REQUIRED_JOB}' has not finished"
    pending=$((pending + 1))
  else
    echo "::error::CI run ${id}: '${REQUIRED_JOB}' is ${req} (run ${run_status}); a missing, skipped or cancelled required job proves nothing."
    red=$((red + 1))
  fi
done

if [ "${green}" -gt 0 ]; then
  echo "OK — ${green} green CI run(s) for ${SHA}"
  exit 0
fi
if [ "${pending}" -gt 0 ]; then
  die "CI for ${SHA} has not finished '${REQUIRED_JOB}' — ${pending} run(s) still pending. This gate is dispatched too early; wait for the test shards to settle, then re-run. Nothing here is red, but nothing has been proved either."
fi
die "No CI run for ${SHA} has a green '${REQUIRED_JOB}' (${red} red). A release gate cannot pass over a red CI."

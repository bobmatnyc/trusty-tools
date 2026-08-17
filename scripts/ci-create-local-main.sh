#!/usr/bin/env bash
#
# ci-create-local-main.sh — create the local `main` branch the trusty-agents git
# tests need, and let a real failure to create it fail the step (#5693).
#
# Why: `git fetch origin main:main || true` ran in ci.yml and pre-publish.yml.
#   On 2026-08-14 GitHub returned a 500 on that fetch. `|| true` swallowed it,
#   the step reported success with no local `main`, and eleven minutes later
#   `trusty-agents git::branch::tests::list_branches_includes_current` failed on
#   `assertion failed: !branches.is_empty()` — one failure out of 6223, in a
#   crate the PR did not touch (PR #5692, run 31782111082).
#
#   The `|| true` was not gratuitous, and removing it alone would turn every
#   push-to-main run red. On a `push: main` run actions/checkout checks out
#   `main` as a local branch, and git then refuses the fetch with
#   `fatal: refusing to fetch into branch 'refs/heads/main' checked out at …`
#   and exit 128 — the same exit code and the same `fatal:` prefix an
#   unreachable remote produces (measured on git 2.50.1). Exit status cannot
#   separate the expected refusal from a real failure, so this script decides on
#   the CONDITION instead of on the status.
#
# What: three steps.
#     1. `main` is already the checked-out branch -> skip the fetch. That is the
#        one case git legitimately refuses, and the branch the tests want
#        already exists.
#     2. Otherwise fetch, retrying a failure ATTEMPTS times with linear backoff
#        so a transient 5xx costs seconds instead of a ~10-minute job. A fetch
#        still failing after that fails the step.
#     3. Either way, assert the postcondition the step exists for: a local
#        `refs/heads/main` resolves afterwards.
#
# Test: no automated coverage. Step 1 runs on every `push: main` leg of ci.yml,
#   step 2 on every `workflow_dispatch` leg and on every pre-publish run.
set -euo pipefail

ATTEMPTS=3

if [ "$(git symbolic-ref --quiet --short HEAD || true)" = "main" ]; then
  echo "HEAD is already the local 'main' branch; git would refuse the fetch and nothing needs creating."
else
  attempt=1
  until git fetch origin main:main; do
    if [ "${attempt}" -ge "${ATTEMPTS}" ]; then
      echo "::error::git fetch origin main:main failed ${ATTEMPTS} times — see #5693, this used to be swallowed by '|| true'."
      exit 1
    fi
    echo "fetch attempt ${attempt}/${ATTEMPTS} failed; retrying in $((attempt * 5))s"
    sleep "$((attempt * 5))"
    attempt=$((attempt + 1))
  done
fi

if ! git show-ref --verify --quiet refs/heads/main; then
  echo "::error::no local 'main' branch after this step — the trusty-agents git tests would fail misleadingly in an unrelated crate. See #5693."
  exit 1
fi

echo "local 'main' branch present at $(git rev-parse --short main)."

#!/usr/bin/env bash
#
# classify-ci-results.sh — honest verdict for a set of CI job conclusions
# (issue #4179).
#
# Why: `ci.yml`'s "Notify on red main" job asked `contains(needs.*.result,
#   'failure')`. `cancelled` is not `failure`, so a run whose every build and
#   test job was CANCELLED skipped the alarm and the job reported success —
#   the merge commit read verified when nothing had been verified. That is not
#   a rare edge: 19 of 60 sampled push-to-main runs (32%) concluded
#   `cancelled`, because every workflow used `cancel-in-progress: true` on a
#   group keyed to `github.ref`, which on a push is `refs/heads/main` for every
#   merge — so each merge killed the previous merge's verification. Live
#   instance: run 30720348101 (head b4b91af9), four jobs cancelled, notifier
#   green.
#
#   The concurrency change in ci.yml (per-commit group on push) stops the
#   cancellations at the source. This script is the second layer: even when a
#   run IS cancelled for some other reason — a manual cancel, a runner
#   eviction — the verdict it produces refuses to call that green.
#
# What: reads `<job>=<conclusion>` pairs (whitespace- or newline-separated)
#   from $CI_JOB_RESULTS or argv and folds them into ONE verdict:
#
#     conclusion  | class         | contributes
#     ------------|---------------|-------------------------------------------
#     success     | pass          | nothing
#     failure     | red           | verdict=red
#     timed_out   | red           | verdict=red
#     cancelled   | inconclusive  | verdict=inconclusive (unless already red)
#     skipped     | inconclusive  | verdict=inconclusive (unless already red)
#     <anything>  | inconclusive  | verdict=inconclusive (unless already red)
#
#   Precedence is red > inconclusive > green. Only an all-`success` set is
#   green. `timed_out` is accepted even though GitHub's `needs.<job>.result`
#   collapses a timed-out job into `failure` and never emits it: the same
#   mapping is then correct if it is ever fed raw check-run conclusions, where
#   `timed_out` is a real value.
#
# Outputs (stdout, and appended to $GITHUB_OUTPUT when set):
#   verdict=green|red|inconclusive
#   summary=<one-line per-job breakdown>
#
# Exit: 0 always. The verdict is data for the caller; deciding whether to fail
#   the run belongs to the workflow, not here.
#
# Test: scripts/check-ci-helpers-selftest.sh (`classify-ci-results:` cases)
#   asserts the verdict for every conclusion value above, individually and in
#   precedence combinations.

set -euo pipefail

# classify_one <conclusion> — echoes `pass`, `red`, or `inconclusive`.
classify_one() {
  case "$1" in
    success) echo "pass" ;;
    failure | timed_out) echo "red" ;;
    *) echo "inconclusive" ;;
  esac
}

main() {
  local raw="${CI_JOB_RESULTS:-$*}"

  local verdict="green"
  local summary=""
  local count=0
  local pair job conclusion class

  for pair in $raw; do
    [ -n "$pair" ] || continue
    count=$((count + 1))
    job="${pair%%=*}"
    conclusion="${pair#*=}"
    class="$(classify_one "$conclusion")"

    case "$class" in
      red) verdict="red" ;;
      inconclusive) [ "$verdict" = "red" ] || verdict="inconclusive" ;;
    esac

    summary="${summary}${summary:+, }${job}: ${conclusion} (${class})"
    echo "  ${job} = ${conclusion} -> ${class}" >&2
  done

  # No inputs at all means the caller could not read the job results. That is
  # not evidence of success — fail closed, same rule as detect-docs-only.sh.
  if [ "$count" -eq 0 ]; then
    verdict="inconclusive"
    summary="no job results supplied"
    echo "classify-ci-results: no inputs — verdict=inconclusive (fail closed)" >&2
  fi

  echo "classify-ci-results: verdict=${verdict}" >&2
  echo "verdict=${verdict}"
  echo "summary=${summary}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "verdict=${verdict}" >>"${GITHUB_OUTPUT}"
    echo "summary=${summary}" >>"${GITHUB_OUTPUT}"
  fi
}

main "$@"

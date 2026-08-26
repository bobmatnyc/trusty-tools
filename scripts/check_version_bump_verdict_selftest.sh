#!/usr/bin/env bash
#
# check_version_bump_verdict_selftest.sh — verdict-integrity fixtures for the
# `pr-version-bump` job in .github/workflows/version-parity.yml (issue #6243).
#
# Why: `pr-version-bump` is a REQUIRED branch-protection context, and branch
#   protection reads the LATEST check run per (app, name, head_sha). A run that
#   concludes `success` without evaluating the check therefore does not merely
#   add a meaningless green — it OVERWRITES a completed real verdict at the
#   same SHA.
#
#   That happened on PR #6241, head `19f206de`. A `synchronize` run
#   (32803218879) ran `scripts/check-pr-version-bump.sh`, found `trusty-mpm
#   1.5.0` and `trusty-agents-common 0.6.0` changed under versions crates.io
#   already carried, and concluded `failure` at 02:55:42Z. A title/body edit 17
#   seconds later fired `pull_request: edited` with `changes.base == null`; run
#   32803235300 took the job's "report success without running the real check"
#   branch, skipped every real step, and concluded `success` at 02:55:50Z. The
#   false green is what an armed auto-merge would have acted on. Both runs were
#   `run_attempt: 1` on distinct trigger events, so no concurrency setting could
#   have collapsed them — the first had already completed when the second was
#   created.
#
#   This is the repo's canonical bug shape: a failure branch downgraded to
#   success (#5620 for the SemVer gate, #4618 for the scan floors, #4576 for
#   the changelog attribution). The gate's own logic was never wrong; the
#   workflow published a verdict the gate never produced.
#
# What: extracts the `pr-version-bump` job from the workflow and asserts the
#   one structural property that makes the incident unreachable — on a pull
#   request, no step in that job can be skipped, so the job cannot conclude
#   `success` without the real check having run. Concretely:
#
#     job-block-present         the job exists and carries steps (scan floor —
#                               an extraction that silently matches nothing
#                               would make every assertion below vacuous)
#     runs-the-real-gate        exactly one step runs check-pr-version-bump.sh
#     no-conditional-skip       EVERY step `if:` in the job is exactly
#                               `github.event_name == 'pull_request'`. A step
#                               gated on anything narrower can be skipped on a
#                               pull request while the job stays green.
#     no-verdict-flag           no step writes `$GITHUB_OUTPUT`. That is how
#                               the pre-#6243 gate step published the boolean
#                               every other step branched on; forbidding the
#                               mechanism stops the shape from being rebuilt
#                               under a different condition string.
#     no-fabricated-success     the job contains no step that announces a
#                               success it did not compute (the pre-fix
#                               "reporting success without running the real
#                               check" notice).
#     duplicates-collapse       workflow-level `cancel-in-progress` no longer
#                               carves `edited` out, so a duplicate PR run
#                               supersedes rather than races (#6243 closure
#                               condition 2).
#
#   Every case fails against the pre-#6243 file. Prove it with `--workflow`:
#
#     git show <pre-fix-rev>:.github/workflows/version-parity.yml > /tmp/pre.yml
#     bash scripts/check_version_bump_verdict_selftest.sh --workflow /tmp/pre.yml
#
#   The job-level `if:` is deliberately NOT asserted away. PR #5434 established
#   that a job-level `if:` which can be false at a PR head SHA produces a
#   `skipped` check run alongside a real one; the fix keeps the job
#   unconditional and gates its steps instead.
#
# Usage:
#   bash scripts/check_version_bump_verdict_selftest.sh
#   bash scripts/check_version_bump_verdict_selftest.sh --workflow <path>
#
# Exit: 0 when every case matches; 1 on the first mismatch, printing both sides.
#
# Test: this file IS the test. It runs as a step of the job it constrains
#   (.github/workflows/version-parity.yml, "Verdict-integrity selftest"), so
#   the invariant is re-proven on every pull request.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ORIG_PWD="${PWD}"

WORKFLOW=".github/workflows/version-parity.yml"
JOB="pr-version-bump"
EXPECTED_STEP_IF="github.event_name == 'pull_request'"

while [ $# -gt 0 ]; do
  case "$1" in
    --workflow)
      # Resolve against the CALLER's directory, not the repo root the script
      # is about to cd into — the mutation demo points this at a scratch copy.
      case "${2:?--workflow needs a path}" in
        /*) WORKFLOW="$2" ;;
        *) WORKFLOW="${ORIG_PWD}/$2" ;;
      esac
      shift 2
      ;;
    *)
      echo "usage: $0 [--workflow <path>]" >&2
      exit 2
      ;;
  esac
done

cd "${REPO_ROOT}" || exit 1

if [ ! -f "${WORKFLOW}" ]; then
  echo "check_version_bump_verdict_selftest: no such workflow: ${WORKFLOW}" >&2
  exit 1
fi

FAILURES=0
CASES=0

assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-56s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# The job block: from `  <job>:` at two-space indent up to the next key at that
# same indent. Comment lines above the next job belong to that job, not this
# one, but they carry no `if:`/`run:` keys so they cannot affect any count.
job_block="$(
  awk -v job="  ${JOB}:" '
    $0 == job { inside = 1; next }
    inside && /^  [A-Za-z_#-]/ && $0 !~ /^  #/ { exit }
    inside { print }
  ' "${WORKFLOW}"
)"

echo "pr-version-bump verdict integrity (${WORKFLOW}):"

# Scan floor. Without it a renamed job would silently zero every count below
# and the whole selftest would read as a clean bill of health (#4618's shape).
assert_eq "job-block-present: job found" "yes" \
  "$([ -n "${job_block}" ] && echo yes || echo no)"
step_count="$(printf '%s\n' "${job_block}" | grep -cE '^      - (name|uses):' || true)"
assert_eq "job-block-present: step count > 0" "yes" \
  "$([ "${step_count:-0}" -gt 0 ] && echo yes || echo no)"

assert_eq "runs-the-real-gate: check-pr-version-bump.sh invoked once" "1" \
  "$(printf '%s\n' "${job_block}" |
    grep -c 'bash scripts/check-pr-version-bump\.sh' || true)"

# Every step `if:` must be the event guard and nothing narrower. Counting the
# non-conforming ones (rather than asserting the conforming count) means a
# newly added step with a novel condition is caught too.
assert_eq "no-conditional-skip: step ifs other than the event guard" "0" \
  "$(printf '%s\n' "${job_block}" |
    grep -E '^        if:' |
    grep -vcF "if: ${EXPECTED_STEP_IF}" || true)"

assert_eq "no-verdict-flag: steps writing \$GITHUB_OUTPUT" "0" \
  "$(printf '%s\n' "${job_block}" | grep -c 'GITHUB_OUTPUT' || true)"

assert_eq "no-fabricated-success: success announced without checking" "0" \
  "$(printf '%s\n' "${job_block}" |
    grep -ci 'success without running the real check' || true)"

# #6243 closure condition 2 — workflow level, outside the job block.
assert_eq "duplicates-collapse: no edited carve-out in cancel-in-progress" "0" \
  "$(grep -E '^  cancel-in-progress:' "${WORKFLOW}" |
    grep -c "github.event.action" || true)"

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check_version_bump_verdict_selftest: ${FAILURES}/${CASES} case(s) FAILED."
  exit 1
fi
echo "check_version_bump_verdict_selftest: ${CASES}/${CASES} cases passed."

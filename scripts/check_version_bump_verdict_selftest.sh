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
#   properties that keep a `pull_request` run from concluding `success` without
#   `scripts/check-pr-version-bump.sh` having run:
#
#     job-block-present         the job exists and carries steps (scan floor —
#                               an extraction that silently matches nothing
#                               would make every assertion below vacuous)
#     runs-the-real-gate        exactly one step runs check-pr-version-bump.sh
#     no-job-level-if           the job header (job key down to `steps:`) has no
#                               `if:` key at all. A job-level `if:` that
#                               evaluates false makes the WHOLE job report
#                               `skipped`, reopening PR #5434's
#                               skipped-required-context hazard, and it sits
#                               above every step-scoped assertion here.
#     no-conditional-skip       every `if:`-shaped line in the job is EXACTLY
#                               `        if: github.event_name == 'pull_request'`,
#                               matched whole-line at its own indent. Anything
#                               narrower — a clause appended to that expression,
#                               a step output, an `if:` at another indent — can
#                               skip a step on a pull request while the job
#                               stays green.
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
#   These cover the shapes that make a step skippable or the job non-running.
#   They do NOT cover every conceivable route to a fabricated green — a second
#   job elsewhere deliberately publishing a check run under this job's `name:`
#   would race a real verdict and is not defended against here.
#
#   WHAT THE ASSERTIONS ARE PROVEN AGAINST. A bare grep guarantee is worth
#   whatever its pattern actually anchors, which is the defect the first cut of
#   this file shipped: `grep -vcF "if: <expected>"` matched as a SUBSTRING, so
#   `if: github.event_name == 'pull_request' && github.event.action != 'edited'`
#   counted as conforming and the file reported 7/7 on a workflow that
#   recreated the #6241 incident. A default run therefore does not just check
#   the live workflow — it builds mutant copies that each recreate a known
#   evasion and asserts this checker REJECTS them:
#
#     mutant/narrowed-step-if    the real check step's `if:` gains
#                                `&& github.event.action != 'edited'`
#     mutant/job-level-if        the same narrowing clause as a job-level `if:`
#     mutant/no-real-gate        the check-pr-version-bump.sh step is deleted
#
#   The first two are the mutations that passed the pre-fix checker.
#
# Usage:
#   bash scripts/check_version_bump_verdict_selftest.sh
#   bash scripts/check_version_bump_verdict_selftest.sh --workflow <path>
#
#   `--workflow` checks ONE file and skips the mutation battery — that is how
#   the battery invokes this script per mutant, and how to point it at a copy
#   of an older revision by hand.
#
# Exit: 0 when every case matches; 1 on the first mismatch, printing both sides.
#
# Test: this file IS the test, and the mutation battery is its own coverage
#   proof. It runs as a step of the job it constrains
#   (.github/workflows/version-parity.yml, "Verdict-integrity selftest"), so
#   the invariant is re-proven on every pull request.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="${REPO_ROOT}/scripts/$(basename "${BASH_SOURCE[0]}")"
ORIG_PWD="${PWD}"

SINGLE_WORKFLOW=""

while [ $# -gt 0 ]; do
  case "$1" in
    --workflow)
      # Resolve against the CALLER's directory, not the repo root the script
      # is about to cd into — the battery points this at a scratch copy.
      case "${2:?--workflow needs a path}" in
        /*) SINGLE_WORKFLOW="$2" ;;
        *) SINGLE_WORKFLOW="${ORIG_PWD}/$2" ;;
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

WORKFLOW=".github/workflows/version-parity.yml"
JOB="pr-version-bump"
# The one permitted step condition, as a whole line at its own indent. Both the
# expression AND the indent are part of the assertion: an `if:` anywhere else in
# the job is a shape this file has not reasoned about, so it fails closed.
STEP_IF_LINE="        if: github.event_name == 'pull_request'"

FAILURES=0
CASES=0

assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-58s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# job_block <file> — the job's body: from `  <job>:` at two-space indent up to
# the next key at that indent. Full-line comments are dropped so a comment that
# merely QUOTES a forbidden shape cannot trip an assertion, and so the trailing
# comment block belonging to the next job cannot either.
job_block() {
  awk -v job="  ${JOB}:" '
    $0 == job { inside = 1; next }
    inside && /^  [^ #]/ { exit }
    inside && /^[[:space:]]*#/ { next }
    inside { print }
  ' "$1"
}

# job_header <file> — the job's own keys, above `steps:`. A job-level `if:`
# lives here and is invisible to every step-scoped assertion.
job_header() {
  job_block "$1" | awk '/^    steps:/ { exit } { print }'
}

check_workflow() {
  local wf="$1" block header step_count if_lines conforming_if
  block="$(job_block "${wf}")"
  header="$(job_header "${wf}")"

  echo "pr-version-bump verdict integrity (${wf}):"

  # Scan floor. Without it a renamed job would silently zero every count below
  # and the whole selftest would read as a clean bill of health (#4618's shape).
  assert_eq "job-block-present: job found" "yes" \
    "$([ -n "${block}" ] && echo yes || echo no)"
  step_count="$(printf '%s\n' "${block}" | grep -cE '^      - (name|uses):' || true)"
  assert_eq "job-block-present: step count > 0" "yes" \
    "$([ "${step_count:-0}" -gt 0 ] && echo yes || echo no)"

  assert_eq "runs-the-real-gate: check-pr-version-bump.sh invoked once" "1" \
    "$(printf '%s\n' "${block}" |
      grep -c 'bash scripts/check-pr-version-bump\.sh' || true)"

  assert_eq "no-job-level-if: \`if:\` keys in the job header" "0" \
    "$(printf '%s\n' "${header}" | grep -cE '^[[:space:]]+if:' || true)"

  # Count every `if:`-shaped line in the job, then count the ones that are
  # EXACTLY the permitted line. The difference is what evades the guarantee —
  # a narrowing clause appended to the expected expression, a step-output
  # condition, or an `if:` at an unexpected indent. Whole-line `-x` matching is
  # the point: the pre-fix substring form counted the first of those three as
  # conforming and reported a clean pass on a workflow that recreated #6241.
  if_lines="$(printf '%s\n' "${block}" | grep -cE '^[[:space:]]+if:' || true)"
  conforming_if="$(printf '%s\n' "${block}" | grep -cxF "${STEP_IF_LINE}" || true)"
  assert_eq "no-conditional-skip: ifs that are not the event guard" "0" \
    "$((if_lines - conforming_if))"

  assert_eq "no-verdict-flag: steps writing \$GITHUB_OUTPUT" "0" \
    "$(printf '%s\n' "${block}" | grep -c 'GITHUB_OUTPUT' || true)"

  assert_eq "no-fabricated-success: success announced without checking" "0" \
    "$(printf '%s\n' "${block}" |
      grep -ci 'success without running the real check' || true)"

  # #6243 closure condition 2 — workflow level, outside the job block.
  assert_eq "duplicates-collapse: no edited carve-out in cancel-in-progress" "0" \
    "$(grep -E '^  cancel-in-progress:' "${wf}" |
      grep -c "github.event.action" || true)"
}

# --- single-file mode: check one workflow, no battery -----------------------

if [ -n "${SINGLE_WORKFLOW}" ]; then
  if [ ! -f "${SINGLE_WORKFLOW}" ]; then
    echo "check_version_bump_verdict_selftest: no such workflow: ${SINGLE_WORKFLOW}" >&2
    exit 1
  fi
  check_workflow "${SINGLE_WORKFLOW}"
  echo
  if [ "${FAILURES}" -gt 0 ]; then
    echo "check_version_bump_verdict_selftest: ${FAILURES}/${CASES} case(s) FAILED."
    exit 1
  fi
  echo "check_version_bump_verdict_selftest: ${CASES}/${CASES} cases passed."
  exit 0
fi

# --- default mode: the live workflow, then the mutation battery -------------

check_workflow "${WORKFLOW}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/vbverdict.XXXXXX")"
trap 'rm -rf "${TMP}"' EXIT

# mutate_narrowed_step_if — append the #6241 narrowing clause to the real check
# step's own `if:`. This is the mutation the pre-fix substring match accepted.
awk '
  /^      - name: Check PR version bumps against crates\.io$/ { arm = 1 }
  arm && /^        if: / {
    print $0 " && github.event.action != '\''edited'\''"
    arm = 0
    next
  }
  { print }
' "${WORKFLOW}" > "${TMP}/narrowed-step-if.yml"

# mutate_job_level_if — the same clause hoisted to the job header, where no
# step-scoped assertion can see it.
awk -v job="  pr-version-bump:" '
  { print }
  $0 == job {
    print "    if: github.event_name == '\''pull_request'\'' && github.event.action != '\''edited'\''"
  }
' "${WORKFLOW}" > "${TMP}/job-level-if.yml"

# mutate_no_real_gate — delete the invocation the whole job exists to make.
grep -v 'bash scripts/check-pr-version-bump\.sh' "${WORKFLOW}" \
  > "${TMP}/no-real-gate.yml"

echo
echo "mutation battery (each mutant must be REJECTED):"
for mutant in narrowed-step-if job-level-if no-real-gate; do
  # Sanity floor: a mutation that did not change the file proves nothing.
  if cmp -s "${WORKFLOW}" "${TMP}/${mutant}.yml"; then
    CASES=$((CASES + 1))
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: mutant/${mutant}: mutation produced an unchanged file"
    continue
  fi
  bash "${SELF}" --workflow "${TMP}/${mutant}.yml" > "${TMP}/${mutant}.out" 2>&1
  rc=$?
  assert_eq "mutant/${mutant}: rejected" "1" "${rc}"
  if [ "${rc}" -ne 1 ]; then
    echo "         checker output for the accepted mutant:"
    sed 's/^/         | /' "${TMP}/${mutant}.out"
  fi
done

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check_version_bump_verdict_selftest: ${FAILURES}/${CASES} case(s) FAILED."
  exit 1
fi
echo "check_version_bump_verdict_selftest: ${CASES}/${CASES} cases passed."

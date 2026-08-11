#!/usr/bin/env bash
#
# detect-pointer-lint-inputs.sh — does a change set touch anything the
# doc-comment pointer lint reads? (issue #5311)
#
# Why: `.github/workflows/test-pointers.yml` used a `paths:` trigger filter to
#   stay off PRs that cannot change its verdict. GitHub never creates the check
#   runs for a workflow its filters skipped, so the moment
#   `Doc-comment pointer lint (Why/What/Test)` becomes a REQUIRED
#   branch-protection context, every PR that does not match those globs goes
#   permanently pending and cannot merge. Proven on #5415/#5416 for the sibling
#   `Version parity` workflow: zero runs against either head SHA. The filter
#   therefore moves OFF the trigger and INTO the job, where the same question is
#   asked and the job still reports. Same remedy, same reasoning, as
#   scripts/detect-docs-only.sh's header.
#
# What: reads a newline-separated list of changed paths on stdin (or resolves
#   one itself from git when given a base ref) and answers ONE question: could
#   any changed path change what `scripts/check_test_pointers.sh` reports?
#   Emits `pointer_inputs_changed=true|false` on stdout, and appends the same
#   line to $GITHUB_OUTPUT when that is set.
#
#   The lint reads exactly four kinds of input, so those are the four rules:
#     **/*.rs                              both the `/// Test:` pointers and the
#                                          `fn <name>(` definitions they cite
#     .test-pointer-allowlist.tsv          the grandfathered set, whose entries
#                                          FAIL once they stop dangling
#     scripts/check_test_pointers.sh       the gate itself
#     scripts/check_scan_floor_selftest.sh the gate's scan-floor proof
#     .github/workflows/test-pointers.yml  the wiring that runs both
#     scripts/detect-pointer-lint-inputs.sh  this classifier — a change to the
#                                          decision must be checked by the gate
#                                          it decides for
#
#   DO NOT substitute scripts/detect-docs-only.sh for this. That helper answers
#   "is this Cargo-inert", and it deliberately classifies
#   `.test-pointer-allowlist.tsv`, `scripts/check_test_pointers.sh` and
#   `.github/workflows/test-pointers.yml` as INERT — true for a cargo build, and
#   exactly wrong here, since a PR that only removes an allowlist entry is the
#   PR this gate most needs to run on.
#
#   An EMPTY change set is relevant, not irrelevant: it means the diff could not
#   be resolved, and a skip must never be the consequence of a lookup failure.
#   Anything unrecognised is likewise treated as irrelevant only because it
#   matched none of the rules above, which are the lint's complete input set.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD | scripts/detect-pointer-lint-inputs.sh
#   POINTER_LINT_BASE=origin/main scripts/detect-pointer-lint-inputs.sh
#
# Exit: 0 on a successful classification; 2 when a requested base ref cannot be
#   resolved (fail closed — the caller must not guess).
#
# Test: scripts/check-ci-helpers-selftest.sh (`detect-pointer-lint-inputs:`
#   cases) runs this against Rust, allowlist, gate-script, unrelated, mixed and
#   empty change sets and asserts the emitted verdict for each.

set -euo pipefail

# is_lint_input <path> — true when the path can change the lint's verdict.
is_lint_input() {
  case "$1" in
    *.rs) return 0 ;;
    .test-pointer-allowlist.tsv) return 0 ;;
    scripts/check_test_pointers.sh) return 0 ;;
    scripts/check_scan_floor_selftest.sh) return 0 ;;
    scripts/detect-pointer-lint-inputs.sh) return 0 ;;
    .github/workflows/test-pointers.yml) return 0 ;;
  esac
  return 1
}

main() {
  local input
  if [ -n "${POINTER_LINT_BASE:-}" ]; then
    local merge_base
    if ! merge_base="$(git merge-base "${POINTER_LINT_BASE}" HEAD 2>/dev/null)"; then
      echo "detect-pointer-lint-inputs: cannot resolve merge-base against '${POINTER_LINT_BASE}'" >&2
      return 2
    fi
    input="$(git diff --name-only --no-renames "${merge_base}" HEAD)"
  else
    input="$(cat)"
  fi

  local changed=false
  local count=0
  local path
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    count=$((count + 1))
    if is_lint_input "$path"; then
      echo "  lint input: ${path}" >&2
      changed=true
    fi
  done <<<"$input"

  if [ "$count" -eq 0 ]; then
    echo "detect-pointer-lint-inputs: empty change set — treating as relevant (fail closed)" >&2
    changed=true
  fi

  echo "detect-pointer-lint-inputs: ${count} changed path(s) -> pointer_inputs_changed=${changed}" >&2
  echo "pointer_inputs_changed=${changed}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "pointer_inputs_changed=${changed}" >>"${GITHUB_OUTPUT}"
  fi
}

main "$@"

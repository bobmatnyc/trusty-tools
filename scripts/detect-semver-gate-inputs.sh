#!/usr/bin/env bash
#
# detect-semver-gate-inputs.sh — does a change set touch the SemVer gate's own
# machinery? (issue #5501)
#
# Why: `.github/workflows/semver-checks.yml` ran its two self-tests only when
#   `scripts/detect-version-bumps.sh` found a declared version bump. A PR that
#   repairs the gate and bumps nothing — which is what every gate-fix PR looks
#   like — therefore took the no-op path, and the self-tests that exist to catch
#   a broken gate were SKIPPED on exactly the PRs that change it. Observed on
#   PR #5496: it fixed this gate, `Public API / SemVer` passed in ~15s through
#   the no-op path, and the repair's own regression tests never ran in CI.
#
#   Keying the self-tests on the bump was never the right predicate for them.
#   The bump predicate belongs to the EXPENSIVE half of the gate — installing
#   the pinned `cargo-semver-checks` and warming a cold `target/semver-checks`
#   cache is the 20+ minutes #5149 refused to pay on every PR, and this script
#   must not restore that. The self-tests cost seconds: they replay committed
#   fixtures and stub the tool. So they get their own, second trigger, ORed with
#   the bump — "did this branch change the gate?" — and the expensive steps keep
#   the bump predicate untouched.
#
# What: reads a newline-separated list of changed paths on stdin (or resolves
#   one itself from git when given a base ref) and answers ONE question: could
#   any changed path change what the SemVer gate's self-tests report? Emits
#   `semver_gate_inputs_changed=true|false` on stdout, and appends the same line
#   to $GITHUB_OUTPUT when that is set.
#
#   The rules below are the gate's machinery as the workflow actually wires it,
#   read out of the file rather than guessed:
#     .github/workflows/semver-checks.yml       the wiring that runs all of it
#     scripts/detect-version-bumps.sh           crate selection (workflow L145)
#     scripts/check_semver_selftest.sh          run at workflow L239
#     scripts/check_semver_types_selftest.sh    run at workflow L255
#     scripts/check_semver.sh                   run at workflow L277, and the
#                                               subject of check_semver_selftest
#     scripts/check_semver_types.sh             the subject of the type-differ
#                                               self-test
#     scripts/lib/build_accel.sh                the build-acceleration resolver
#                                               check_semver.sh and
#                                               check_semver_types.sh both source
#     scripts/build_accel_selftest.sh           its self-test, run at workflow L292
#     scripts/lib/rustdoc_walk.py               the walk check_semver_types.sh
#                                               imports (ADR-0047)
#     scripts/semver-checks-*-exclusions.tsv    the crate and feature exclusion
#                                               tables check_semver.sh reads
#     scripts/test-data/semver-gate/**          the captured cargo-semver-checks
#     scripts/test-data/semver-types/**         output and rustdoc JSON the two
#                                               self-tests replay — a changed
#                                               fixture changes what they prove
#     scripts/detect-semver-gate-inputs.sh      this classifier — a change to
#                                               the decision must be checked by
#                                               the gate it decides for
#
#   `scripts/preflight-publish.sh` is deliberately NOT here. It runs the same
#   check_semver.sh at publish time and has its own self-test
#   (scripts/preflight-check5-selftest.sh); nothing this workflow runs reads it.
#
#   An EMPTY change set is relevant, not irrelevant: it means the diff could not
#   be resolved, and a self-test must never stand down as the consequence of a
#   lookup failure. Same fail-closed direction as
#   scripts/detect-pointer-lint-inputs.sh, whose shape this follows.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD | scripts/detect-semver-gate-inputs.sh
#   SEMVER_GATE_BASE=origin/main scripts/detect-semver-gate-inputs.sh
#
# Exit: 0 on a successful classification; 2 when a requested base ref cannot be
#   resolved (fail closed — the caller must not guess).
#
# Test: scripts/check-ci-helpers-selftest.sh (`detect-semver-gate-inputs:`
#   cases) runs this against each machinery path, unrelated paths, mixed and
#   empty change sets and asserts the emitted verdict for each.

set -euo pipefail

# is_gate_input <path> — true when the path can change what the SemVer gate's
# self-tests report.
is_gate_input() {
  case "$1" in
    .github/workflows/semver-checks.yml) return 0 ;;
    scripts/check_semver.sh) return 0 ;;
    scripts/check_semver_selftest.sh) return 0 ;;
    scripts/check_semver_types.sh) return 0 ;;
    scripts/check_semver_types_selftest.sh) return 0 ;;
    scripts/detect-version-bumps.sh) return 0 ;;
    scripts/detect-semver-gate-inputs.sh) return 0 ;;
    scripts/lib/rustdoc_walk.py) return 0 ;;
    scripts/lib/build_accel.sh) return 0 ;;
    scripts/build_accel_selftest.sh) return 0 ;;
    scripts/semver-checks-crate-exclusions.tsv) return 0 ;;
    scripts/semver-checks-feature-exclusions.tsv) return 0 ;;
    scripts/test-data/semver-gate/*) return 0 ;;
    scripts/test-data/semver-types/*) return 0 ;;
  esac
  return 1
}

main() {
  local input
  if [ -n "${SEMVER_GATE_BASE:-}" ]; then
    local merge_base
    if ! merge_base="$(git merge-base "${SEMVER_GATE_BASE}" HEAD 2>/dev/null)"; then
      echo "detect-semver-gate-inputs: cannot resolve merge-base against '${SEMVER_GATE_BASE}'" >&2
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
    if is_gate_input "$path"; then
      echo "  gate machinery: ${path}" >&2
      changed=true
    fi
  done <<<"$input"

  if [ "$count" -eq 0 ]; then
    echo "detect-semver-gate-inputs: empty change set — treating as relevant (fail closed)" >&2
    changed=true
  fi

  echo "detect-semver-gate-inputs: ${count} changed path(s) -> semver_gate_inputs_changed=${changed}" >&2
  echo "semver_gate_inputs_changed=${changed}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "semver_gate_inputs_changed=${changed}" >>"${GITHUB_OUTPUT}"
  fi
}

main "$@"

#!/usr/bin/env bash
#
# rustdoc-links-scope-selftest.sh — fixtures for scripts/rustdoc-links-scope.sh
#   and for the two workflow lines that consume it (#8716).
#
# Why: the scope decides whether a release candidate is documented at all. The
#   case that matters is the changelog-only release PR (#8681's shape): its diff
#   is Cargo-inert, so without the release arm it answers `skip` and a broken
#   link in a published crate reaches the publish boundary behind a green run.
#   The `skip` and `ordinary` cases matter as much: a scope that always says
#   `release` would widen every per-PR run, which #8716 rules out.
#
# What: drives the script with synthetic event inputs and asserts each scope,
#   then asserts the wiring: ci.yml's rustdoc-links job computes the scope with
#   this script and passes `--require-published` on the release arm, and
#   pre-publish.yml's Gate 1 always passes it.
#
# Usage: bash scripts/rustdoc-links-scope-selftest.sh
# Exit: 0 when every case matches; 1 otherwise, naming each mismatch.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# SCOPE_SCRIPT points the cases at a mutant copy, to show they can fail.
SCRIPT="${SCOPE_SCRIPT:-${REPO_ROOT}/scripts/rustdoc-links-scope.sh}"
CI="${REPO_ROOT}/.github/workflows/ci.yml"
PREPUB="${REPO_ROOT}/.github/workflows/pre-publish.yml"

FAILURES=0
CASES=0

assert_eq() {   # <label> <expected> <actual>
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-58s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# scope <event> <docs_only> <pr_title> <pr_head_ref> <head_commit_msg>
scope() {
  env -u GITHUB_OUTPUT EVENT_NAME="$1" DOCS_ONLY="$2" PR_TITLE="$3" \
    PR_HEAD_REF="$4" HEAD_COMMIT_MSG="$5" bash "$SCRIPT" 2>/dev/null
}

echo "rustdoc-links-scope: pull_request"
# The #8716 regression: a changelog-only release PR is Cargo-inert.
assert_eq "changelog-only release PR (#8681 shape)" release \
  "$(scope pull_request true 'chore(release): assemble trusty-mpm 1.7.7 changelog' release/trusty-mpm-1.7.7 '')"
assert_eq "chore(<crate>): release title" release \
  "$(scope pull_request false 'chore(trusty-memory): release 0.28.0' feature-x '')"
assert_eq "chore/release- branch, plain title" release \
  "$(scope pull_request true 'assemble notes' chore/release-tga-8.0.0 '')"
assert_eq "release/ branch, plain title" release \
  "$(scope pull_request true 'assemble notes' release/trusty-mpm-1.7.8 '')"
# Ordinary PRs keep their old behaviour.
assert_eq "docs-only ordinary PR skips" skip \
  "$(scope pull_request true 'docs: fix a typo' docs/typo '')"
assert_eq "code PR runs the ordinary scope" ordinary \
  "$(scope pull_request false 'fix(trusty-mpm): a bug' fix/bug '')"
assert_eq "docs(release) is not a release candidate" skip \
  "$(scope pull_request true 'docs(release): close the milestone' docs/release-rule '')"
assert_eq "'release' later in a chore title is not one" skip \
  "$(scope pull_request true 'chore: note the release date' chore/note '')"
assert_eq "branch merely containing release/ is not one" skip \
  "$(scope pull_request true 'docs: x' fix/release/notes '')"
assert_eq "empty docs_only (changes job failed) runs" ordinary \
  "$(scope pull_request '' 'docs: x' docs/x '')"

echo "rustdoc-links-scope: push"
assert_eq "squash-merged release commit" release \
  "$(scope push true '' '' 'chore(release): assemble trusty-mpm 1.7.7 changelog (#8681)')"
assert_eq "release subject, body below" release \
  "$(scope push true '' '' "$(printf 'chore(trusty-mpm): release 1.7.8 (#1)\n\nbody text')")"
assert_eq "release words only in the body do not count" skip \
  "$(scope push true '' '' "$(printf 'docs: x (#2)\n\nchore(release): y')")"
assert_eq "docs-only ordinary push skips" skip \
  "$(scope push true '' '' 'docs: x (#3)')"
assert_eq "code push runs the ordinary scope" ordinary \
  "$(scope push false '' '' 'fix: y (#4)')"

echo "rustdoc-links-scope: other events"
assert_eq "workflow_dispatch" release "$(scope workflow_dispatch false '' '' '')"
assert_eq "unknown event fails closed" release "$(scope merge_group true '' '' '')"

echo "rustdoc-links-scope: GITHUB_OUTPUT"
out="$(mktemp)"
EVENT_NAME=pull_request DOCS_ONLY=true PR_TITLE='chore(release): x' \
  GITHUB_OUTPUT="$out" bash "$SCRIPT" >/dev/null 2>&1
assert_eq "writes scope=release" "scope=release" "$(cat "$out")"
rm -f "$out"

echo "rustdoc-links-scope: workflow wiring"
# ci.yml: the job computes the scope here and the release arm widens the gate.
ci_job="$(awk '/^  rustdoc-links:$/ { on = 1; next } on && /^  [a-z0-9-]+:$/ { exit } on' "$CI")"
has() { if printf '%s\n' "$1" | grep -qF -- "$2"; then echo yes; else echo no; fi; }
assert_eq "ci.yml rustdoc-links runs the scope script" yes \
  "$(has "$ci_job" 'bash scripts/rustdoc-links-scope.sh')"
assert_eq "ci.yml rustdoc-links passes --require-published" yes \
  "$(has "$ci_job" 'check_rustdoc_links.sh --require-published')"
assert_eq "ci.yml rustdoc-links no longer gates on docs_only" no \
  "$(has "$ci_job" "if: needs.changes.outputs.docs_only != 'true'")"
assert_eq "pre-publish.yml Gate 1 passes --require-published" yes \
  "$(has "$(cat "$PREPUB")" 'run: bash scripts/check_rustdoc_links.sh --require-published')"

echo
if [ "$FAILURES" -ne 0 ]; then
  echo "rustdoc-links-scope-selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "rustdoc-links-scope-selftest: all ${CASES} case(s) passed"

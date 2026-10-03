#!/usr/bin/env bash
#
# rustdoc-links-scope.sh — decide what ci.yml's `rustdoc-links` job checks for
#   one event (#8716).
#
# Why: the job skips its doc build when the `changes` job calls the diff
#   Cargo-inert (`docs_only=true`). A release PR that only assembles changelogs
#   touches nothing but CHANGELOG.md and changelog.d/, so it skipped too, and
#   main could carry broken links in a published crate behind a green release
#   commit. That red blocked the trusty-mpm 1.7.8 publish. A release candidate
#   is the one commit where "this diff touched no Rust" is the wrong question.
#
# What: prints one scope on stdout.
#     release   a release candidate. The job always runs, and it runs the gate
#               with `--require-published`, so every crate `cargo metadata`
#               calls publishable must be documented.
#     ordinary  any other change set the `changes` job did not call Cargo-inert.
#               The job runs the gate as before.
#     skip      a Cargo-inert change set that is not a release candidate. The
#               job skips its doc build, as before.
#   A release candidate is recognised the way this repo names release work:
#     - a pull_request whose title starts `chore(release)` or reads
#       `chore(<crate>): release …`, or whose head branch starts
#       `chore/release-` or `release/`;
#     - a push whose head commit subject has that title shape (a squash merge
#       keeps the PR title as the subject);
#     - a workflow_dispatch, the manual run used to check a branch before a cut.
#   A push that bumps a crate version changes a Cargo.toml, so the `changes`
#   job already calls it Cargo-relevant and it gets `ordinary` or `release`.
#
#   FAIL CLOSED: an event this script does not know answers `release`, the
#   widest scope. An empty DOCS_ONLY (the `changes` job failed) answers
#   `ordinary`, matching the job's former `docs_only != 'true'` test.
#
# Inputs (environment; titles and branch names are untrusted, so the workflow
#   passes them here as env, never inside a `run:` script):
#   EVENT_NAME         github.event_name
#   DOCS_ONLY          needs.changes.outputs.docs_only
#   PR_TITLE           github.event.pull_request.title
#   PR_HEAD_REF        github.event.pull_request.head.ref
#   HEAD_COMMIT_MSG    github.event.head_commit.message (push only)
#
# Output: the scope on stdout. When $GITHUB_OUTPUT is set, also `scope=<scope>`.
#
# Test: scripts/rustdoc-links-scope-selftest.sh

set -euo pipefail

emit() {
  echo "$1"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf 'scope=%s\n' "$1" >>"${GITHUB_OUTPUT}"
  fi
  echo "rustdoc-links-scope: ${EVENT_NAME:-<unset>} -> $1 ($2)" >&2
}

# is_release_title <subject>
is_release_title() {
  printf '%s\n' "$1" | grep -qE '^chore\(release\)!?:|^chore\([A-Za-z0-9_.-]+\)!?: release '
}

event="${EVENT_NAME:-}"
docs_only="${DOCS_ONLY:-}"

case "$event" in
  pull_request)
    if is_release_title "${PR_TITLE:-}"; then
      emit release "release PR title"
      exit 0
    fi
    case "${PR_HEAD_REF:-}" in
      chore/release-* | release/*)
        emit release "release PR branch"
        exit 0
        ;;
    esac
    ;;
  push)
    # The subject is the first line of the commit message. Parameter
    # expansion, NOT `printf … | head -n 1`: under `set -euo pipefail` `head`
    # exits after its first line and printf's later write (a multi-line
    # message) can then take SIGPIPE (#8716).
    msg="${HEAD_COMMIT_MSG:-}"
    subject="${msg%%$'\n'*}"
    if is_release_title "$subject"; then
      emit release "release commit subject"
      exit 0
    fi
    ;;
  workflow_dispatch)
    emit release "manual dispatch"
    exit 0
    ;;
  *)
    emit release "unknown event, fail closed"
    exit 0
    ;;
esac

if [ "$docs_only" = "true" ]; then
  emit skip "Cargo-inert change set"
else
  emit ordinary "docs_only=${docs_only:-<empty>}"
fi

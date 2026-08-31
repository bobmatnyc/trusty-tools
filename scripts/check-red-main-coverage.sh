#!/usr/bin/env bash
#
# check-red-main-coverage.sh — every push-to-main workflow must have a red-main
# notifier watching it (issue #5657).
#
# Why: `ci.yml`'s `notify-main-failure` cannot see a job in another workflow
#   file, so a sibling with its own `push: branches: [main]` trigger goes red on
#   main and nothing files or comments on the `ci-red-main` tracking issue.
#   `test-pointers.yml` did that for 24 hours; `capabilities-drift.yml` did it
#   again on 2026-08-31. Both were patched one file at a time, and the audit
#   table written to catch the rest was already missing four workflows the day
#   it was written. A hand-maintained list is the failure mode, not the fix, so
#   the list is machine-checked here instead.
#
# What: reads every `.github/workflows/*.yml`, decides which ones trigger on a
#   push to `main`, and asserts each of those is covered exactly once — either
#   listed in `red-main-notify.yml`'s `workflow_run.workflows:` list, or one of
#   the two workflows that notify themselves (`ci.yml`, `version-parity.yml`)
#   for reasons `red-main-notify.yml`'s header states. Three ways to fail:
#     1. a push-to-main workflow nobody watches (the #5657 bug, recurring);
#     2. a listed name that matches no workflow file — `workflow_run` matches on
#        NAME, so renaming a workflow silently drops its coverage;
#     3. a self-notifying workflow that lost its own notify job.
#   Fails CLOSED: an `on:` block it cannot parse counts as push-to-main and must
#   be covered, because a false alarm is noise and a false pass is the bug.
#
#   `--detect <file>` prints the two facts it derives from one workflow file
#   (`name=` and `push_main=`) and exits. That mode exists so the decision is
#   fixture-testable without a repo full of real workflows.
#
# Usage:
#   bash scripts/check-red-main-coverage.sh            # audit the whole repo
#   bash scripts/check-red-main-coverage.sh --detect .github/workflows/ci.yml
#
# Env: RED_MAIN_WORKFLOWS_DIR overrides the directory scanned (fixtures only).
#
# Exit: 0 when every push-to-main workflow is covered; 1 on the first gap, with
#   the offending file and the exact edit that closes it.
#
# Test: scripts/check-ci-helpers-selftest.sh (`check-red-main-coverage:` cases)
#   drives `--detect` over fixture workflows — bare push, `branches: [main]`,
#   tags-only, pull-request-only, a multi-line branch list, an unparseable `on:`
#   block — and asserts the live repo audit passes.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOWS_DIR="${RED_MAIN_WORKFLOWS_DIR:-${REPO_ROOT}/.github/workflows}"
NOTIFIER="${WORKFLOWS_DIR}/red-main-notify.yml"

# Workflows that carry their own red-main notifier and are therefore NOT listed
# in red-main-notify.yml — listing them would file twice for one failure.
# `<file>|<job key that must still be present>|<why>`.
SELF_NOTIFIED=(
  "ci.yml|notify-main-failure:|classifies the shard matrix directly (#5998), which an aggregate run conclusion cannot"
  "version-parity.yml|notify-main-drift:|files the version-drift-main label and also fires on the schedule trigger (#4688)"
)

# ---------------------------------------------------------------------------
# workflow_name <file> — the workflow's `name:` value, unquoted. Empty when the
# file declares none (GitHub then falls back to the path, which `workflow_run`
# cannot match reliably, so the audit treats that as uncoverable).
# ---------------------------------------------------------------------------
workflow_name() {
  sed -n 's/^name:[[:space:]]*//p' "$1" | head -1 |
    sed -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"
}

# ---------------------------------------------------------------------------
# push_main <file> — `true` when a push to `main` triggers this workflow.
#
# A `push:` block covers main when it names `main` (or a wildcard) under
# `branches:`, or when it constrains nothing at all. A `tags:`-only push block
# fires on tag refs and never on a branch, so it is not push-to-main — that is
# how release.yml, pre-publish.yml and semver-checks.yml stay out of the audit
# without an exclusion list.
# ---------------------------------------------------------------------------
push_main() {
  awk '
    # Top-level "on:" opens the trigger block; the next top-level key closes it.
    # Anything after the colon is the inline form (`on: [push]`), which this
    # parser does not read — it is reported as push-to-main so the audit forces
    # a human answer rather than assuming a safe one.
    /^on:/ {
      saw_on = 1
      rest = $0
      sub(/^on:[[:space:]]*/, "", rest)
      if (rest != "" && rest !~ /^#/) inline = 1
      in_on = 1
      next
    }
    in_on && /^[^ \t#]/ { in_on = 0 }
    !in_on            { next }

    # Two-space keys inside "on:". Only "push:" matters.
    /^  push:/        { in_push = 1; saw_push = 1; next }
    in_push && /^  [^ \t]/ { in_push = 0 }
    !in_push          { next }

    # Four-space keys inside "push:".
    /^    branches:/  { saw_branches = 1; in_list = "branches"; rest = $0; sub(/^[^:]*:/, "", rest); if (rest ~ /main|\*/) hit = 1; next }
    /^    tags:/      { saw_tags = 1; in_list = "tags"; next }
    /^    [^ \t]/     { in_list = ""; next }

    # Deeper lines belong to whichever list is open.
    in_list == "branches" && /^ *- / { if ($0 ~ /main|\*/) hit = 1 }

    END {
      # No "on:" block, or one this parser cannot read, is a parse failure, not
      # evidence of no push trigger — fail closed.
      if (!saw_on || inline) { print "true"; exit }
      if (!saw_push)      { print "false"; exit }
      if (saw_branches)   { print (hit ? "true" : "false"); exit }
      if (saw_tags)       { print "false"; exit }
      # A bare "push:" constrains nothing, so it includes main. So does anything
      # else this parser did not recognise — `branches-ignore:` included.
      print "true"
    }
  ' "$1"
}

# ---------------------------------------------------------------------------
# --detect <file>: print the two derived facts and stop.
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--detect" ]; then
  file="${2:-}"
  if [ ! -f "${file}" ]; then
    echo "check-red-main-coverage: no such workflow file: ${file}" >&2
    exit 1
  fi
  echo "name=$(workflow_name "${file}")"
  echo "push_main=$(push_main "${file}")"
  exit 0
fi

# ---------------------------------------------------------------------------
# Full audit.
# ---------------------------------------------------------------------------
FAILURES=0

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $*" >&2
}

if [ ! -f "${NOTIFIER}" ]; then
  echo "check-red-main-coverage: notifier missing: ${NOTIFIER}" >&2
  echo "  Every push-to-main workflow depends on it (#5657). Restore it." >&2
  exit 1
fi

# The names red-main-notify.yml watches: the block-list entries under
# `workflows:`, up to the next key at the same indent (`types:`).
WATCHED="$(awk '
  /^    workflows:/ { in_list = 1; next }
  in_list && /^    [^ \t]/ { in_list = 0 }
  in_list && /^ *- / {
    line = $0
    sub(/^ *- /, "", line)
    sub(/[[:space:]]*#.*$/, "", line)
    sub(/[[:space:]]*$/, "", line)
    gsub(/^"|"$/, "", line)
    gsub(/^'"'"'|'"'"'$/, "", line)
    print line
  }
' "${NOTIFIER}")"

if [ -z "${WATCHED}" ]; then
  echo "check-red-main-coverage: ${NOTIFIER} watches nothing — parse failed or the list is empty." >&2
  exit 1
fi

echo "check-red-main-coverage: scanning ${WORKFLOWS_DIR}"

# --- 1. Every push-to-main workflow is covered. ----------------------------
covered=0
for wf in "${WORKFLOWS_DIR}"/*.yml; do
  [ -f "${wf}" ] || continue
  base="$(basename "${wf}")"
  [ "${base}" = "red-main-notify.yml" ] && continue

  [ "$(push_main "${wf}")" = "true" ] || continue

  self_notified=""
  for entry in "${SELF_NOTIFIED[@]}"; do
    [ "${entry%%|*}" = "${base}" ] && self_notified="${entry}"
  done

  if [ -n "${self_notified}" ]; then
    marker="$(echo "${self_notified}" | cut -d'|' -f2)"
    if grep -q "^  ${marker}$" "${wf}"; then
      echo "  ok   ${base} — self-notified (${marker%:})"
      covered=$((covered + 1))
    else
      fail "${base} is registered as self-notifying but has no '${marker%:}' job."
      echo "       Either restore that job or add its name to ${NOTIFIER}." >&2
    fi
    continue
  fi

  name="$(workflow_name "${wf}")"
  if [ -z "${name}" ]; then
    fail "${base} triggers on push to main but declares no 'name:'."
    echo "       workflow_run matches on name, so it cannot be watched without one." >&2
    continue
  fi

  if grep -Fxq "${name}" <<<"${WATCHED}"; then
    echo "  ok   ${base} — watched as \"${name}\""
    covered=$((covered + 1))
  else
    fail "${base} triggers on push to main and NOTHING watches it (#5657)."
    echo "       Add \"${name}\" to the workflows: list in ${NOTIFIER}." >&2
  fi
done

# --- 2. Every watched name still resolves to a push-to-main workflow. ------
while IFS= read -r name; do
  [ -n "${name}" ] || continue
  match=""
  for wf in "${WORKFLOWS_DIR}"/*.yml; do
    [ -f "${wf}" ] || continue
    [ "$(workflow_name "${wf}")" = "${name}" ] && match="${wf}"
  done
  if [ -z "${match}" ]; then
    fail "${NOTIFIER} watches \"${name}\", which no workflow declares."
    echo "       A renamed workflow silently loses coverage — update the list." >&2
  elif [ "$(push_main "${match}")" != "true" ]; then
    fail "${NOTIFIER} watches \"${name}\" ($(basename "${match}")), which no longer triggers on push to main."
    echo "       Drop it from the list, or restore its push: branches: [main] trigger." >&2
  fi
done <<<"${WATCHED}"

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check-red-main-coverage: ${FAILURES} coverage gap(s) — a red main would go unreported." >&2
  exit 1
fi
echo "check-red-main-coverage: ${covered} push-to-main workflow(s), all covered"

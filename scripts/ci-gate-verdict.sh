#!/usr/bin/env bash
#
# ci-gate-verdict.sh — the `CI gate` job's verdict over every context it stands
#   in for (#8378).
#
# Why: branch protection listed twenty required contexts, most of them jobs a
#   docs or instruction-content PR only starts in order to skip. The owner
#   approved a six-item list — CI gate, line-cap, generation-artifact-lint,
#   test-pointers, PR version bump, capabilities-drift — which is safe only if
#   `CI gate` fails whenever any of the other fourteen would have. This script
#   is that promise, stated once.
#
# What: reads `needs` as JSON ($NEEDS_JSON, the job's `toJSON(needs)`) and
#   checks each row of CONTEXTS below:
#     - `changes` (the classifier) must be `success`. A classifier that failed
#       still makes every job run in full, but the gate goes red so a broken
#       classifier is never mistaken for a clean one.
#     - each context's job must be `success`, or `skipped` when — and only
#       when — the row names a relevance output and `changes` set it to
#       `false`. A skip the classifier did not ask for is a failure.
#     - a context whose job key is absent from `needs` is a failure: the
#       workflow and this table have drifted apart.
#   Prints one table row per context and, when $GITHUB_STEP_SUMMARY is set,
#   writes the same table plus the classifier's verdicts there.
#
# Env:
#   NEEDS_JSON             required; `${{ toJSON(needs) }}`
#   GITHUB_STEP_SUMMARY    optional; the Markdown summary file
#
# Exit: 0 when every context passes; 1 when any fails; 2 on unusable input.
#
# Test: scripts/ci-gate-selftest.sh (`ci-gate-verdict:` cases, and the
#   `ci.yml wiring:` cases that hold this table and the job's `needs:` equal).

set -uo pipefail

# `<job key>|<context name>|<relevance output that may skip it, or ->`.
# The ONLY list of what `CI gate` covers. ci-gate-selftest.sh asserts it equals
# the ci-gate job's `needs:` (minus `changes`) and each job's `name:`.
CONTEXTS="
fmt|Format check|-
clippy|Clippy|-
msrv|MSRV check|-
affected|Rust tests (affected crates)|-
trusty-common-lanes|trusty-common coverage lanes|-
agents-ui|trusty-agents-ui clippy|-
code-gui|trusty-code-gui clippy|-
search-daemon-smoke|trusty-search daemon smoke test|-
rustdoc-links|Rustdoc intra-doc links|-
teardown-guard|Durable writes hold the teardown guard|teardown_guard_relevant
tmux-exact-targets|Every tmux -t target is exact|tmux_targets_relevant
parity-selftest|Parity gate self-test (must catch a drifted tag)|-
check5-decision|CHECK 5 decision self-test (0 compared must never print PASS)|-
check8-decision|CHECK 8 decision self-test (a run's head_sha is not what it gated)|-
website-corpus|Website content corpus|-
"

if [ "${1:-}" = "--list" ]; then
  printf '%s\n' "$CONTEXTS" | sed '/^$/d'
  exit 0
fi

if [ -z "${NEEDS_JSON:-}" ]; then
  echo "ci-gate-verdict: NEEDS_JSON is unset" >&2
  exit 2
fi
if ! printf '%s' "$NEEDS_JSON" | jq -e 'type == "object"' >/dev/null 2>&1; then
  echo "ci-gate-verdict: NEEDS_JSON is not a JSON object" >&2
  exit 2
fi

# need <jq path> — one string out of NEEDS_JSON, empty when absent.
need() {
  printf '%s' "$NEEDS_JSON" | jq -r "$1 // empty"
}

failures=0
rows=""

row() {
  rows="${rows}| $1 | $2 | $3 |
"
  printf '%-4s %-70s %s\n' "$3" "$1" "$2"
}

changes_result="$(need '.changes.result')"
if [ "$changes_result" = "success" ]; then
  row "Detect Cargo-inert change set" "success" "ok"
else
  row "Detect Cargo-inert change set" "${changes_result:-absent}" "FAIL"
  failures=$((failures + 1))
fi

while IFS='|' read -r key context relevance; do
  [ -n "$key" ] || continue
  result="$(need ".\"${key}\".result")"
  verdict="FAIL"
  note="${result:-absent from needs}"
  case "$result" in
    success) verdict="ok" ;;
    skipped)
      if [ "$relevance" != "-" ] &&
        [ "$(need ".changes.outputs.${relevance}")" = "false" ]; then
        verdict="ok"
        note="skipped: ${relevance}=false"
      else
        note="skipped, but the classifier did not ask for a skip"
      fi
      ;;
  esac
  [ "$verdict" = "ok" ] || failures=$((failures + 1))
  row "$context" "$note" "$verdict"
done <<EOF
$CONTEXTS
EOF

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  outputs="$(printf '%s' "$NEEDS_JSON" |
    jq -r '.changes.outputs // {} | to_entries | map(select(.key | test("_reason$") | not))
           | .[] | "| `\(.key)` | `\(.value)` |"')"
  {
    echo "## CI gate"
    echo
    if [ "$(need '.changes.outputs.docs_only')" = "true" ]; then
      echo "Classifier: **Cargo-inert** — clippy, fmt, MSRV, the Rust tests, the GUI clippies and the daemon smoke test report success without building."
    else
      echo "Classifier: **code** — every Cargo job runs in full."
    fi
    echo
    echo "| Classifier output | Value |"
    echo "|---|---|"
    printf '%s\n' "$outputs"
    echo
    echo "| Context | Result | Verdict |"
    echo "|---|---|---|"
    printf '%s' "$rows"
  } >>"$GITHUB_STEP_SUMMARY"
fi

if [ "$failures" -gt 0 ]; then
  echo "::error::CI gate: ${failures} context(s) did not pass — see the table above"
  exit 1
fi
echo "CI gate: every covered context passed or was skipped by the classifier"

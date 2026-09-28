#!/usr/bin/env bash
#
# ci-gate-selftest.sh — fixtures for the `CI gate` job's two scripts and its
#   ci.yml wiring (#8378).
#
# Why: `CI gate` is meant to become the one required context standing in for
#   fourteen others. Its failure modes are silent: a relevance rule that
#   answers `false` for a real input skips a gate, a verdict that accepts an
#   unordered skip reports green on nothing, and a `needs:` list that drifts
#   from the covered-context table drops a gate from the verdict entirely.
# What: asserts
#   ci-gate-relevance: each mode's inputs, non-inputs, self-inclusion and
#                      every fail-closed arm;
#   ci-gate-verdict:   pass, fail, ordered skip, unordered skip, failed
#                      classifier, missing need, unusable input;
#   ci.yml wiring:     the ci-gate job's `needs:` equals the verdict table,
#                      each job carries the context name the table claims,
#                      the job-level filters fail closed, the moved
#                      workflows are gone (no second check run per name),
#                      and trusty-common is tested only by its lanes job;
#   vmtest-harness:    its pull_request trigger carries a `paths:` filter.
# Usage: bash scripts/ci-gate-selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing each mismatch.
# Test: this IS the test. CI runs it in ci.yml's `changes` job.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}" || exit 1

FAILURES=0
CASES=0

assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-62s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# ---------------------------------------------------------------------------
echo "ci-gate-relevance:"
rel() { printf '%s' "$2" | env -u EVENT_NAME -u GITHUB_OUTPUT bash scripts/ci-gate-relevance.sh "$1" 2>/dev/null; }

assert_eq "teardown: trusty-search source"      "true"  "$(rel teardown-guard 'crates/trusty-search/src/service/reindex/semaphore.rs')"
assert_eq "teardown: its manifest"              "true"  "$(rel teardown-guard 'scripts/teardown-guard-manifest.tsv')"
assert_eq "teardown: its methods table"         "true"  "$(rel teardown-guard 'scripts/teardown-guard-methods.tsv')"
assert_eq "teardown: the awk it sources"        "true"  "$(rel teardown-guard 'scripts/lib/sloc_awk.sh')"
assert_eq "teardown: the hosting workflow"      "true"  "$(rel teardown-guard '.github/workflows/ci.yml')"
assert_eq "teardown: this classifier"           "true"  "$(rel teardown-guard 'scripts/ci-gate-relevance.sh')"
assert_eq "teardown: docs only"                 "false" "$(rel teardown-guard 'docs/a.md')"
assert_eq "teardown: another crate's source"    "false" "$(rel teardown-guard 'crates/trusty-mpm/src/lib.rs')"
assert_eq "teardown: trusty-search tests dir"   "false" "$(rel teardown-guard 'crates/trusty-search/tests/x.rs')"
assert_eq "tmux: rust source"                   "true"  "$(rel tmux-targets 'crates/trusty-mpm/src/lib.rs')"
assert_eq "tmux: shell script"                  "true"  "$(rel tmux-targets 'scripts/foo.sh')"
assert_eq "tmux: svelte"                        "true"  "$(rel tmux-targets 'website/src/routes/+page.svelte')"
assert_eq "tmux: instruction asset"             "true"  "$(rel tmux-targets 'crates/trusty-agents-common/src/assets/agents/BASE-AGENT.md')"
assert_eq "tmux: its allowlist"                 "true"  "$(rel tmux-targets 'scripts/tmux-exact-targets-allowlist.tsv')"
assert_eq "tmux: docs only"                     "false" "$(rel tmux-targets 'docs/a.md
README.md')"
assert_eq "tmux: node_modules is not scanned"   "false" "$(rel tmux-targets 'website/node_modules/x/y.js')"
assert_eq "tmux: website content"               "false" "$(rel tmux-targets 'website/src/content/tools/x.md')"
assert_eq "C-quoted path (fail closed)"         "true"  "$(rel teardown-guard '"crates/caf\303\251.rs"')"
assert_eq "empty change set (fail closed)"      "true"  "$(rel tmux-targets '')"
assert_eq "no usable base (fail closed)" "true" \
  "$(env -u GITHUB_OUTPUT EVENT_NAME=push PUSH_BEFORE='' bash scripts/ci-gate-relevance.sh teardown-guard 2>/dev/null)"
assert_eq "unresolvable base (fail closed)" "true" \
  "$(env -u GITHUB_OUTPUT EVENT_NAME=pull_request BASE_REF=no-such-branch-8378 bash scripts/ci-gate-relevance.sh tmux-targets 2>/dev/null)"
out="$(mktemp)"
printf 'docs/a.md' | env -u EVENT_NAME GITHUB_OUTPUT="$out" bash scripts/ci-gate-relevance.sh tmux-targets >/dev/null 2>&1
assert_eq "writes <mode>_relevant to GITHUB_OUTPUT" "tmux_targets_relevant=false" "$(cat "$out")"
rm -f "$out"
bash scripts/ci-gate-relevance.sh bogus </dev/null >/dev/null 2>&1
assert_eq "unknown mode is a usage error" "2" "$?"

# ---------------------------------------------------------------------------
echo
echo "ci-gate-verdict:"
# needs_json [<key>=<result> ...] [out:<name>=<value> ...] — every covered job
# `success` and `changes` `success` unless overridden.
needs_json() {
  local keys k v arg
  keys="$(bash scripts/ci-gate-verdict.sh --list | cut -d'|' -f1)"
  local json='{"changes":{"result":"success","outputs":{}}'
  for k in $keys; do json="${json},\"${k}\":{\"result\":\"success\",\"outputs\":{}}"; done
  json="${json}}"
  for arg in "$@"; do
    case "$arg" in
      out:*)
        k="${arg#out:}"; v="${k#*=}"; k="${k%%=*}"
        json="$(printf '%s' "$json" | jq -c --arg k "$k" --arg v "$v" '.changes.outputs[$k] = $v')" ;;
      drop:*)
        json="$(printf '%s' "$json" | jq -c --arg k "${arg#drop:}" 'del(.[$k])')" ;;
      *)
        k="${arg%%=*}"; v="${arg#*=}"
        json="$(printf '%s' "$json" | jq -c --arg k "$k" --arg v "$v" '.[$k].result = $v')" ;;
    esac
  done
  printf '%s' "$json"
}
# env -u: a fixture verdict must not append its table to the real job summary.
verdict() { env -u GITHUB_STEP_SUMMARY NEEDS_JSON="$(needs_json "$@")" bash scripts/ci-gate-verdict.sh >/dev/null 2>&1; echo "$?"; }

assert_eq "every context succeeded"                     "0" "$(verdict)"
assert_eq "clippy failed (probe 7)"                     "1" "$(verdict clippy=failure)"
assert_eq "affected legs cancelled"                     "1" "$(verdict affected=cancelled)"
assert_eq "trusty-common lanes failed"                  "1" "$(verdict trusty-common-lanes=failure)"
assert_eq "trusty-common lanes skipped (never allowed)" "1" "$(verdict trusty-common-lanes=skipped)"
assert_eq "classifier failed (probe 8, fail closed)"    "1" "$(verdict changes=failure)"
assert_eq "teardown skipped, classifier said false"     "0" "$(verdict teardown-guard=skipped out:teardown_guard_relevant=false)"
assert_eq "teardown skipped, classifier said true"      "1" "$(verdict teardown-guard=skipped out:teardown_guard_relevant=true)"
assert_eq "teardown skipped, classifier said nothing"   "1" "$(verdict teardown-guard=skipped)"
assert_eq "tmux skipped under the teardown verdict"     "1" "$(verdict tmux-exact-targets=skipped out:teardown_guard_relevant=false)"
assert_eq "an unfiltered job skipped (Clippy)"          "1" "$(verdict clippy=skipped out:docs_only=true)"
assert_eq "a covered job missing from needs"            "1" "$(verdict drop:website-corpus)"
assert_eq "NEEDS_JSON not an object"                    "2" \
  "$(env -u GITHUB_STEP_SUMMARY NEEDS_JSON='[]' bash scripts/ci-gate-verdict.sh >/dev/null 2>&1; echo "$?")"
assert_eq "NEEDS_JSON unset"                            "2" \
  "$(env -u NEEDS_JSON -u GITHUB_STEP_SUMMARY bash scripts/ci-gate-verdict.sh >/dev/null 2>&1; echo "$?")"
summary="$(mktemp)"
NEEDS_JSON="$(needs_json out:docs_only=true)" GITHUB_STEP_SUMMARY="$summary" \
  bash scripts/ci-gate-verdict.sh >/dev/null 2>&1
assert_eq "summary states the Cargo-inert verdict" "1" "$(grep -c 'Classifier: \*\*Cargo-inert\*\*' "$summary")"
assert_eq "summary lists every covered context" "$(bash scripts/ci-gate-verdict.sh --list | wc -l | tr -d ' ')" \
  "$(grep -cE '^\| .* \| (success|skipped.*) \| ok \|$' "$summary" | awk '{print $1 - 1}')"
rm -f "$summary"

# ---------------------------------------------------------------------------
echo
echo "ci.yml wiring:"
ci=".github/workflows/ci.yml"
gate_job="$(sed -n '/^  ci-gate:$/,/^  # ----/p' "$ci")"
needs_list="$(printf '%s\n' "$gate_job" | sed -n '/^    needs:$/,/^      \]$/p' |
  sed -n 's/^        \([a-z0-9-]*\),$/\1/p' | grep -v '^changes$' | sort)"
table_keys="$(bash scripts/ci-gate-verdict.sh --list | cut -d'|' -f1 | sort)"
assert_eq "ci-gate needs == the verdict's covered table" "$table_keys" "$needs_list"
assert_eq "ci-gate also needs the classifier" "1" \
  "$(printf '%s\n' "$gate_job" | grep -c '^        changes,$')"
# shellcheck disable=SC2016  # a literal `${{ }}` expression, matched as text
assert_eq "ci-gate runs whatever its needs did" "1" \
  "$(printf '%s\n' "$gate_job" | grep -c '^    if: \${{ always() }}$')"
assert_eq "ci-gate reports as \"CI gate\"" "1" \
  "$(printf '%s\n' "$gate_job" | grep -c '^    name: CI gate$')"
mismatch=""
while IFS='|' read -r key context _; do
  [ -n "$key" ] || continue
  name="$(sed -n "/^  ${key}:\$/,/^    name:/p" "$ci" | sed -n 's/^    name: //p')"
  [ "$name" = "$context" ] || mismatch="${mismatch}${key}:'${name}' "
done <<EOF
$(bash scripts/ci-gate-verdict.sh --list)
EOF
assert_eq "each covered job carries the context name the table claims" "" "$mismatch"
assert_eq "teardown-guard skips only on an explicit false" "1" \
  "$(grep -cF "if: \${{ !cancelled() && needs.changes.outputs.teardown_guard_relevant != 'false' }}" "$ci")"
assert_eq "tmux-exact-targets skips only on an explicit false" "1" \
  "$(grep -cF "if: \${{ !cancelled() && needs.changes.outputs.tmux_targets_relevant != 'false' }}" "$ci")"
assert_eq "changes exports both relevance outputs" "2" \
  "$(grep -cE '^      (teardown_guard|tmux_targets)_relevant: \$\{\{ steps\.detect-shell-gates\.outputs\.' "$ci")"
# Owner ruling 2026-09-27: trusty-common is tested by its coverage lanes and
# nowhere else, so no single `-p trusty-common` run can pass for the crate.
lanes_job="$(sed -n '/^  trusty-common-lanes:$/,/^  # ----/p' "$ci")"
assert_eq "trusty-common-lanes runs the lanes script" "1" \
  "$(printf '%s\n' "$lanes_job" | grep -c '^        run: \./scripts/test_trusty_common_lanes\.sh$')"
assert_eq "trusty-common-lanes steps read the plan's trusty_common" "1" \
  "$(printf '%s\n' "$lanes_job" | grep -c 'SELECTED: \${{ needs\.affected-plan\.outputs\.trusty_common }}')"
assert_eq "no single cargo test -p trusty-common step remains" "0" \
  "$(grep -vE '^[[:space:]]*#' "$ci" | grep -c 'cargo test -p trusty-common')"
for moved in teardown-guard tmux-exact-targets tag-publish-parity; do
  assert_eq "${moved}.yml is gone (one check run per name)" "0" \
    "$([ -e ".github/workflows/${moved}.yml" ] && echo 1 || echo 0)"
done
assert_eq "website-tests.yml no longer defines the corpus job" "0" \
  "$(grep -c '^    name: Website content corpus$' .github/workflows/website-tests.yml)"

# ---------------------------------------------------------------------------
echo
echo "vmtest-harness:"
vm_pr="$(sed -n '/^  pull_request:$/,/^[a-z]/p' .github/workflows/vmtest-harness.yml)"
assert_eq "pull_request is path-filtered to the harness" "1" \
  "$(printf '%s\n' "$vm_pr" | grep -c '^      - "vmtest-harness/\*\*"$')"
assert_eq "pull_request filter includes the workflow itself" "1" \
  "$(printf '%s\n' "$vm_pr" | grep -c '^      - ".github/workflows/vmtest-harness.yml"$')"

echo
if [ "$FAILURES" -gt 0 ]; then
  echo "ci-gate-selftest: ${FAILURES}/${CASES} case(s) FAILED"
  exit 1
fi
echo "ci-gate-selftest: all ${CASES} cases passed"

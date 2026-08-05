#!/usr/bin/env bash
#
# check-ci-helpers-selftest.sh — regression fixtures for the CI helper scripts
# (issues #4179, #4468, #4421, #4688).
#
# Why: the three helpers this covers each encode a decision that is invisible
#   until it is WRONG in production — a cancelled run reported as green, a
#   compiled-in asset classified as documentation and skipped, a drifted crate
#   waved through. None of that shows up in a diff review, and none of it is
#   reachable by `cargo test`. Same rationale as
#   scripts/check_line_cap_selftest.sh, which exists because a silent counting
#   bug in the SLOC gate is worse than no gate.
#
# What: asserts the exact mapping each helper produces:
#   classify-ci-results: every conclusion value, alone and in precedence pairs
#   detect-docs-only:    docs-only / code-only / mixed / embedded-asset / empty
#   check-pr-version-bump: registry-decision branches, via the stub oracle, plus
#                        the #4688 attribution pair — one tree run twice, once
#                        from a frozen stale base (misattributes) and once from
#                        the live base branch (does not) — and the workflow
#                        wiring that decides which of the two CI gets.
#
# Usage: bash scripts/check-ci-helpers-selftest.sh
# Exit: 0 when every case matches; 1 on the first mismatch, printing both sides.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

FAILURES=0
CASES=0

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $*"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-58s -> %s\n' "$1" "$3"
  else
    fail "$1: expected '$2', got '$3'"
  fi
}

# ---------------------------------------------------------------------------
# classify-ci-results.sh
# ---------------------------------------------------------------------------
verdict_of() {
  CI_JOB_RESULTS="$1" bash scripts/classify-ci-results.sh 2>/dev/null |
    sed -n 's/^verdict=//p'
}

echo "classify-ci-results:"
assert_eq "success"                       "green"        "$(verdict_of 'test=success')"
assert_eq "failure"                       "red"          "$(verdict_of 'test=failure')"
assert_eq "cancelled"                     "inconclusive" "$(verdict_of 'test=cancelled')"
assert_eq "skipped"                       "inconclusive" "$(verdict_of 'test=skipped')"
assert_eq "timed_out"                     "red"          "$(verdict_of 'test=timed_out')"
assert_eq "unknown value"                 "inconclusive" "$(verdict_of 'test=neutral')"
assert_eq "empty input (fail closed)"     "inconclusive" "$(verdict_of '')"
assert_eq "all success"                   "green"        "$(verdict_of 'a=success b=success c=success')"
assert_eq "success + cancelled"           "inconclusive" "$(verdict_of 'a=success b=cancelled')"
assert_eq "success + skipped"             "inconclusive" "$(verdict_of 'a=success b=skipped')"
assert_eq "failure outranks cancelled"    "red"          "$(verdict_of 'a=cancelled b=failure')"
assert_eq "failure outranks skipped"      "red"          "$(verdict_of 'a=skipped b=failure')"
# Both cases above put `failure` LAST, so they pass even without the
# red-precedence guard in classify-ci-results.sh. These assert the other
# direction: once red, nothing downgrades it. Order-independence is the actual
# contract, and only these cases hold the guard in place.
assert_eq "cancelled cannot downgrade red" "red"         "$(verdict_of 'a=failure b=cancelled')"
assert_eq "skipped cannot downgrade red"   "red"         "$(verdict_of 'a=failure b=skipped')"
assert_eq "success+cancelled after red"    "red"         "$(verdict_of 'a=failure b=success c=cancelled')"
assert_eq "the #4179 live run shape"      "inconclusive" \
  "$(verdict_of 'fmt=success clippy=cancelled test=cancelled msrv=cancelled smoke=cancelled')"

# ---------------------------------------------------------------------------
# detect-docs-only.sh
# ---------------------------------------------------------------------------
docs_only_of() {
  printf '%s' "$1" | bash scripts/detect-docs-only.sh 2>/dev/null |
    sed -n 's/^docs_only=//p'
}

echo
echo "detect-docs-only:"
assert_eq "docs/ tree"            "true"  "$(docs_only_of 'docs/specs/foo.md')"
assert_eq "docs/ nested asset"    "true"  "$(docs_only_of 'docs/a/b/c/diagram.svg')"
assert_eq "root markdown"         "true"  "$(docs_only_of 'README.md')"
assert_eq "root CHANGELOG"        "true"  "$(docs_only_of 'CHANGELOG.md')"
assert_eq "LICENSE"               "true"  "$(docs_only_of 'LICENSE')"
assert_eq "crate README"          "true"  "$(docs_only_of 'crates/trusty-mpm/README.md')"
assert_eq "crate CHANGELOG"       "true"  "$(docs_only_of 'crates/trusty-mpm/CHANGELOG.md')"
assert_eq "changelog fragment"    "true"  "$(docs_only_of 'crates/trusty-mpm/changelog.d/4468-x.md')"
assert_eq "issue template"        "true"  "$(docs_only_of '.github/ISSUE_TEMPLATE/bug.md')"
assert_eq "PR template"           "true"  "$(docs_only_of '.github/PULL_REQUEST_TEMPLATE.md')"
assert_eq "docs-only multi-file"  "true"  "$(docs_only_of 'docs/a.md
README.md
crates/trusty-mpm/changelog.d/1-x.md')"

assert_eq "rust source"           "false" "$(docs_only_of 'crates/trusty-mpm/src/lib.rs')"
assert_eq "Cargo.toml"            "false" "$(docs_only_of 'Cargo.toml')"
assert_eq "Cargo.lock"            "false" "$(docs_only_of 'Cargo.lock')"
assert_eq "workflow file"         "false" "$(docs_only_of '.github/workflows/ci.yml')"
assert_eq "script"                "false" "$(docs_only_of 'scripts/check_line_cap.sh')"
assert_eq "UI source"             "false" "$(docs_only_of 'crates/trusty-agents/ui/src/App.svelte')"
# The trap this denylist exists to avoid: markdown UNDER a crate's src/ is a
# bundled agent/skill/instruction asset compiled in via include_dir!.
assert_eq "embedded .md asset"    "false" "$(docs_only_of 'crates/trusty-code/src/assets/agents/ops.md')"
assert_eq "nested fragment"       "false" "$(docs_only_of 'crates/x/changelog.d/sub/1.md')"
assert_eq "mixed docs + code"     "false" "$(docs_only_of 'docs/a.md
crates/trusty-mpm/src/lib.rs')"
assert_eq "empty (fail closed)"   "false" "$(docs_only_of '')"

# ---------------------------------------------------------------------------
# check-pr-version-bump.sh — decision branches with the registry stubbed.
# ---------------------------------------------------------------------------
echo
echo "check-pr-version-bump:"

STUB_DIR="$(mktemp -d)"
trap 'rm -rf "${STUB_DIR}"' EXIT

# Oracle: "pinned-crate 1.0.0" is published; everything else is not.
cat >"${STUB_DIR}/published.sh" <<'STUB'
#!/usr/bin/env bash
[ "$1" = "pinned-crate" ] && [ "$2" = "1.0.0" ] && exit 0
exit 1
STUB
cat >"${STUB_DIR}/notpublished.sh" <<'STUB'
#!/usr/bin/env bash
exit 1
STUB
cat >"${STUB_DIR}/unreachable.sh" <<'STUB'
#!/usr/bin/env bash
exit 2
STUB
chmod +x "${STUB_DIR}/published.sh" "${STUB_DIR}/notpublished.sh" \
  "${STUB_DIR}/unreachable.sh"

# Build a throwaway repo so the gate sees a real merge base and a real diff.
make_fixture_repo() {
  local dir="$1" version_at_head="$2" publish_line="$3"
  rm -rf "${dir}"
  mkdir -p "${dir}/crates/pinned-crate/src"
  git -C "${dir}" init -q
  git -C "${dir}" config user.email ci@example.com
  git -C "${dir}" config user.name ci
  cat >"${dir}/crates/pinned-crate/Cargo.toml" <<EOF
[package]
name = "pinned-crate"
version = "1.0.0"
${publish_line}
EOF
  echo "// base" >"${dir}/crates/pinned-crate/src/lib.rs"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm base
  git -C "${dir}" branch -q base-ref

  echo "// drifted" >>"${dir}/crates/pinned-crate/src/lib.rs"
  sed -i.bak "s/^version = .*/version = \"${version_at_head}\"/" \
    "${dir}/crates/pinned-crate/Cargo.toml"
  rm -f "${dir}/crates/pinned-crate/Cargo.toml.bak"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm head
}

# run_gate <dir> <stub> [base-ref]  — base defaults to the `base-ref` branch.
run_gate() {
  local dir="$1" stub="$2" base="${3:-base-ref}"
  mkdir -p "${dir}/scripts"
  cp scripts/check-pr-version-bump.sh "${dir}/scripts/check-pr-version-bump.sh"
  (
    cd "${dir}" &&
      PR_VERSION_BUMP_BASE="${base}" \
        PR_VERSION_BUMP_REGISTRY_STUB="${stub}" \
        bash scripts/check-pr-version-bump.sh >/dev/null 2>&1
    echo "$?"
  )
}

make_fixture_repo "${STUB_DIR}/drift" "1.0.0" ""
assert_eq "src changed, version published, no bump -> fail" "1" \
  "$(run_gate "${STUB_DIR}/drift" "${STUB_DIR}/published.sh")"

make_fixture_repo "${STUB_DIR}/bumped" "1.0.1" ""
assert_eq "src changed, version bumped -> pass" "0" \
  "$(run_gate "${STUB_DIR}/bumped" "${STUB_DIR}/published.sh")"

make_fixture_repo "${STUB_DIR}/unpub" "1.0.0" ""
assert_eq "src changed, version not published -> pass" "0" \
  "$(run_gate "${STUB_DIR}/unpub" "${STUB_DIR}/notpublished.sh")"

make_fixture_repo "${STUB_DIR}/private" "1.0.0" "publish = false"
assert_eq "publish = false -> skipped, pass" "0" \
  "$(run_gate "${STUB_DIR}/private" "${STUB_DIR}/published.sh")"

make_fixture_repo "${STUB_DIR}/offline" "1.0.0" ""
assert_eq "registry unreachable -> warn, pass" "0" \
  "$(run_gate "${STUB_DIR}/offline" "${STUB_DIR}/unreachable.sh")"

# --- Attribution (#4688) ------------------------------------------------------
# The shape that failed docs-only PR #4666: the base branch itself changes a
# published crate's src, and an unrelated branch is checked out as a MERGE REF
# (`refs/pull/N/merge`) whose first parent is that same base tip. The PR's own
# diff contains no crate source at all, so the gate must clear it. It used to
# fail, because the base it diffed from was a frozen `base.sha` predating the
# base branch's own commits rather than the branch tip.
make_misattribution_repo() {
  local dir="$1"
  rm -rf "${dir}"
  mkdir -p "${dir}/crates/pinned-crate/src" "${dir}/docs"
  git -C "${dir}" init -q -b base-ref
  git -C "${dir}" config user.email ci@example.com
  git -C "${dir}" config user.name ci
  cat >"${dir}/crates/pinned-crate/Cargo.toml" <<'EOF'
[package]
name = "pinned-crate"
version = "1.0.0"
EOF
  echo "// base" >"${dir}/crates/pinned-crate/src/lib.rs"
  echo "docs" >"${dir}/docs/readme.md"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm "shared ancestor"
  # An unrelated branch forks HERE, touching documentation only...
  git -C "${dir}" branch -q pr-branch
  # ...and this is the base tip a `base.sha` snapshot would have frozen.
  git -C "${dir}" branch -q shared-ancestor
  # ...then the base branch drifts pinned-crate's src with no version bump.
  echo "// changed on the base branch" >>"${dir}/crates/pinned-crate/src/lib.rs"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm "base branch changes pinned-crate/src"
  git -C "${dir}" checkout -q pr-branch
  echo "more docs" >>"${dir}/docs/readme.md"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm "docs only"
  # What actions/checkout hands the job on a pull_request event.
  git -C "${dir}" checkout -q --detach base-ref
  git -C "${dir}" merge -q --no-ff --no-edit pr-branch
}

make_misattribution_repo "${STUB_DIR}/misattributed"
# The two halves of #4688, on ONE tree, so the difference is the base ref alone.
# `shared-ancestor` stands in for a frozen `github.event.pull_request.base.sha`
# that the base branch has since moved past: the gate then sweeps the base
# branch's own commit into the PR's diff and blames it. Asserted as a FAILURE on
# purpose — it is the defect, and it is why the workflow must never pass a
# frozen SHA (guarded below by `workflow passes a base BRANCH, not base.sha`).
assert_eq "frozen stale base -> misattributes (the #4688 defect)" "1" \
  "$(run_gate "${STUB_DIR}/misattributed" "${STUB_DIR}/published.sh" shared-ancestor)"
assert_eq "live base branch -> docs-only PR not blamed" "0" \
  "$(run_gate "${STUB_DIR}/misattributed" "${STUB_DIR}/published.sh" base-ref)"

# Wiring guard: the behavioural fix above is only delivered if the workflow
# actually hands the gate a branch. Assert the wiring, not just the script.
version_parity_wf=".github/workflows/version-parity.yml"
assert_eq "workflow passes a base BRANCH, not base.sha" "1" \
  "$(grep -c 'PR_VERSION_BUMP_BASE: origin/\${{ github.event.pull_request.base.ref }}' \
    "${version_parity_wf}" || true)"
assert_eq "workflow no longer passes the frozen base.sha" "0" \
  "$(grep -c 'PR_VERSION_BUMP_BASE: \${{ github.event.pull_request.base.sha }}' \
    "${version_parity_wf}" || true)"
assert_eq "main-side parity also runs on a schedule (#4688)" "1" \
  "$(grep -c '^  schedule:' "${version_parity_wf}" || true)"

# Same #4688 staleness class, found live on PR #4960: ci.yml's own `changes`
# job (the docs-only classifier every gated job in ci.yml consumes) fed
# detect-docs-only.sh a frozen `github.event.pull_request.base.sha`. On a
# repo merging this often, the gap between "when the PR event fired" and
# "when this job runs" routinely spans several unrelated main merges, so
# `git merge-base <stale-sha> HEAD` resolved to the stale snapshot itself and
# swept every file touched by those merges into the diff — PR #4960 (4 files,
# all docs/) was classified as 23 changed paths and ran full Clippy/Test/MSRV/
# search-daemon-smoke. Same wiring guard as version-parity.yml above: assert
# the fix is actually wired, not just that detect-docs-only.sh is correct in
# isolation.
ci_wf=".github/workflows/ci.yml"
assert_eq "ci.yml passes a base BRANCH, not base.sha" "1" \
  "$(grep -c 'DOCS_ONLY_BASE: origin/\${{ github.event.pull_request.base.ref }}' \
    "${ci_wf}" || true)"
assert_eq "ci.yml no longer passes the frozen base.sha" "0" \
  "$(grep -c 'DOCS_ONLY_BASE: \${{ github.event.pull_request.base.sha }}' \
    "${ci_wf}" || true)"

# capabilities-drift.yml grew its own `changes` job (reusing detect-docs-only.sh,
# not a second predicate) so the tm-capabilities cargo build stops running
# unconditionally on every PR. Same wiring guard, same reason: the script being
# correct proves nothing if the workflow never passes it a live base.
capabilities_wf=".github/workflows/capabilities-drift.yml"
# 3 = every cargo-touching step in the job (toolchain install, cache, the
# actual `check_capabilities.sh` run) — update this count if a step is added
# or removed, the same way the count itself would need a human to notice.
assert_eq "capabilities-drift.yml gates its cargo run on docs_only" "3" \
  "$(grep -c "if: needs.changes.outputs.docs_only != 'true'" "${capabilities_wf}" || true)"
assert_eq "capabilities-drift.yml passes a base BRANCH, not base.sha" "1" \
  "$(grep -c 'DOCS_ONLY_BASE: origin/\${{ github.event.pull_request.base.ref }}' \
    "${capabilities_wf}" || true)"

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check-ci-helpers-selftest: ${FAILURES}/${CASES} case(s) FAILED"
  exit 1
fi
echo "check-ci-helpers-selftest: all ${CASES} cases passed"

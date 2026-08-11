#!/usr/bin/env bash
#
# check-ci-helpers-selftest.sh — regression fixtures for the CI helper scripts
# (issues #4179, #4468, #4421, #4688, #5407).
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
#   ci.yml `changes` job: that every gate verdict comes from a diff, never from
#                        the PR activity type, and that the push path's
#                        no-before-SHA arms still fail closed (#5407).
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
assert_eq "website/ tree"         "true"  "$(docs_only_of 'website/src/routes/+page.svelte')"
assert_eq "website/ nested asset" "true"  "$(docs_only_of 'website/static/img/logo.svg')"
assert_eq "website-only multi-file" "true" "$(docs_only_of 'website/package.json
docs/a.md')"

assert_eq "rust source"           "false" "$(docs_only_of 'crates/trusty-mpm/src/lib.rs')"
assert_eq "Cargo.toml"            "false" "$(docs_only_of 'Cargo.toml')"
assert_eq "Cargo.lock"            "false" "$(docs_only_of 'Cargo.lock')"
assert_eq "workflow file"         "false" "$(docs_only_of '.github/workflows/ci.yml')"
assert_eq "script"                "false" "$(docs_only_of 'scripts/check_line_cap.sh')"
assert_eq "ADR checker"           "true"  "$(docs_only_of 'scripts/check_adr.sh')"
assert_eq "doc-number checker"    "true"  "$(docs_only_of 'scripts/check_doc_numbers.sh')"
assert_eq "doc-number allowlist"  "true"  "$(docs_only_of '.doc-number-allowlist.tsv')"
assert_eq "doc-number workflow"   "true"  "$(docs_only_of '.github/workflows/doc-numbers.yml')"
assert_eq "SLD workflow wiring"   "true"  "$(docs_only_of '.github/workflows/sld-lint.yml')"
assert_eq "SLD checker"           "true"  "$(docs_only_of 'scripts/check_sld.sh')"
assert_eq "optional token gate"   "true"  "$(docs_only_of '.github/workflows/token-drift.yml')"
assert_eq "capabilities wrapper"  "true"  "$(docs_only_of 'scripts/check_capabilities.sh')"
assert_eq "UI source"             "false" "$(docs_only_of 'crates/trusty-agents/ui/src/App.svelte')"
# The trap this denylist exists to avoid: markdown UNDER a crate's src/ is a
# bundled agent/skill/instruction asset compiled in via include_dir!.
assert_eq "embedded .md asset"    "false" "$(docs_only_of 'crates/trusty-code/src/assets/agents/ops.md')"
assert_eq "nested fragment"       "false" "$(docs_only_of 'crates/x/changelog.d/sub/1.md')"
assert_eq "mixed docs + code"     "false" "$(docs_only_of 'docs/a.md
crates/trusty-mpm/src/lib.rs')"
assert_eq "mixed website + code"  "false" "$(docs_only_of 'website/src/routes/+page.svelte
crates/trusty-mpm/src/lib.rs')"
assert_eq "empty (fail closed)"   "false" "$(docs_only_of '')"

# ---------------------------------------------------------------------------
# detect-embedder-cuda-relevant.sh
# ---------------------------------------------------------------------------
cuda_relevant_of() {
  printf '%s' "$1" | bash scripts/detect-embedder-cuda-relevant.sh 2>/dev/null |
    sed -n 's/^embedder_cuda_relevant=//p'
}

echo
echo "detect-embedder-cuda-relevant:"
assert_eq "trusty-common source"     "true"  "$(cuda_relevant_of 'crates/trusty-common/src/embedder/mod.rs')"
assert_eq "trusty-common manifest"   "true"  "$(cuda_relevant_of 'crates/trusty-common/Cargo.toml')"
assert_eq "workspace manifest"       "true"  "$(cuda_relevant_of 'Cargo.toml')"
assert_eq "workspace lockfile"       "true"  "$(cuda_relevant_of 'Cargo.lock')"
assert_eq "Cargo configuration"      "true"  "$(cuda_relevant_of '.cargo/config.toml')"
assert_eq "CI workflow"              "true"  "$(cuda_relevant_of '.github/workflows/ci.yml')"
assert_eq "scope detector"           "true"  "$(cuda_relevant_of 'scripts/detect-embedder-cuda-relevant.sh')"
assert_eq "unrelated Rust crate"     "false" "$(cuda_relevant_of 'crates/trusty-mpm/src/main.rs')"
assert_eq "trusty-search CUDA code"  "false" "$(cuda_relevant_of 'crates/trusty-search/src/main.rs')"
assert_eq "documentation"            "false" "$(cuda_relevant_of 'docs/adr/0001-example.md')"
assert_eq "mixed relevant changes"   "true"  "$(cuda_relevant_of 'docs/adr/0001-example.md
crates/trusty-common/src/lib.rs')"
assert_eq "empty diff (fail closed)" "true"  "$(cuda_relevant_of '')"

cuda_job="$(sed -n '/^  embedder-cuda-check:/,/^  # ui-checks:/p' .github/workflows/ci.yml)"
assert_eq "CI exports CUDA relevance" "1" \
  "$(grep -c '^      embedder_cuda_relevant:.*steps.detect-cuda' .github/workflows/ci.yml || true)"
assert_eq "CUDA job is gated by crate relevance" "1" \
  "$(grep -c "needs.changes.outputs.embedder_cuda_relevant != 'false'" <<<"$cuda_job" || true)"
assert_eq "CUDA job no longer uses broad docs-only gate" "0" \
  "$(grep -c 'needs.changes.outputs.docs_only' <<<"$cuda_job" || true)"
assert_eq "main notifier treats an intentional CUDA skip as green" "1" \
  "$(grep -c "embedder-cuda-check=.*embedder_cuda_relevant != 'false'" .github/workflows/ci.yml || true)"

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
  "$(grep -c 'DOCS_ONLY_BASE="origin/\${{ github.event.pull_request.base.ref }}"' \
    "${ci_wf}" || true)"
assert_eq "ci.yml no longer passes the frozen base.sha" "0" \
  "$(grep -c 'DOCS_ONLY_BASE: \${{ github.event.pull_request.base.sha }}' \
    "${ci_wf}" || true)"
assert_eq "ci.yml classifies the push range instead of forcing full Cargo" "1" \
  "$(grep -c 'DOCS_ONLY_BASE="\$before" bash scripts/detect-docs-only.sh' \
    "${ci_wf}" || true)"

# #5407: the `changes` job decided from the EVENT, not the diff — activity type
# `edited` with a null `changes.base` short-circuited to `docs_only=true` and
# `embedder_cuda_relevant=false`, so a retitle on a Rust PR no-opped every gated
# step and still reported `Test` SUCCESS in ~14 s. `cancel-in-progress` excludes
# `edited`, so that run coexisted with the real one, and GitHub shows only the
# later-completing check-run per context name: the unrun green superseded the
# real verdict everywhere, `gh pr checks` included. The two guards below are
# deliberately structural rather than string-matched on the old wording — the
# short-circuit was written twice, and a third detector would inherit it.
changes_job="$(sed -n '/^  changes:/,/^  fmt:/p' "${ci_wf}")"
assert_eq "changes job never branches on the activity type (#5407)" "0" \
  "$(grep -c 'github\.event\.action' <<<"${changes_job}" || true)"
# Only the FAIL-OPEN verdicts are banned as literals: a skip must be something a
# detector concluded from a diff. The fail-CLOSED literals on the push path
# (docs_only=false, embedder_cuda_relevant=true) stay, and are asserted below.
assert_eq "no hardcoded skip verdict in the changes job (#5407)" "0" \
  "$(grep -cE 'docs_only=true|embedder_cuda_relevant=false' <<<"${changes_job}" || true)"
assert_eq "a push with no before SHA still fails closed" "1" \
  "$(grep -c 'echo "docs_only=false" >> "\$GITHUB_OUTPUT"' <<<"${changes_job}" || true)"
assert_eq "a push with no before SHA still runs the CUDA check" "1" \
  "$(grep -c 'echo "embedder_cuda_relevant=true" >> "\$GITHUB_OUTPUT"' <<<"${changes_job}" || true)"

# capabilities-drift is optional, so it can use exact trigger paths instead of
# paying for a duplicate checkout/classifier job on every PR.
capabilities_wf=".github/workflows/capabilities-drift.yml"
assert_eq "capabilities-drift.yml has no duplicate classifier" "0" \
  "$(grep -c '^  changes:' "${capabilities_wf}" || true)"
assert_eq "capabilities-drift.yml scopes both events to trusty-mpm" "2" \
  "$(grep -c '      - "crates/trusty-mpm/\*\*"' "${capabilities_wf}" || true)"

# ADR consistency is a pure-shell document gate. Keep it beside document
# allocation rather than installing Rust in the SLD workflow for ADR-only PRs.
assert_eq "doc-numbers owns the ADR consistency step" "1" \
  "$(grep -c 'run: bash scripts/check_adr.sh' .github/workflows/doc-numbers.yml || true)"
assert_eq "SLD no longer runs the ADR consistency step" "0" \
  "$(grep -c 'run: bash scripts/check_adr.sh' .github/workflows/sld-lint.yml || true)"

# ---------------------------------------------------------------------------
# ci-free-disk-space.sh (#5325)
#
# The step it replaces spent 147s deleting SDKs on a runner that already had
# 87G free against a ~27G peak. The gate that stops that is a threshold
# comparison, and a threshold comparison is exactly the kind of thing that
# silently inverts. Drive every branch with injected measurements; --dry-run so
# nothing is actually deleted while this runs on a live runner.
# ---------------------------------------------------------------------------
disk_decision() { # disk_decision <avail_gb> [<avail_after_purge_gb>]
  CI_DISK_AVAIL_GB="$1" CI_DISK_AVAIL_AFTER_GB="${2:-}" \
    bash scripts/ci-free-disk-space.sh --dry-run 2>/dev/null |
    sed -n 's/^decision=//p'
}

echo
echo "ci-free-disk-space:"
assert_eq "87G observed on the measured run -> no work" "skip-all" "$(disk_decision 87)"
assert_eq "at the purge floor exactly"                  "skip-all" "$(disk_decision 45)"
assert_eq "just below the purge floor"                  "purge"    "$(disk_decision 44 44)"
assert_eq "purge frees enough to skip the prune"        "purge"    "$(disk_decision 30 60)"
assert_eq "purge leaves under the prune floor"          "purge+prune" "$(disk_decision 30 24)"
assert_eq "at the prune floor exactly"                  "purge"    "$(disk_decision 30 25)"
assert_eq "empty disk takes both tiers"                 "purge+prune" "$(disk_decision 1 2)"

# The CI_DISK_AVAIL_GB override short-circuits the `df` call, so every case
# above leaves the real measurement path untested. These drive it, by putting a
# stub `df` ahead of the real one on PATH. Without this the numeric guard in
# measure_avail_gb could be deleted and nothing here would go red.
# CI_DISK_AVAIL_AFTER_GB is pinned so only the FIRST measurement varies.
disk_decision_real_df() { # disk_decision_real_df <what df prints>
  local tmp
  tmp="$(mktemp -d)"
  { printf '#!/bin/sh\ncat <<"STUB_EOF"\n%s\nSTUB_EOF\n' "$1" > "${tmp}/df"; } 2>/dev/null
  chmod +x "${tmp}/df"
  PATH="${tmp}:${PATH}" CI_DISK_AVAIL_GB="" CI_DISK_AVAIL_AFTER_GB=999 \
    bash scripts/ci-free-disk-space.sh --dry-run 2>/dev/null |
    sed -n 's/^decision=//p'
  rm -rf "${tmp}"
}

# 125829120 KiB = 120G, 31457280 KiB = 30G — the real arithmetic, not an override.
assert_eq "real df, 120G free"          "skip-all" "$(disk_decision_real_df 'Avail
125829120')"
assert_eq "real df, 30G free"           "purge"    "$(disk_decision_real_df 'Avail
31457280')"
# An unreadable disk must read as FULL and reclaim, never as roomy and skip.
assert_eq "df prints a non-number"      "purge"    "$(disk_decision_real_df 'Avail
not-a-number')"
assert_eq "df prints nothing at all"    "purge"    "$(disk_decision_real_df '')"
assert_eq "df prints only a header"     "purge"    "$(disk_decision_real_df 'Avail')"

# Wiring: the script being correct proves nothing if a job still inlines the
# ungated `rm -rf`. All four jobs that reclaim disk must route through it.
assert_eq "ci.yml has no inlined SDK purge left" "0" \
  "$(grep -c 'sudo rm -rf /usr/share/dotnet' "${ci_wf}" || true)"
assert_eq "all four disk-reclaim jobs call the helper" "4" \
  "$(grep -c 'bash scripts/ci-free-disk-space.sh' "${ci_wf}" || true)"

# ---------------------------------------------------------------------------
# detect-pointer-lint-inputs.sh (#5311)
# ---------------------------------------------------------------------------
pointer_inputs_of() {
  printf '%s' "$1" | bash scripts/detect-pointer-lint-inputs.sh 2>/dev/null |
    sed -n 's/^pointer_inputs_changed=//p'
}

echo
echo "detect-pointer-lint-inputs:"
assert_eq "crate source"              "true"  "$(pointer_inputs_of 'crates/trusty-mpm/src/lib.rs')"
assert_eq "integration test"          "true"  "$(pointer_inputs_of 'crates/trusty-mpm/tests/x.rs')"
assert_eq "build script"              "true"  "$(pointer_inputs_of 'crates/trusty-mpm/build.rs')"
assert_eq "the allowlist"             "true"  "$(pointer_inputs_of '.test-pointer-allowlist.tsv')"
assert_eq "the gate itself"           "true"  "$(pointer_inputs_of 'scripts/check_test_pointers.sh')"
assert_eq "the scan-floor selftest"   "true"  "$(pointer_inputs_of 'scripts/check_scan_floor_selftest.sh')"
assert_eq "this classifier"           "true"  "$(pointer_inputs_of 'scripts/detect-pointer-lint-inputs.sh')"
assert_eq "the workflow wiring"       "true"  "$(pointer_inputs_of '.github/workflows/test-pointers.yml')"
assert_eq "documentation"             "false" "$(pointer_inputs_of 'docs/adr/0001-example.md')"
assert_eq "website"                   "false" "$(pointer_inputs_of 'website/src/routes/+page.svelte')"
assert_eq "an unrelated workflow"     "false" "$(pointer_inputs_of '.github/workflows/ci.yml')"
assert_eq "a manifest"                "false" "$(pointer_inputs_of 'crates/trusty-mpm/Cargo.toml')"
assert_eq "mixed docs + Rust"         "true"  "$(pointer_inputs_of 'docs/a.md
crates/trusty-mpm/src/lib.rs')"
assert_eq "empty diff (fail closed)"  "true"  "$(pointer_inputs_of '')"

# The trap this classifier exists to avoid, asserted as a difference: the
# Cargo-inert helper calls the lint's own inputs inert (true for a cargo build),
# so reusing it here would skip the gate on exactly the PR that removes a fixed
# allowlist entry — the failure mode check_test_pointers.sh is built to catch.
assert_eq "detect-docs-only calls the allowlist inert" "true" \
  "$(docs_only_of '.test-pointer-allowlist.tsv')"
assert_eq "this classifier does not"                   "true" \
  "$(pointer_inputs_of '.test-pointer-allowlist.tsv')"

# ---------------------------------------------------------------------------
# detect-version-bumps.sh (#5311)
# ---------------------------------------------------------------------------
# Under STUB_DIR so the EXIT trap set above still cleans it up — a second
# `trap ... EXIT` would replace the first and leak the earlier fixtures.
BUMP_DIR="${STUB_DIR}/versionbumps"
mkdir -p "${BUMP_DIR}"

# make_bump_repo <dir> <head-version-or-DELETE> [extra-crate-head-version]
make_bump_repo() {
  local dir="$1" head_version="$2" extra="${3:-}"
  rm -rf "${dir}"
  mkdir -p "${dir}/crates/alpha/src" "${dir}/crates/beta/src" "${dir}/docs"
  git -C "${dir}" init -q -b base-ref
  git -C "${dir}" config user.email ci@example.com
  git -C "${dir}" config user.name ci
  printf '[package]\nname = "alpha"\nversion = "1.0.0"\n' >"${dir}/crates/alpha/Cargo.toml"
  printf '[package]\nname = "beta"\nversion = "2.0.0"\n' >"${dir}/crates/beta/Cargo.toml"
  echo "// base" >"${dir}/crates/alpha/src/lib.rs"
  echo "// base" >"${dir}/crates/beta/src/lib.rs"
  echo "docs" >"${dir}/docs/readme.md"
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm base
  git -C "${dir}" checkout -q -b pr-branch

  # Every branch changes crate source; only the version line varies. That is the
  # whole point — "source changed" must NOT be what selects a crate here.
  echo "// changed" >>"${dir}/crates/alpha/src/lib.rs"
  if [ "${head_version}" = "DELETE" ]; then
    rm -rf "${dir}/crates/alpha"
  elif [ -n "${head_version}" ]; then
    printf '[package]\nname = "alpha"\nversion = "%s"\n' "${head_version}" \
      >"${dir}/crates/alpha/Cargo.toml"
  fi
  if [ -n "${extra}" ]; then
    printf '[package]\nname = "beta"\nversion = "%s"\n' "${extra}" \
      >"${dir}/crates/beta/Cargo.toml"
  fi
  git -C "${dir}" add -A
  git -C "${dir}" commit -qm head
}

# bumps_of <dir> — the emitted crate list, or `ERR<exit>` when it fails closed.
bumps_of() {
  local dir="$1" out rc=0
  mkdir -p "${dir}/scripts"
  cp scripts/detect-version-bumps.sh "${dir}/scripts/detect-version-bumps.sh"
  out="$(cd "${dir}" && VERSION_BUMP_BASE=base-ref bash scripts/detect-version-bumps.sh 2>/dev/null)" || rc=$?
  if [ "${rc}" -ne 0 ]; then
    echo "ERR${rc}"
    return 0
  fi
  printf '%s' "${out}" | sed -n 's/^bumped_crates=//p'
}

echo
echo "detect-version-bumps:"
make_bump_repo "${BUMP_DIR}/unbumped" ""
assert_eq "source changed, no version bump" "" "$(bumps_of "${BUMP_DIR}/unbumped")"
make_bump_repo "${BUMP_DIR}/bumped" "1.0.1"
assert_eq "version bumped"                  "alpha" "$(bumps_of "${BUMP_DIR}/bumped")"
make_bump_repo "${BUMP_DIR}/multi" "1.0.1" "2.1.0"
assert_eq "two crates bumped"               "alpha beta" "$(bumps_of "${BUMP_DIR}/multi")"
make_bump_repo "${BUMP_DIR}/removed" "DELETE"
assert_eq "crate deleted by the branch"     "" "$(bumps_of "${BUMP_DIR}/removed")"

# A crate this branch ADDS has no base manifest to compare, and that is a bump:
# check_semver.sh then records its own "never published — no baseline" skip
# rather than this classifier guessing on its behalf.
added_repo="${BUMP_DIR}/added"
make_bump_repo "${added_repo}" ""
mkdir -p "${added_repo}/crates/gamma/src"
printf '[package]\nname = "gamma"\nversion = "0.1.0"\n' >"${added_repo}/crates/gamma/Cargo.toml"
echo "// new" >"${added_repo}/crates/gamma/src/lib.rs"
git -C "${added_repo}" add -A
git -C "${added_repo}" commit -qm "add gamma"
assert_eq "crate added by the branch"       "gamma" "$(bumps_of "${added_repo}")"

# An inherited version declares nothing in the crate's own manifest, so there is
# nothing here to compare — and the workspace-level bump is its own diff entry.
inherit_repo="${BUMP_DIR}/inherited"
make_bump_repo "${inherit_repo}" ""
printf '[package]\nname = "alpha"\nversion.workspace = true\n' \
  >"${inherit_repo}/crates/alpha/Cargo.toml"
git -C "${inherit_repo}" add -A
git -C "${inherit_repo}" commit -qm "inherit version"
assert_eq "version.workspace = true"        "" "$(bumps_of "${inherit_repo}")"

# SCAN FLOOR: an empty diff means the base ref is wrong or the checkout is
# shallow. "No release under test" must never be the conclusion drawn from a
# lookup that failed, so this exits non-zero instead of reporting a clean set.
empty_repo="${BUMP_DIR}/empty"
make_bump_repo "${empty_repo}" "1.0.1"
git -C "${empty_repo}" checkout -q base-ref
assert_eq "empty diff fails closed"         "ERR2" "$(bumps_of "${empty_repo}")"

# ---------------------------------------------------------------------------
# #5311 wiring: a required context must RUN and REPORT on every PR.
#
# Two ways to break that, and both are invisible in a diff review until a PR
# hangs: a `paths:` filter on the pull_request trigger (GitHub creates no check
# run at all, so the context stays pending forever — observed on #5415/#5416 for
# version-parity) and a job-level `if:` (the job concludes `skipped`, which does
# not satisfy the requirement). Assert the absence of both, structurally.
# ---------------------------------------------------------------------------
echo
echo "required-context wiring (#5311):"

# pr_trigger_block <workflow> — the pull_request trigger's own lines, from
# `  pull_request:` to the next top-level `on:` key or the `concurrency:` block.
#
# awk, not a `sed` range: `\|` alternation is a GNU BRE extension, so a
# sed-range end pattern silently never matches under BSD sed and the "block"
# becomes the whole file — an assertion that passes for the wrong reason on a
# developer's machine and a different one in CI.
pr_trigger_block() {
  awk '
    /^  pull_request:/ { inblk = 1; next }
    inblk && /^[^[:space:]]/ { exit }   # a new top-level key (concurrency:, jobs:)
    inblk && /^  [^[:space:]]/ { exit } # a sibling trigger (push:, workflow_dispatch:)
    inblk { print }
  ' "$1"
}

for wf in .github/workflows/test-pointers.yml .github/workflows/semver-checks.yml; do
  assert_eq "${wf##*/}: pull_request trigger has no paths filter" "0" \
    "$(grep -cE '^    paths(-ignore)?:' <<<"$(pr_trigger_block "${wf}")" || true)"
  # `    if:` at four-space indent is a JOB-level condition; step-level ones are
  # indented six and are the prescribed mechanism, so they must not be counted.
  assert_eq "${wf##*/}: no job-level if: can skip the gate" "0" \
    "$(grep -cE '^    if:' "${wf}" || true)"
  assert_eq "${wf##*/}: has a no-op step that reports success" "1" \
    "$(grep -cE "^      - name: No .* — nothing to (lint|compare)$" "${wf}" || true)"
done

assert_eq "semver-checks runs on pull requests at all" "1" \
  "$(grep -c '^  pull_request:' .github/workflows/semver-checks.yml || true)"
assert_eq "semver-checks selects crates from the diff, not the event" "1" \
  "$(grep -c 'bash scripts/detect-version-bumps.sh' .github/workflows/semver-checks.yml || true)"
assert_eq "test-pointers classifies from the diff, not the event" "1" \
  "$(grep -c 'bash scripts/detect-pointer-lint-inputs.sh' .github/workflows/test-pointers.yml || true)"
# Structural, not string-matched on the old condition: the banned thing is any
# gate verdict taken from the ACTIVITY TYPE (#5407). `concurrency:` may still
# name it — that decides what gets cancelled, never what gets checked — so the
# assertion is scoped to the jobs the checks report from.
assert_eq "no gate in test-pointers branches on the activity type" "0" \
  "$(grep -c 'github\.event\.action' <<<"$(sed -n '/^jobs:/,$p' .github/workflows/test-pointers.yml)" || true)"
assert_eq "no gate in semver-checks branches on the activity type" "0" \
  "$(grep -c 'github\.event\.action' <<<"$(sed -n '/^jobs:/,$p' .github/workflows/semver-checks.yml)" || true)"
# The expensive half of the SemVer gate stays behind the diff verdict — this is
# what keeps #5149's "not 20 minutes on every PR" true while the context reports.
assert_eq "semver-checks gates its costly steps on there being work" "5" \
  "$(grep -c "if: steps.crate.outputs.have_work == 'true'" .github/workflows/semver-checks.yml || true)"

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check-ci-helpers-selftest: ${FAILURES}/${CASES} case(s) FAILED"
  exit 1
fi
echo "check-ci-helpers-selftest: all ${CASES} cases passed"

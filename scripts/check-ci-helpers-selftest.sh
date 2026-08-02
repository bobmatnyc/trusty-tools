#!/usr/bin/env bash
#
# check-ci-helpers-selftest.sh — regression fixtures for the CI helper scripts
# (issues #4179, #4468, #4421).
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
#   check-pr-version-bump: registry-decision branches, via the stub oracle
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

run_gate() {
  local dir="$1" stub="$2"
  mkdir -p "${dir}/scripts"
  cp scripts/check-pr-version-bump.sh "${dir}/scripts/check-pr-version-bump.sh"
  (
    cd "${dir}" &&
      PR_VERSION_BUMP_BASE=base-ref \
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

echo
if [ "${FAILURES}" -gt 0 ]; then
  echo "check-ci-helpers-selftest: ${FAILURES}/${CASES} case(s) FAILED"
  exit 1
fi
echo "check-ci-helpers-selftest: all ${CASES} cases passed"

#!/usr/bin/env bash
#
# contract_target_selftest.sh — cases for scripts/resolve_contract_target.sh and
#   for the two pre-publish.yml gate steps that consume it (#9539).
#
# Why: pre-publish.yml's Gates 5-6 hardcoded trusty-common. A run for any other
#   crate (trusty-mpm 1.8.0; trusty-review in run 37920566051) checked and
#   diffed trusty-common's contracts and labelled the verdict with the other
#   crate's name. The resolver derives the crate, its version and its
#   contracts.json path from one name; this file proves the resolution order
#   and that both gates use it.
#
# What: drives the resolver with synthetic INPUT_CRATE / REF_TYPE / REF_NAME
#   values against this checkout's real crates, asserting the exit code, the
#   key=value lines on stdout and the annotation on stderr. Then asserts the
#   wiring: the Gate 5 and Gate 6 steps call the resolver and carry no
#   hardcoded trusty-common crate, path or default.
#
#   The trusty-mpm cases assume crates/trusty-mpm has no contracts.json. When
#   it gains one, move those cases to a crate that still has none.
#
# Usage: bash scripts/contract_target_selftest.sh
#   CONTRACT_TARGET_RESOLVER / CONTRACT_TARGET_WORKFLOW point the cases at a
#   mutant copy, to show they can fail.
# Exit: 0 when every case matches; 1 otherwise, naming each mismatch.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOLVER="${CONTRACT_TARGET_RESOLVER:-${REPO_ROOT}/scripts/resolve_contract_target.sh}"
PREPUB="${CONTRACT_TARGET_WORKFLOW:-${REPO_ROOT}/.github/workflows/pre-publish.yml}"

FAILURES=0
CASES=0
ERRF="$(mktemp "${TMPDIR:-/tmp}/contract-target-selftest.XXXXXX")"
trap 'rm -f "$ERRF"' EXIT

assert_eq() {   # <label> <expected> <actual>
  CASES=$((CASES + 1))
  if [ "$2" = "$3" ]; then
    printf '  ok   %-60s -> %s\n' "$1" "$3"
  else
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1: expected '$2', got '$3'"
  fi
}

# Here-string, not a pipe into `grep -q`: see rustdoc-links-scope-selftest.sh.
has() { if grep -qF -- "$2" <<<"$1"; then echo yes; else echo no; fi; }

# The [package] table's version, read independently of the resolver.
pkg_version() {
  awk -F'"' '/^\[package\]$/ { p = 1; next } /^\[/ { p = 0 }
             p && /^version[[:space:]]*=/ { print $2; exit }' \
    "${REPO_ROOT}/crates/$1/Cargo.toml"
}

# resolve <input_crate> <ref_type> <ref_name>
#   Sets OUT (stdout), ERR (stderr) and RC.
resolve() {
  OUT="$(env -u GITHUB_OUTPUT INPUT_CRATE="$1" REF_TYPE="$2" REF_NAME="$3" \
    bash "$RESOLVER" --gate selftest-gate 2>"$ERRF")"
  RC=$?
  ERR="$(cat "$ERRF")"
}

field() { sed -n "s/^$1=//p" <<<"$OUT"; }

assert_eq "resolver script exists" yes "$( [ -f "$RESOLVER" ] && echo yes || echo no)"

MPM_VER="$(pkg_version trusty-mpm)"
TC_VER="$(pkg_version trusty-common)"

echo "contract-target: crate input"
resolve trusty-mpm '' ''
assert_eq "trusty-mpm input: exit is no-contracts (10)" 10 "$RC"
assert_eq "trusty-mpm input: crate" trusty-mpm "$(field crate)"
assert_eq "trusty-mpm input: its own Cargo.toml version" "$MPM_VER" "$(field version)"
assert_eq "trusty-mpm input: its own contracts path" \
  crates/trusty-mpm/contracts.json "$(field contracts)"
assert_eq "trusty-mpm input: SKIP annotation" yes "$(has "$ERR" '::warning::[SKIP] selftest-gate:')"
assert_eq "trusty-mpm input: SKIP names trusty-mpm" yes \
  "$(has "$ERR" 'crates/trusty-mpm/contracts.json')"
assert_eq "trusty-mpm input: no trusty-common anywhere" no \
  "$(has "${OUT}${ERR}" 'trusty-common')"

resolve trusty-common '' ''
assert_eq "trusty-common input: exit 0" 0 "$RC"
assert_eq "trusty-common input: crate" trusty-common "$(field crate)"
assert_eq "trusty-common input: its own Cargo.toml version" "$TC_VER" "$(field version)"
assert_eq "trusty-common input: its own contracts path" \
  crates/trusty-common/contracts.json "$(field contracts)"
assert_eq "trusty-common input: no SKIP" no "$(has "$ERR" '[SKIP]')"

echo "contract-target: tag push"
resolve '' tag trusty-mpm-v1.8.0
assert_eq "tag trusty-mpm-v1.8.0: exit 10" 10 "$RC"
assert_eq "tag trusty-mpm-v1.8.0: crate" trusty-mpm "$(field crate)"
assert_eq "tag trusty-mpm-v1.8.0: version" "$MPM_VER" "$(field version)"

resolve '' tag "trusty-common-v${TC_VER}"
assert_eq "tag trusty-common-v<ver>: exit 0" 0 "$RC"
assert_eq "tag trusty-common-v<ver>: crate" trusty-common "$(field crate)"

resolve '' tag trusty-mpm-v1.8.0-rc.1
assert_eq "prerelease tag: crate" trusty-mpm "$(field crate)"

resolve trusty-common tag trusty-mpm-v1.8.0
assert_eq "input wins over the tag" trusty-common "$(field crate)"

echo "contract-target: unresolved"
resolve '' tag not-a-release-tag
assert_eq "unparseable tag: exit is unresolved (11)" 11 "$RC"
assert_eq "unparseable tag: loud SKIP" yes "$(has "$ERR" '::warning::[SKIP] selftest-gate:')"
assert_eq "unparseable tag: says could not be resolved" yes "$(has "$ERR" 'could not be resolved')"
assert_eq "unparseable tag: names the tag" yes "$(has "$ERR" 'not-a-release-tag')"
assert_eq "unparseable tag: no crate line" '' "$(field crate)"

resolve '' tag trusty-mpm-v1.8
assert_eq "two-part version tag: unresolved" 11 "$RC"

resolve '' branch main
assert_eq "dispatch with no input: unresolved" 11 "$RC"
assert_eq "dispatch with no input: no trusty-common default" no \
  "$(has "$OUT" 'trusty-common')"

echo "contract-target: unknown crate"
resolve no-such-crate '' ''
assert_eq "unknown crate input: exit 1" 1 "$RC"
assert_eq "unknown crate input: error annotation" yes "$(has "$ERR" '::error::')"
assert_eq "unknown crate input: names it" yes "$(has "$ERR" 'no-such-crate')"
assert_eq "unknown crate input: no SKIP" no "$(has "$ERR" '[SKIP]')"

resolve '' tag no-such-crate-v1.0.0
assert_eq "unknown crate from tag: exit 1" 1 "$RC"

resolve ../crates/trusty-common '' ''
assert_eq "path-shaped input: exit 1" 1 "$RC"

echo "contract-target: workflow wiring"
# step <name-prefix>: one step's text, from its `- name:` to the next step or
# job, with comment lines dropped so prose about trusty-common does not count.
step() {
  awk -v want="      - name: \"$1" '
    index($0, want) == 1 { on = 1; print; next }
    on && (/^      - / || /^  [A-Za-z0-9_-]+:/) { exit }
    on' "$PREPUB" | grep -vE '^[[:space:]]*#'
}
for gate in "Gate 5" "Gate 6"; do
  text="$(step "$gate")"
  assert_eq "${gate} step found" yes "$( [ -n "$text" ] && echo yes || echo no)"
  assert_eq "${gate} calls the resolver" yes \
    "$(has "$text" 'scripts/resolve_contract_target.sh')"
  assert_eq "${gate} has no crates/trusty-common path" no "$(has "$text" 'crates/trusty-common')"
  assert_eq "${gate} has no --crate trusty-common" no "$(has "$text" '--crate trusty-common')"
  assert_eq "${gate} has no trusty-common default" no "$(has "$text" "'trusty-common'")"
done

echo
if [ "$FAILURES" -ne 0 ]; then
  echo "contract_target_selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "contract_target_selftest: all ${CASES} case(s) passed"

#!/usr/bin/env bash
#
# ci-crate-relevance-selftest.sh — regression fixtures for
#   scripts/ci-crate-relevance.sh (#7063).
#
# Why: that script decides whether four REQUIRED Tauri UI clippy jobs do any
#   real work, and both ways of being wrong are invisible in a diff review. Too
#   eager and the gate saves nothing; too eager the other way and a crate
#   reaches main uncompiled, which is the hole #5934 / #5935 closed. The
#   `false` answers below are the ones that matter: a detector that can only
#   ever say `true` is one nobody would notice had broken.
#
# What: builds a throwaway Cargo workspace with a known dependency shape —
#   top -> mid -> leaf, top -(dev)-> devonly, an unrelated crate, a sibling
#   whose name extends top's, and a crate nested inside another crate's
#   directory — and asserts the verdict for each. Testing the closure walk
#   against a fixture rather than against this repo's live graph is deliberate:
#   an unrelated PR that adds a dependency edge must not turn these cases red.
#
#   A short LIVE section follows, holding only the four pairs #7063 names as
#   its acceptance criteria. Those DO track this repo's real graph, and are
#   expected to change when a Tauri UI crate gains or loses a dependency.
#
#   Every fail-closed arm is covered too: no crate name, an unknown crate, an
#   empty change set, a blank-only change set, an unresolvable base ref, and a
#   `cargo metadata` that cannot run. The cargo-failure case is asserted
#   against an input that answers `false` when cargo works, so it proves the
#   arm was reached rather than that the answer happened to be true.
#
#   One section builds real single-commit git repos and drives the script
#   through its OWN `git diff` (#7063). Feeding paths on stdin cannot reach
#   that defect: git C-quotes a path holding a non-ASCII byte, a double quote
#   or a backslash, and the quoted string matches no crate directory, so a
#   change INSIDE a crate answered `false` and that crate's required clippy job
#   was skipped. Its last case is a non-ASCII path OUTSIDE the closure,
#   asserting `false`, so the section cannot pass by answering `true` always.
#
# Usage: bash scripts/ci-crate-relevance-selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing both sides of each
#   mismatch.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/ci-crate-relevance.sh"

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
    printf '  ok   %-62s -> %s\n' "$1" "$3"
  else
    fail "$1: expected '$2', got '$3'"
  fi
}

WORK="$(mktemp -d)" || exit 1
trap 'rm -rf "${WORK}"' EXIT

FIXTURE="${WORK}/fixture"
STUB_DIR="${WORK}/stub"
mkdir -p "${STUB_DIR}"

# ---------------------------------------------------------------------------
# Fixture workspace
#
#   top ──> mid ──> leaf          normal dependencies, one transitive hop
#   top ──(dev)──> devonly        dev-dependencies are in the closure because
#                                 clippy --all-targets compiles test targets
#   unrelated                     no edge to anything
#   top-extra                     name extends top's, to catch a prefix match
#   nested, nested/ui/src-tauri   a crate whose directory nests inside another
#                                 crate's, the trusty-audit-ui / trusty-agents-ui
#                                 shape
# ---------------------------------------------------------------------------
new_crate() {
  local dir="$1" name="$2"
  mkdir -p "${FIXTURE}/${dir}/src"
  : >"${FIXTURE}/${dir}/src/lib.rs"
  {
    echo '[package]'
    echo "name = \"${name}\""
    echo 'version = "0.1.0"'
    echo 'edition = "2021"'
  } >"${FIXTURE}/${dir}/Cargo.toml"
}

mkdir -p "${FIXTURE}"
{
  echo '[workspace]'
  echo 'resolver = "2"'
  echo 'members = ["crates/*", "crates/nested/ui/src-tauri"]'
} >"${FIXTURE}/Cargo.toml"

new_crate crates/leaf leaf
new_crate crates/mid mid
new_crate crates/top top
new_crate crates/devonly devonly
new_crate crates/unrelated unrelated
new_crate crates/top-extra top-extra
new_crate crates/nested nested
new_crate crates/nested/ui/src-tauri nested-ui

{
  echo ''
  echo '[dependencies]'
  echo 'leaf = { path = "../leaf" }'
} >>"${FIXTURE}/crates/mid/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'mid = { path = "../mid" }'
  echo ''
  echo '[dev-dependencies]'
  echo 'devonly = { path = "../devonly" }'
} >>"${FIXTURE}/crates/top/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'nested = { path = "../.." }'
} >>"${FIXTURE}/crates/nested/ui/src-tauri/Cargo.toml"

# A pristine copy, taken before any verdict runs so the Cargo.lock that
# `cargo metadata` writes never reaches a git fixture and never shows up as a
# changed path (Cargo.lock is a workspace-wide input, which would make every
# git-fixture case answer `true` for the wrong reason).
PRISTINE="${WORK}/pristine"
cp -R "${FIXTURE}" "${PRISTINE}"

# fixture_verdict <crate> <changed path>... — run the script inside the fixture.
fixture_verdict() {
  local crate="$1"
  shift
  (cd "${FIXTURE}" && printf '%s\n' "$@" | bash "${SCRIPT}" "${crate}" 2>/dev/null)
}

# live_verdict <crate> <changed path>... — run it against this repo.
live_verdict() {
  local crate="$1"
  shift
  (cd "${REPO_ROOT}" && printf '%s\n' "$@" | bash "${SCRIPT}" "${crate}" 2>/dev/null)
}

# gitfix_verdict <crate> <added path>... — build a throwaway git repo from the
# pristine fixture, add the named files in a second commit, and let the script
# resolve the change set with its OWN `git diff` (#7063). A fresh repo per case
# keeps the cases independent and needs no history rewriting between them.
gitfix_verdict() {
  local crate="$1" repo path base
  shift
  repo="$(mktemp -d "${WORK}/gitcase.XXXXXX")" || return 1
  cp -R "${PRISTINE}/." "${repo}/" || return 1
  (
    cd "${repo}" || exit 1
    git init -q . >/dev/null 2>&1
    git config user.email selftest@example.invalid >/dev/null 2>&1
    git config user.name selftest >/dev/null 2>&1
    git add -A >/dev/null 2>&1
    git commit -qm base >/dev/null 2>&1
    base="$(git rev-parse HEAD 2>/dev/null)"
    # Without this, a git that could not build the fixture would leave
    # CRATE_RELEVANCE_BASE empty, the script would read its empty stdin, and
    # the fail-closed arm would answer `true` — passing five of the six cases
    # below for a reason that has nothing to do with path quoting.
    if [ -z "${base}" ]; then
      echo "git-fixture-setup-failed"
      exit 0
    fi
    for path in "$@"; do
      mkdir -p "$(dirname "${path}")"
      printf 'pub fn x() {}\n' >"${path}"
    done
    git add -A >/dev/null 2>&1
    git commit -qm change >/dev/null 2>&1
    CRATE_RELEVANCE_BASE="${base}" bash "${SCRIPT}" "${crate}" </dev/null 2>/dev/null
  )
}

echo "fixture: the crate's own directory"
assert_eq "top <- crates/top/src/lib.rs" \
  "true" "$(fixture_verdict top crates/top/src/lib.rs)"
assert_eq "top <- crates/top/Cargo.toml" \
  "true" "$(fixture_verdict top crates/top/Cargo.toml)"
assert_eq "nested-ui <- crates/nested/ui/src-tauri/src/lib.rs (nested dir)" \
  "true" "$(fixture_verdict nested-ui crates/nested/ui/src-tauri/src/lib.rs)"

echo "fixture: the dependency closure"
assert_eq "top <- crates/mid/src/lib.rs (direct)" \
  "true" "$(fixture_verdict top crates/mid/src/lib.rs)"
assert_eq "top <- crates/leaf/src/lib.rs (transitive)" \
  "true" "$(fixture_verdict top crates/leaf/src/lib.rs)"
assert_eq "top <- crates/devonly/src/lib.rs (dev-dependency)" \
  "true" "$(fixture_verdict top crates/devonly/src/lib.rs)"
assert_eq "nested-ui <- crates/nested/src/lib.rs (parent crate is a dep)" \
  "true" "$(fixture_verdict nested-ui crates/nested/src/lib.rs)"
assert_eq "top <- one relevant among three" \
  "true" "$(fixture_verdict top crates/unrelated/src/lib.rs crates/mid/src/lib.rs docs/x.md)"

echo "fixture: outside the closure"
assert_eq "top <- crates/unrelated/src/lib.rs" \
  "false" "$(fixture_verdict top crates/unrelated/src/lib.rs)"
assert_eq "top <- docs/reference/ci-scripts.md" \
  "false" "$(fixture_verdict top docs/reference/ci-scripts.md)"
assert_eq "top <- two unrelated paths" \
  "false" "$(fixture_verdict top crates/unrelated/src/lib.rs docs/x.md)"
assert_eq "mid <- crates/top/src/lib.rs (edges are directed)" \
  "false" "$(fixture_verdict mid crates/top/src/lib.rs)"
assert_eq "nested-ui <- crates/unrelated/src/lib.rs" \
  "false" "$(fixture_verdict nested-ui crates/unrelated/src/lib.rs)"

echo "fixture: segment boundaries, not string prefixes"
# `crates/top` is a string prefix of `crates/top-extra`, and top-extra is in no
# closure. A `startswith` match with no separator check would answer true here.
assert_eq "top <- crates/top-extra/src/lib.rs" \
  "false" "$(fixture_verdict top crates/top-extra/src/lib.rs)"
assert_eq "top-extra <- crates/top/src/lib.rs" \
  "false" "$(fixture_verdict top-extra crates/top/src/lib.rs)"

echo "fixture: workspace-wide inputs"
for path in Cargo.lock Cargo.toml clippy.toml rust-toolchain rust-toolchain.toml \
  .cargo/config.toml .github/workflows/ci.yml \
  scripts/ci-apt-install.sh scripts/ci-crate-relevance.sh \
  scripts/ci-crate-relevance-selftest.sh; do
  assert_eq "top <- ${path}" "true" "$(fixture_verdict top "${path}")"
done

echo "fixture: git-quoted paths reach the matcher (#7063)"
# Each of these three characters makes git C-quote the path — wrapping it in
# literal double quotes and escaping the byte — unless the diff is read with
# `-z`. The quoted string is under no crate directory, so before the fix a
# change inside the crate answered `false`.
assert_eq "top <- crates/top/src/café.rs (non-ASCII, own dir)" \
  "true" "$(gitfix_verdict top 'crates/top/src/café.rs')"
assert_eq "top <- crates/mid/src/café.rs (non-ASCII, in the closure)" \
  "true" "$(gitfix_verdict top 'crates/mid/src/café.rs')"
assert_eq 'top <- crates/top/src/a"b.rs (double quote, own dir)' \
  "true" "$(gitfix_verdict top 'crates/top/src/a"b.rs')"
assert_eq 'top <- crates/top/src/back\slash.rs (backslash, own dir)' \
  "true" "$(gitfix_verdict top 'crates/top/src/back\slash.rs')"
# The control: a quoted path that is genuinely inert must still answer `false`,
# so the four above cannot be passing because the fix answers `true` always.
assert_eq "top <- crates/unrelated/src/café.rs (non-ASCII, outside)" \
  "false" "$(gitfix_verdict top 'crates/unrelated/src/café.rs')"
# An ordinary ASCII path through the same git-diff route, pinning that `-z`
# did not break the common case.
assert_eq "top <- crates/top/src/plain.rs (ASCII, own dir)" \
  "true" "$(gitfix_verdict top 'crates/top/src/plain.rs')"

echo "fixture: fail-closed arms"
assert_eq "no crate name" \
  "true" "$(cd "${FIXTURE}" && echo crates/unrelated/src/lib.rs | bash "${SCRIPT}" 2>/dev/null)"
assert_eq "unknown crate" \
  "true" "$(fixture_verdict not-a-workspace-member crates/unrelated/src/lib.rs)"
assert_eq "empty change set" \
  "true" "$(cd "${FIXTURE}" && printf '' | bash "${SCRIPT}" top 2>/dev/null)"
assert_eq "blank-only change set" \
  "true" "$(cd "${FIXTURE}" && printf '\n\n' | bash "${SCRIPT}" top 2>/dev/null)"
assert_eq "unresolvable base ref" \
  "true" "$(cd "${REPO_ROOT}" &&
    CRATE_RELEVANCE_BASE=refs/heads/definitely-not-a-ref \
      bash "${SCRIPT}" trusty-mpm-gui </dev/null 2>/dev/null)"

# cargo metadata failure. The same input answers `false` two cases above, so a
# `true` here can only come from the fail-closed arm.
printf '#!/bin/sh\nexit 1\n' >"${STUB_DIR}/cargo"
chmod +x "${STUB_DIR}/cargo"
assert_eq "cargo metadata failure" \
  "true" "$(cd "${FIXTURE}" && printf 'crates/unrelated/src/lib.rs\n' |
    PATH="${STUB_DIR}:${PATH}" bash "${SCRIPT}" top 2>/dev/null)"

echo "fixture: \$GITHUB_OUTPUT contract (read by ci.yml's \`changes\` job)"
OUT_FILE="${WORK}/github_output"
: >"${OUT_FILE}"
(cd "${FIXTURE}" && printf 'crates/unrelated/src/lib.rs\n' |
  GITHUB_OUTPUT="${OUT_FILE}" bash "${SCRIPT}" top-extra >/dev/null 2>&1)
assert_eq "hyphens become underscores in the key" \
  "top_extra_relevant=false" "$(grep '^top_extra_relevant=' "${OUT_FILE}")"
assert_eq "exactly one reason line" \
  "1" "$(grep -c '^top_extra_reason=' "${OUT_FILE}")"
assert_eq "exactly two lines written" \
  "2" "$(wc -l <"${OUT_FILE}" | tr -d ' ')"

: >"${OUT_FILE}"
(cd "${FIXTURE}" && printf 'Cargo.lock\n' |
  GITHUB_OUTPUT="${OUT_FILE}" bash "${SCRIPT}" top >/dev/null 2>&1)
assert_eq "true verdict is written the same way" \
  "top_relevant=true" "$(grep '^top_relevant=' "${OUT_FILE}")"

# ---------------------------------------------------------------------------
# Live workspace — the four acceptance pairs from #7063. These read this
# repo's real dependency graph, so a PR that adds or removes an edge on a
# Tauri UI crate is expected to change them.
# ---------------------------------------------------------------------------
echo "live: the four #7063 acceptance pairs"
assert_eq "trusty-mpm-gui  <- crates/trusty-embedderd/src/lib.rs" \
  "false" "$(live_verdict trusty-mpm-gui crates/trusty-embedderd/src/lib.rs)"
assert_eq "trusty-code-gui <- crates/trusty-embedderd/src/lib.rs" \
  "false" "$(live_verdict trusty-code-gui crates/trusty-embedderd/src/lib.rs)"
assert_eq "trusty-audit-ui <- crates/trusty-common/src/lib.rs" \
  "true" "$(live_verdict trusty-audit-ui crates/trusty-common/src/lib.rs)"
for crate in trusty-agents-ui trusty-audit-ui trusty-mpm-gui trusty-code-gui; do
  assert_eq "${crate} <- Cargo.lock" "true" "$(live_verdict "${crate}" Cargo.lock)"
done

echo
if [ "${FAILURES}" -eq 0 ]; then
  echo "ci-crate-relevance-selftest: ${CASES} cases, all passed"
  exit 0
fi
echo "ci-crate-relevance-selftest: ${CASES} cases, ${FAILURES} FAILED"
exit 1

#!/usr/bin/env bash
#
# select-test-crates_selftest.sh — regression fixtures for
#   scripts/select-test-crates.sh (#7753).
#
# Why: a selector whose only failure mode is "answer everything" is never
#   noticed as broken; the cases that matter are the ones that narrow the
#   output, and the ones that prove a broken `cargo metadata` still fails open
#   rather than printing nothing.
#
# What: builds a throwaway Cargo workspace with a known dependency shape —
#   isolated (no edges in or out), leaf <- mid <- top (two-hop transitive
#   chain), top -(dev)-> devonly, and base <- consumer1 <- consumer2 (a
#   two-hop REVERSE chain, the shape the selector exists to compute) — and
#   asserts the crate set `select-test-crates.sh --files ...` prints for each
#   case. A fixture rather than this repo's live graph, so an unrelated PR
#   that adds a dependency edge cannot turn these cases red. A short LIVE
#   section follows, checking this repo's own trusty-common override.
#
# Usage: bash scripts/select-test-crates_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing both sides of each
#   mismatch.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/select-test-crates.sh"

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
    printf '  ok   %-62s -> %s\n' "$1" "$(printf '%s' "$3" | tr '\n' ',')"
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
#   isolated                      no edges in or out — the true "leaf" case
#   leaf <── mid <── top           normal deps, one transitive hop
#   top ──(dev)──> devonly         dev-dependencies count as edges (#7753,
#                                  same rationale as ci-crate-relevance.sh)
#   base <── consumer1 <── consumer2   two-hop REVERSE chain: changing base
#                                  must select consumer2 even though it only
#                                  depends on consumer1, never on base
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
  echo 'members = ["crates/*"]'
} >"${FIXTURE}/Cargo.toml"

new_crate crates/isolated isolated
new_crate crates/leaf leaf
new_crate crates/mid mid
new_crate crates/top top
new_crate crates/devonly devonly
new_crate crates/base base
new_crate crates/consumer1 consumer1
new_crate crates/consumer2 consumer2

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
  echo 'base = { path = "../base" }'
} >>"${FIXTURE}/crates/consumer1/Cargo.toml"

{
  echo ''
  echo '[dependencies]'
  echo 'consumer1 = { path = "../consumer1" }'
} >>"${FIXTURE}/crates/consumer2/Cargo.toml"

# A git repo so fallback_all_crates' `git rev-parse --show-toplevel` resolves
# inside the fixture (same as a real checkout whose cargo binary is broken),
# and so --staged mode has an index to diff against. No commit yet — the
# baseline commit happens after the --files-mode cases below, so any
# Cargo.lock those `cargo metadata` calls write lands in that commit rather
# than showing up as a stray untracked file for the --staged case.
(cd "${FIXTURE}" && git init -q . >/dev/null 2>&1)
(cd "${FIXTURE}" && git config user.email selftest@example.invalid >/dev/null 2>&1)
(cd "${FIXTURE}" && git config user.name selftest >/dev/null 2>&1)

ALL_EIGHT="base
consumer1
consumer2
devonly
isolated
leaf
mid
top"

# run <path>... — select-test-crates.sh --files <path>..., from the fixture.
run() {
  (cd "${FIXTURE}" && bash "${SCRIPT}" --files "$@" 2>/dev/null)
}

echo "fixture: leaf-crate change (no dependents) prints only itself"
assert_eq "isolated changes" \
  "isolated" "$(run crates/isolated/src/lib.rs)"

echo "fixture: forward closure — a dependency's change selects its dependents"
assert_eq "leaf changes -> leaf, mid, top (transitive)" \
  "leaf
mid
top" "$(run crates/leaf/src/lib.rs)"
assert_eq "mid changes -> mid, top (direct)" \
  "mid
top" "$(run crates/mid/src/lib.rs)"
assert_eq "devonly changes -> devonly, top (dev-dependency edge)" \
  "devonly
top" "$(run crates/devonly/src/lib.rs)"

echo "fixture: a base crate's change selects its full reverse closure"
assert_eq "base changes -> base, consumer1, consumer2 (two-hop reverse)" \
  "base
consumer1
consumer2" "$(run crates/base/src/lib.rs)"

echo "fixture: non-crate inputs"
assert_eq "docs-only change prints nothing" \
  "" "$(run docs/some-page.md)"
assert_eq "root-level *.md prints nothing" \
  "" "$(run README.md)"
assert_eq "Cargo.lock prints all crates" \
  "${ALL_EIGHT}" "$(run Cargo.lock)"
assert_eq "scripts/** prints all crates" \
  "${ALL_EIGHT}" "$(run scripts/some-gate.sh)"
assert_eq "an unknown path prints all crates (fail open)" \
  "${ALL_EIGHT}" "$(run some/unclassified/path.rs)"

echo "fixture: a broken cargo prints all crates and exits 0"
cat >"${STUB_DIR}/cargo" <<'EOF'
#!/usr/bin/env bash
exit 7
EOF
chmod +x "${STUB_DIR}/cargo"
broken_out="$(cd "${FIXTURE}" && PATH="${STUB_DIR}:${PATH}" bash "${SCRIPT}" --files crates/isolated/src/lib.rs 2>/dev/null)"
broken_exit=$?
assert_eq "broken cargo metadata -> all crates" "${ALL_EIGHT}" "${broken_out}"
assert_eq "broken cargo metadata -> exit 0" "0" "${broken_exit}"

echo "fixture: --cargo-args mode"
assert_eq "isolated --cargo-args -> -p isolated" \
  "-p isolated" "$(cd "${FIXTURE}" && bash "${SCRIPT}" --files crates/isolated/src/lib.rs --cargo-args 2>/dev/null)"

echo "fixture: --staged mode sees an untracked file"
(cd "${FIXTURE}" && git add -A >/dev/null 2>&1 && git commit -qm base >/dev/null 2>&1)
(cd "${FIXTURE}" && mkdir -p crates/top/src && echo 'pub fn y() {}' >crates/top/src/extra.rs)
staged_out="$(cd "${FIXTURE}" && bash "${SCRIPT}" --staged 2>/dev/null)"
assert_eq "untracked crates/top/src/extra.rs -> top (--staged sees untracked too)" \
  "top" "${staged_out}"
rm -f "${FIXTURE}/crates/top/src/extra.rs"

echo "fixture: empty change set fails open"
empty_out="$(cd "${FIXTURE}" && bash "${SCRIPT}" --files 2>/dev/null)"
assert_eq "--files with no paths -> all crates" "${ALL_EIGHT}" "${empty_out}"

echo "live: this repo's own trusty-common feature override"
live_out="$(cd "${REPO_ROOT}" && bash "${SCRIPT}" --files crates/trusty-common/src/lib.rs --cargo-args 2>/dev/null)"
case "${live_out}" in
  *"-p trusty-common --features unconditional-only"*)
    CASES=$((CASES + 1))
    printf '  ok   %-62s -> %s\n' "trusty-common --cargo-args carries the override" "present"
    ;;
  *)
    fail "trusty-common --cargo-args override missing: got '${live_out}'"
    ;;
esac

echo
echo "${CASES} cases, ${FAILURES} failures"
[ "${FAILURES}" -eq 0 ]

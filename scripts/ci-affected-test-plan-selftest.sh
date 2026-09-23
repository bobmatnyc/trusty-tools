#!/usr/bin/env bash
#
# ci-affected-test-plan-selftest.sh — fixtures for scripts/ci-affected-test-plan.sh.
#
# Why: the plan decides whether `Rust tests (affected crates)` runs any test at
#   all. A plan that drops a crate, or answers "nothing" for a Rust change,
#   turns that check green without testing anything.
#
# What: runs a copy of the plan next to a STUB select-test-crates.sh that
#   prints a fixed crate list (or fails), so the Cargo-inert short-circuit, the
#   Tauri UI exclusion, the leg split and the error arms are checked without
#   depending on this repo's live dependency graph. Two live cases at the end
#   run the real selector against this checkout: a docs-only path selects
#   nothing, and a trusty-common path selects trusty-common and no Tauri crate.
#
# Test: this file is the test; ci.yml's `affected-plan` job runs it before
#   the step that consults the plan.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)" || exit 1
trap 'rm -rf "$WORK"' EXIT
cp "${REPO}/scripts/ci-affected-test-plan.sh" "${WORK}/ci-affected-test-plan.sh"

fails=0
pass() { echo "ok   - $1"; }
fail() {
  echo "FAIL - $1"
  fails=$((fails + 1))
}

# stub <exit-code> <crate>... — the stub selector prints the crates and exits.
stub() {
  local rc="$1"
  shift
  {
    echo '#!/usr/bin/env bash'
    echo "touch '${WORK}/stub-called'"
    for c in "$@"; do echo "echo '$c'"; done
    echo "exit $rc"
  } >"${WORK}/select-test-crates.sh"
  chmod +x "${WORK}/select-test-crates.sh"
  rm -f "${WORK}/stub-called"
}

plan() { (cd "$REPO" && GITHUB_OUTPUT="" "${WORK}/ci-affected-test-plan.sh" "$@" 2>/dev/null); }
field() { printf '%s\n' "$1" | awk -v k="$2=" 'index($0, k) == 1 { print substr($0, length(k) + 1) }'; }

# 1. docs_only=true answers nothing and never consults the selector.
stub 0 trusty-mpm
out="$(plan --docs-only true -- --files x)"
if [ "$(field "$out" count)" = 0 ] && [ ! -e "${WORK}/stub-called" ]; then
  pass "docs_only=true -> count=0, selector not called"
else fail "docs_only=true: $out"; fi

# 2. An empty selection is count=0 with an explicit reason.
stub 0
out="$(plan --docs-only false -- --files docs/x.md)"
if [ "$(field "$out" count)" = 0 ] && [ "$(field "$out" matrix)" = '{"include":[]}' ] &&
  field "$out" reason | grep -q 'maps to no workspace crate'; then
  pass "empty selection -> count=0, empty matrix"
else fail "empty selection: $out"; fi

# 3. Only Tauri UI crates selected -> nothing for the headless runner.
stub 0 trusty-code-gui trusty-audit-ui
out="$(plan -- --files x)"
if [ "$(field "$out" count)" = 0 ] && field "$out" reason | grep -q 'Tauri'; then
  pass "Tauri-only selection -> count=0"
else fail "Tauri-only selection: $out"; fi

# 4. Tauri crates are dropped, everything else kept, one leg per crate.
stub 0 alpha trusty-mpm-gui beta gamma
out="$(plan -- --files x)"
m="$(field "$out" matrix)"
if [ "$(field "$out" count)" = 3 ] && [ "$(field "$out" crates)" = "alpha beta gamma" ] &&
  [ "$(printf '%s' "$m" | jq -r '.include[0].total')" = 3 ] &&
  ! printf '%s' "$m" | grep -q trusty-mpm-gui; then
  pass "mixed selection -> 3 crates, 3 legs, Tauri crate dropped"
else fail "mixed selection: $out"; fi

# 5. More crates than legs -> capped at 8, every crate exactly once.
names=(c01 c02 c03 c04 c05 c06 c07 c08 c09 c10 c11 c12)
stub 0 "${names[@]}"
out="$(plan -- --files x)"
m="$(field "$out" matrix)"
got="$(printf '%s' "$m" | jq -r '.include[].crates' | tr ' ' '\n' | sort | tr '\n' ' ')"
want="$(printf '%s\n' "${names[@]}" | tr '\n' ' ')"
if [ "$(printf '%s' "$m" | jq '.include | length')" = 8 ] &&
  [ "$(printf '%s' "$m" | jq -r '[.include[].total] | unique | .[0]')" = 8 ] &&
  [ "$got" = "$want" ]; then
  pass "12 crates -> 8 legs, each crate exactly once"
else fail "12 crates: got '$got' matrix $m"; fi

# 6. --max-legs bounds the split.
stub 0 a b c d e
out="$(plan --max-legs 2 -- --files x)"
if [ "$(field "$out" matrix | jq '.include | length')" = 2 ]; then
  pass "--max-legs 2 -> 2 legs"
else fail "--max-legs 2: $out"; fi

# 7. A failing selector fails the plan instead of answering "nothing".
stub 2
if plan -- --files x >/dev/null; then fail "selector exit 2 was swallowed"; else pass "selector failure -> non-zero exit"; fi

# 8. An unknown argument is a usage error.
plan --bogus >/dev/null
if [ $? -eq 2 ]; then pass "unknown argument -> exit 2"; else fail "unknown argument not rejected"; fi

# Live: the real selector against this checkout.
cp "${REPO}/scripts/select-test-crates.sh" "${WORK}/select-test-crates.sh"
out="$(plan -- --files docs/reference/ci-gates.md)"
if [ "$(field "$out" count)" = 0 ]; then pass "live: docs path -> count=0"; else fail "live docs: $out"; fi
out="$(plan -- --files crates/trusty-common/src/lib.rs)"
crates=" $(field "$out" crates) "
case "$crates" in
  *" trusty-code-gui "* | *" trusty-audit-ui "* | *" trusty-mpm-gui "* | *" trusty-agents-ui "*)
    fail "live: trusty-common selection kept a Tauri crate: $out" ;;
  *" trusty-common "*" trusty-mpm "*) pass "live: trusty-common path -> trusty-common + dependents, no Tauri crate" ;;
  *) fail "live: trusty-common selection: $out" ;;
esac

if [ "$fails" -ne 0 ]; then
  echo "ci-affected-test-plan-selftest: ${fails} case(s) failed"
  exit 1
fi
echo "ci-affected-test-plan-selftest: all cases passed"

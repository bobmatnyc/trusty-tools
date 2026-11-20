#!/usr/bin/env bash
#
# check_demo_intentions_selftest.sh — mutation self-test for
# scripts/check_demo_intentions.sh (#11489).
#
# Why: the lint is the only thing that keeps an `intent(demo):` test intention
#   inside declared demo paths. A matcher that stopped matching would pass a
#   clean tree and a violating tree alike, so CI proves it can fail first.
# What: one throwaway git repo per case with its own allowlist (passed through
#   DEMO_INTENTION_PATHS). Cases: a marker under an allowlisted path passes; a
#   marker outside fails and names file:line; prose in Markdown is not a
#   marker; an allowlist row naming no tracked path fails.
#
# Usage: bash scripts/check_demo_intentions_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_demo_intentions.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
FAILED=0

# new_fixture <name> <allowlist-row...>: a repo with a demo crate and an app crate.
new_fixture() {
  local dir="$TMP_ROOT/$1" row
  shift
  mkdir -p "$dir/crates/demo/src" "$dir/crates/app/src"
  printf '# demo paths\n' > "$dir/allow.txt"
  for row in "$@"; do printf '%s\n' "$row" >> "$dir/allow.txt"; done
  printf 'pub fn nothing() {}\n' > "$dir/crates/app/src/lib.rs"
  printf 'pub fn demo() {}\n' > "$dir/crates/demo/src/lib.rs"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# run_case <name> <want-exit> <needle-or-empty> <dir>
run_case() {
  local name="$1" want="$2" needle="$3" dir="$4" out status
  git -C "$dir" add -A
  set +e
  out="$(cd "$dir" && DEMO_INTENTION_PATHS=allow.txt bash "$GATE" 2>&1)"
  status=$?
  set -e
  if [ "$status" -ne "$want" ]; then
    echo "  FAIL  $name — wanted exit $want, got $status" >&2
    printf '%s\n' "$out" | sed 's/^/        /' >&2
    FAILED=$((FAILED + 1))
  elif [ -n "$needle" ] && ! grep -qF -- "$needle" <<<"$out"; then
    echo "  FAIL  $name — exit $status as wanted, no line contains: $needle" >&2
    printf '%s\n' "$out" | sed 's/^/        /' >&2
    FAILED=$((FAILED + 1))
  else
    echo "  ok    $name (exit $status)"
  fi
}

MARKER='#[test]
#[ignore = "intent(demo): the error path is owed"]
fn owed() {}'

echo "check_demo_intentions selftest"

d="$(new_fixture inside crates/demo/)"
printf '%s\n' "$MARKER" >> "$d/crates/demo/src/lib.rs"
run_case "a marker inside an allowlisted path passes" 0 "1 intention(s)" "$d"

d="$(new_fixture outside crates/demo/)"
printf '%s\n' "$MARKER" >> "$d/crates/app/src/lib.rs"
run_case "a marker outside the allowlist fails and names file:line" 1 \
  "crates/app/src/lib.rs:3:" "$d"

d="$(new_fixture empty)"
printf '%s\n' "$MARKER" >> "$d/crates/app/src/lib.rs"
run_case "an empty allowlist refuses every marker" 1 "crates/app/src/lib.rs:3:" "$d"

d="$(new_fixture prose)"
printf 'Write `#[ignore = "intent(demo): ..."]`.\n' > "$d/crates/app/README.md"
run_case "a mention in Markdown is not a marker" 0 "0 intention(s)" "$d"

d="$(new_fixture stale crates/gone/)"
run_case "an allowlist row naming no tracked path fails" 1 "crates/gone/" "$d"

if [ "$FAILED" -ne 0 ]; then
  echo "check_demo_intentions selftest: $FAILED case(s) failed" >&2
  exit 1
fi
echo "check_demo_intentions selftest: all cases passed"

#!/usr/bin/env bash
#
# ci-affected-test-plan.sh — turn a PR's change set into the leg matrix for
#   ci.yml's `Rust tests (affected crates)` job.
#
# Why: the pre-publish shards (`cargo nextest run --workspace`) do not run on
#   pull requests (#4179, #5407), so a PR executes no test at all before it
#   merges. Owner ruling 2026-09-23: a PR runs `cargo test --no-fail-fast` for
#   the crates it touches plus their dependents. The crate set comes from
#   scripts/select-test-crates.sh; this script only adds the three things a
#   workflow needs on top of that list — the Cargo-inert short-circuit, the
#   headless-runner exclusions, and a split into parallel legs so a
#   workspace-wide change still finishes inside one leg's timeout.
#
# What:
#   1. `--docs-only true` (the `changes` job's `docs_only` verdict) -> no crates.
#      select-test-crates.sh maps a changelog fragment or a crate README to its
#      owning crate; detect-docs-only.sh already rules those Cargo-inert, and the
#      two answers must agree with every other job in ci.yml.
#   2. Otherwise run select-test-crates.sh with every argument after `--`. Its
#      own rules apply unchanged: a root Cargo.toml/Cargo.lock, deny.toml,
#      rust-toolchain or .cargo/** change selects ALL crates; this job's own
#      inputs select the trusty-common + trusty-mpm canary; any other
#      scripts/** or .github/** path selects only the crates whose Rust source
#      names it literally, often none (#7777); and its FAIL OPEN applies on any
#      detection error.
#   3. Drop the four Tauri UI crates. The headless runner has no WebKit2GTK, and
#      each has its own dedicated job in ci.yml (see that file's header).
#   4. Split the rest into at most `--max-legs` legs (default 8, the shard
#      count), greedy longest-first on a weight of the crate's `#[test]` /
#      `#[tokio::test]` attribute count, so trusty-mpm (~8k tests) gets a leg
#      to itself when everything is selected.
#
# Output: `key=value` lines on stdout, also appended to $GITHUB_OUTPUT when set:
#   count=<n>  crates=<space-separated>  reason=<one line>
#   matrix={"include":[{"leg":1,"total":L,"crates":"a b"},...]}
#
# Exit: 0 on every answer, including "no crates". Non-zero only when
#   select-test-crates.sh itself exits non-zero (a malformed invocation) or on
#   a malformed invocation of this script, so the caller's job goes red rather
#   than guessing.
#
# Test: scripts/ci-affected-test-plan-selftest.sh

set -uo pipefail

MAX_LEGS=8
DOCS_ONLY=""
UI_CRATES="trusty-agents-ui trusty-audit-ui trusty-mpm-gui trusty-code-gui"

usage() {
  echo "Usage: ci-affected-test-plan.sh [--docs-only true|false] [--max-legs N] -- <select-test-crates.sh args>" >&2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --docs-only)
      [ $# -ge 2 ] || { usage; exit 2; }
      DOCS_ONLY="$2"
      shift 2
      ;;
    --max-legs)
      [ $# -ge 2 ] && [ "$2" -ge 1 ] 2>/dev/null || { usage; exit 2; }
      MAX_LEGS="$2"
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

emit() {
  printf '%s=%s\n' "$1" "$2"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s=%s\n' "$1" "$2" >>"$GITHUB_OUTPUT"
  fi
}

finish_empty() {
  emit count 0
  emit crates ""
  emit matrix '{"include":[]}'
  emit reason "$1"
  exit 0
}

if [ "$DOCS_ONLY" = "true" ]; then
  finish_empty "no affected crates: Cargo-inert change set (docs_only=true)"
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if ! selected="$("${HERE}/select-test-crates.sh" "$@")"; then
  echo "::error::select-test-crates.sh failed — cannot plan the affected-crate test run" >&2
  exit 1
fi

crates=()
dropped=()
for c in $selected; do
  case " ${UI_CRATES} " in
    *" ${c} "*) dropped+=("$c") ;;
    *) crates+=("$c") ;;
  esac
done

if [ ${#dropped[@]} -gt 0 ]; then
  echo "Tauri UI crates left to their dedicated ci.yml jobs: ${dropped[*]}" >&2
fi
if [ ${#crates[@]} -eq 0 ]; then
  if [ ${#dropped[@]} -gt 0 ]; then
    finish_empty "no affected crates: only Tauri UI crates (${dropped[*]}), tested by their own jobs"
  fi
  finish_empty "no affected crates: the change set maps to no workspace crate"
fi

# Weight each crate by its test-attribute count. Any lookup failure weighs 1:
# balance gets worse, the crate set does not change.
meta="$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null ||
  cargo metadata --no-deps --format-version 1 2>/dev/null || true)"
root="$(printf '%s' "$meta" | jq -r '.workspace_root // empty' 2>/dev/null)"
weighted=()
for c in "${crates[@]}"; do
  w=1
  dir="$(printf '%s' "$meta" | jq -r --arg n "$c" --arg r "${root}/" \
    '.packages[] | select(.name == $n) | .manifest_path | rtrimstr("/Cargo.toml") | ltrimstr($r)' 2>/dev/null)"
  if [ -n "$dir" ]; then
    n="$(git grep -c -E '^[[:space:]]*#\[(tokio::)?test' -- "${dir}/*.rs" 2>/dev/null |
      awk -F: '{ s += $NF } END { print s + 0 }')"
    [ "${n:-0}" -gt 0 ] && w="$n"
  fi
  weighted+=("${w} ${c}")
done

legs=${#crates[@]}
[ "$legs" -gt "$MAX_LEGS" ] && legs="$MAX_LEGS"
loads=()
members=()
for ((i = 0; i < legs; i++)); do
  loads[i]=0
  members[i]=""
done

# Longest-processing-time greedy: heaviest crate first, into the lightest leg.
while read -r w c; do
  best=0
  for ((i = 1; i < legs; i++)); do
    [ "${loads[i]}" -lt "${loads[best]}" ] && best=$i
  done
  loads[best]=$((loads[best] + w))
  members[best]="${members[best]:+${members[best]} }${c}"
done < <(printf '%s\n' "${weighted[@]}" | sort -k1,1nr -k2,2)

matrix="$(for ((i = 0; i < legs; i++)); do
  # shellcheck disable=SC2086 # split the space-joined crate names on purpose
  printf '%s\t%s\n' "$((i + 1))" "$(printf '%s\n' ${members[i]} | sort | tr '\n' ' ' | sed 's/ $//')"
done | jq -R -s -c --argjson total "$legs" \
  '{include: [split("\n")[] | select(length > 0) | split("\t") | {leg: (.[0] | tonumber), total: $total, crates: .[1]}]}')"

sorted="$(printf '%s\n' "${crates[@]}" | sort | tr '\n' ' ' | sed 's/ $//')"
emit count "${#crates[@]}"
emit crates "$sorted"
emit matrix "$matrix"
emit reason "${#crates[@]} affected crate(s) over ${legs} leg(s)"

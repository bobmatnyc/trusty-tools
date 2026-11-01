#!/usr/bin/env bash
#
# check_fastembed_cache_selftest.sh — planted cases for
# scripts/check_fastembed_cache.sh and the workflows that fill that cache
# (#10639).
#
# Why: the cache that made `default_model_matches_sentence_transformers_reference`
#   flake passed the old check, because the check stripped whitespace from
#   refs/main and hf-hub does not. A check only ever seen passing cannot show
#   that it agrees with hf-hub.
#
# What: builds a complete fixture cache, asserts the check passes on it, then
#   plants one fault per case and asserts exit 1 plus the expected text. Also
#   asserts that no workflow writes refs/main with `echo`, and that every
#   workflow derives the same fastembed cache key, so a poisoned cache cannot be
#   restored through a key site that was missed.
#
# Test: this IS the test. Run: ./scripts/check_fastembed_cache_selftest.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_fastembed_cache.sh"
WORKFLOWS="$SCRIPT_DIR/../.github/workflows"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/fastembed-cache-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

QDRANT="models--Qdrant--all-MiniLM-L6-v2-onnx"
XENOVA="models--Xenova--all-MiniLM-L6-v2"
REV="8f518e882455312b086101e60691f5e6e2f05c3c"
failures=0

fail() {
  echo "FAIL $1"
  failures=$((failures + 1))
}

# seed_repo <cache> <repo-dir> <model-file>
seed_repo() {
  local snap="$1/$2/snapshots/$REV" f
  mkdir -p "$1/$2/refs" "$(dirname "$snap/$3")"
  printf '%s' "$REV" > "$1/$2/refs/main"
  for f in tokenizer.json config.json special_tokens_map.json tokenizer_config.json "$3"; do
    printf 'x' > "$snap/$f"
  done
}

new_cache() {
  local cache
  cache="$(mktemp -d "$WORK/cache.XXXXXX")"
  seed_repo "$cache" "$QDRANT" model.onnx
  seed_repo "$cache" "$XENOVA" onnx/model_quantized.onnx
  echo "$cache"
}

# expect_red <case> <expected-text> <cache>
expect_red() {
  local out rc=0
  out="$(FASTEMBED_CACHE_DIR="$3" "$CHECK" 2>&1)" || rc=$?
  if [ "$rc" -ne 1 ]; then
    fail "$1: wanted exit 1, got $rc: $out"
  elif ! printf '%s' "$out" | grep -qF "$2"; then
    fail "$1: wanted text '$2' in: $out"
  else
    echo "ok   $1"
  fi
}

# --- the check on a clean cache -------------------------------------------
cache="$(new_cache)"
if out="$(FASTEMBED_CACHE_DIR="$cache" "$CHECK" 2>&1)"; then
  echo "ok   clean cache passes"
else
  fail "clean cache must pass: $out"
fi

# --- planted faults: refs/main --------------------------------------------
cache="$(new_cache)"
printf '%s\n' "$REV" > "$cache/$QDRANT/refs/main"
expect_red "fp32 ref with a trailing newline (the #10639 cache)" "contains whitespace" "$cache"

cache="$(new_cache)"
printf '%s\n' "$REV" > "$cache/$XENOVA/refs/main"
expect_red "INT8 ref with a trailing newline" "contains whitespace" "$cache"

cache="$(new_cache)"
printf '%s\r\n' "$REV" > "$cache/$QDRANT/refs/main"
expect_red "ref with CRLF" "contains whitespace" "$cache"

cache="$(new_cache)"
printf ' %s' "$REV" > "$cache/$QDRANT/refs/main"
expect_red "ref with a leading space" "contains whitespace" "$cache"

cache="$(new_cache)"
printf '\n' > "$cache/$QDRANT/refs/main"
expect_red "ref holding only a newline" "contains whitespace" "$cache"

cache="$(new_cache)"
: > "$cache/$QDRANT/refs/main"
expect_red "empty ref" "missing or empty" "$cache"

cache="$(new_cache)"
rm "$cache/$QDRANT/refs/main"
expect_red "missing ref" "missing or empty" "$cache"

# --- planted faults: snapshot files ---------------------------------------
cache="$(new_cache)"
rm "$cache/$QDRANT/snapshots/$REV/model.onnx"
expect_red "missing fp32 model" "model.onnx is missing or empty" "$cache"

cache="$(new_cache)"
rm "$cache/$XENOVA/snapshots/$REV/onnx/model_quantized.onnx"
expect_red "missing INT8 model" "model_quantized.onnx is missing or empty" "$cache"

cache="$(new_cache)"
ln -sf "$cache/no-such-blob" "$cache/$QDRANT/snapshots/$REV/tokenizer.json"
expect_red "dangling snapshot symlink" "tokenizer.json is missing or empty" "$cache"

# --- fail-closed arm ------------------------------------------------------
rc=0
out="$(env -u FASTEMBED_CACHE_DIR "$CHECK" 2>&1)" || rc=$?
if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -qF "is unset"; then
  echo "ok   unset FASTEMBED_CACHE_DIR refuses"
else
  fail "unset FASTEMBED_CACHE_DIR: wanted refusal, got exit $rc: $out"
fi

# --- workflow guards ------------------------------------------------------
# Lines that write refs/main with echo; comments cannot match the anchor.
echo_ref_writers() {
  grep -hE '^[[:space:]]*echo[[:space:]].*>[[:space:]]*"?[^[:space:]]*refs/main' "$@" || true
}

# The distinct fastembed cache keys, with the crate version left as written.
fastembed_keys() {
  { grep -hoE 'key=fastembed-[^"]*' "$@" || true; } | sort -u
}

planted="$WORK/planted.yml"
# shellcheck disable=SC2016  # the planted workflow line is literal text.
printf '%s\n' '          echo "${REV}" > "${DIR}/refs/main"' > "$planted"
if [ -n "$(echo_ref_writers "$planted")" ]; then
  echo "ok   the echo-writer guard sees a planted writer"
else
  fail "the echo-writer guard missed a planted writer"
fi

writers="$(echo_ref_writers "$WORKFLOWS"/*.yml)"
if [ -z "$writers" ]; then
  echo "ok   no workflow writes refs/main with echo"
else
  fail "a workflow writes refs/main with echo, which adds the newline hf-hub misses on: $writers"
fi

printf '%s\n' 'echo "key=fastembed-a" >> x' 'echo "key=fastembed-b" >> x' > "$planted"
if [ "$(fastembed_keys "$planted" | grep -c .)" -eq 2 ]; then
  echo "ok   the key guard sees a planted divergence"
else
  fail "the key guard missed a planted divergence"
fi

keys="$(fastembed_keys "$WORKFLOWS"/*.yml)"
if [ "$(printf '%s\n' "$keys" | grep -c .)" -eq 1 ]; then
  echo "ok   every workflow derives one fastembed key: $keys"
else
  fail "wanted exactly one fastembed cache key across workflows, got: ${keys:-none}"
fi

if [ "$failures" -ne 0 ]; then
  echo "check_fastembed_cache_selftest: $failures failure(s)"
  exit 1
fi
echo "check_fastembed_cache_selftest: all cases passed"

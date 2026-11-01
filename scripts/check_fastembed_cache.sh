#!/usr/bin/env bash
# check_fastembed_cache.sh — fail fast when hf-hub cannot read the restored
# fastembed model cache (#10639).
#
# Why: the fp32 embedder load (Qdrant/all-MiniLM-L6-v2-onnx) failing makes
# FastEmbedder fall back to INT8, and the reference-accuracy test then fails
# with a wrong-model error that hides the real cause. Two faults reach that
# state: a missing cache file, and a refs/main holding whitespace. hf-hub's
# `Cache::get` reads refs/main verbatim, so a trailing newline resolves to a
# snapshot path that does not exist. Every file then misses, parallel test
# processes all start the same download, and the loser's blob flock times out
# after 5 s.
# What: requires each model's refs/main to be whitespace-free, then requires
# every file fastembed loads to exist, non-empty, in snapshots/<rev>/. File
# lists come from fastembed 5.x `TextEmbedding::get_model_info` (model_code,
# model_file) and its tokenizer loader (tokenizer.json, config.json,
# special_tokens_map.json, tokenizer_config.json).
# Test: scripts/check_fastembed_cache_selftest.sh; CI runs this script after
# the cache restore/pre-seed.
set -euo pipefail

if [ -z "${FASTEMBED_CACHE_DIR:-}" ]; then
  echo "::error::FASTEMBED_CACHE_DIR is unset; refusing to guess a cache path"
  exit 1
fi
CACHE_DIR="${FASTEMBED_CACHE_DIR}"
TOKENIZER_FILES="tokenizer.json config.json special_tokens_map.json tokenizer_config.json"

# check_model <hf-repo> <model-file>
check_model() {
  local repo="$1" model_file="$2" dir ref rev snap f
  dir="${CACHE_DIR}/models--${repo//\//--}"
  ref="${dir}/refs/main"
  if [ ! -s "${ref}" ]; then
    echo "::error::fastembed cache incomplete: ${ref} is missing or empty (${repo})"
    exit 1
  fi
  rev=$(tr -d '[:space:]' < "${ref}")
  # #10639: compare byte counts. `$(cat ref)` would strip the trailing newline
  # this check exists to catch.
  if [ "$(wc -c < "${ref}")" -ne "${#rev}" ]; then
    echo "::error::fastembed cache unreadable by hf-hub: ${ref} contains whitespace (${repo})"
    exit 1
  fi
  snap="${dir}/snapshots/${rev}"
  for f in ${TOKENIZER_FILES} "${model_file}"; do
    # -s follows symlinks: a dangling blob link or empty file fails too.
    if [ ! -s "${snap}/${f}" ]; then
      echo "::error::fastembed cache incomplete: ${snap}/${f} is missing or empty (${repo})"
      exit 1
    fi
  done
  echo "fastembed cache ok: ${repo}@${rev}"
}

# fp32 default (AllMiniLML6V2) and INT8 fallback (AllMiniLML6V2Q).
check_model "Qdrant/all-MiniLM-L6-v2-onnx" "model.onnx"
check_model "Xenova/all-MiniLM-L6-v2" "onnx/model_quantized.onnx"

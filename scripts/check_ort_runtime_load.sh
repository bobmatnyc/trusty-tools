#!/usr/bin/env bash
#
# check_ort_runtime_load.sh — prove a built trusty-embedderd binary loads its
# ONNX Runtime at startup and produces one embedding (issue #8612).
#
# Why: the AL2023 `load-dynamic` CI job only compiled and linked, so it never
#   dlopen()ed `libonnxruntime.so`. `ort` 2.0.0-rc.12 (feature `api-24`)
#   refuses any runtime whose minor version is below 24, and the published
#   docs named ORT 1.20.1 without a gate noticing (#8612).
#
# What: pipes one JSON-RPC `embed` request into `<binary> --stdio`, then
#   closes stdin. The daemon loads ORT and the model before it reads the
#   request, answers it, and exits on EOF. Every run is bounded by
#   `timeout CHECK_TIMEOUT_SECS`: with `ort` rc.12 a runtime that fails to
#   load (missing, or older than 1.24) deadlocks the loader instead of
#   exiting — its error path re-enters the `OnceLock` it is initialising —
#   so a hang (exit 124) is the expected failure shape, not a flake.
#   - Default mode passes when the process exits 0 and the response carries
#     one embedding of EXPECTED_DIM floats (default 384, all-MiniLM-L6-v2).
#   - `--expect-load-failure` is the negative control: it passes only when the
#     process exits non-zero (error or timeout) and produced no embedding.
#     CI runs it against a missing ORT_DYLIB_PATH so the default mode is
#     shown unable to pass without a loadable runtime.
#   The runtime comes from the caller's environment (ORT_DYLIB_PATH for a
#   load-dynamic build); this script never sets it.
#
# Usage: scripts/check_ort_runtime_load.sh <trusty-embedderd-binary> [--expect-load-failure]
# Env:   EXPECTED_DIM (default 384), CHECK_TIMEOUT_SECS (default 600).
# Needs: `timeout` (GNU coreutils) — without it a failed load would hang forever.
#
# Test: run by the `al2023-load-dynamic-search` job in
#   .github/workflows/al2023-build.yml, in both modes.

set -uo pipefail

bin="${1:-}"
mode="${2:-}"
if [[ -z "${bin}" || ! -x "${bin}" ]]; then
  echo "usage: $0 <trusty-embedderd-binary> [--expect-load-failure]" >&2
  echo "error: '${bin}' is not an executable file" >&2
  exit 2
fi
if [[ -n "${mode}" && "${mode}" != "--expect-load-failure" ]]; then
  echo "error: unknown mode '${mode}'" >&2
  exit 2
fi
if ! command -v timeout >/dev/null 2>&1; then
  echo "error: 'timeout' (GNU coreutils) is required to bound a hung ORT load" >&2
  exit 2
fi

expected_dim="${EXPECTED_DIM:-384}"
limit="${CHECK_TIMEOUT_SECS:-600}"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

request='{"jsonrpc":"2.0","method":"embed","params":{"texts":["ort runtime load check"]},"id":1}'

echo "ORT_DYLIB_PATH=${ORT_DYLIB_PATH:-<unset>} (bound: ${limit}s)"
printf '%s\n' "${request}" \
  | timeout --kill-after=10 "${limit}" "${bin}" --stdio >"${work}/out" 2>"${work}/err"
status=$?
echo "exit status: ${status}"

show_stderr() {
  echo "--- last 20 lines of stderr ---"
  tail -n 20 "${work}/err"
}

# Float count of the first vector in `"embeddings":[[…]]` (commas + 1), or 0.
embedding_dim() {
  local vector
  vector="$(tr -d ' \n' <"${work}/out" | sed -n 's/.*"embeddings":\[\[\([^]]*\)\].*/\1/p')"
  if [[ -z "${vector}" ]]; then
    echo 0
  else
    echo $(( $(printf '%s' "${vector}" | tr -cd ',' | wc -c) + 1 ))
  fi
}

describe_failure() {
  if [[ ${status} -eq 124 || ${status} -eq 137 ]]; then
    echo "the daemon hung for ${limit}s before producing an embedding — the shape of" \
      "an ONNX Runtime load failure under ort 2.0.0-rc.12 (missing dylib, or older than 1.24)"
  else
    echo "the daemon exited ${status} before producing an embedding"
  fi
}

dim="$(embedding_dim)"

if [[ "${mode}" == "--expect-load-failure" ]]; then
  if [[ ${status} -eq 0 || "${dim}" -ne 0 ]]; then
    echo "FAIL: expected no embedding without a loadable runtime, got exit ${status} and a ${dim}-float vector" >&2
    show_stderr
    exit 1
  fi
  echo "PASS (negative control): $(describe_failure)"
  exit 0
fi

if [[ ${status} -ne 0 ]]; then
  echo "FAIL: $(describe_failure)" >&2
  show_stderr
  exit 1
fi
if [[ "${dim}" -ne "${expected_dim}" ]]; then
  echo "FAIL: expected one ${expected_dim}-float embedding, got ${dim} floats" >&2
  echo "--- stdout ---"
  cat "${work}/out"
  show_stderr
  exit 1
fi
echo "PASS: ONNX Runtime loaded and one ${dim}-float embedding was produced"
exit 0

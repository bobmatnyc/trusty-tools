#!/usr/bin/env bash
# Decide whether a change can affect the trusty-common embedder-cuda build.

set -euo pipefail

is_relevant_path() {
  case "$1" in
    Cargo.toml | \
      Cargo.lock | \
      rust-toolchain | \
      rust-toolchain.toml | \
      .cargo/* | \
      crates/trusty-common/* | \
      .github/workflows/ci.yml | \
      scripts/detect-embedder-cuda-relevant.sh)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

main() {
  local input
  if [[ -n "${CUDA_SCOPE_BASE:-}" ]]; then
    local merge_base
    if ! merge_base="$(git merge-base "$CUDA_SCOPE_BASE" HEAD)"; then
      echo "detect-embedder-cuda-relevant: cannot resolve merge-base against '${CUDA_SCOPE_BASE}'" >&2
      return 2
    fi
    input="$(git diff --name-only --no-renames "$merge_base" HEAD)"
  else
    input="$(cat)"
  fi

  local relevant=false
  local count=0
  local path
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    count=$((count + 1))
    if is_relevant_path "$path"; then
      echo "  cuda-relevant: $path" >&2
      relevant=true
    else
      echo "  cuda-inert   : $path" >&2
    fi
  done <<<"$input"

  # A missing diff must never suppress the specialized compile check.
  if ((count == 0)); then
    echo "detect-embedder-cuda-relevant: empty change set — running fail closed" >&2
    relevant=true
  fi

  echo "detect-embedder-cuda-relevant: ${count} changed path(s) -> embedder_cuda_relevant=${relevant}" >&2
  echo "embedder_cuda_relevant=${relevant}"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "embedder_cuda_relevant=${relevant}" >>"$GITHUB_OUTPUT"
  fi
}

main "$@"

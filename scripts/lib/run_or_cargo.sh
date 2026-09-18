#!/usr/bin/env bash
#
# run_or_cargo.sh — installed-binary-first, cargo-run-fallback runner shared by
# the thin "exec cargo run -p <crate> --bin <bin>" gate wrappers (issue #8021
# rollup item: doc gates queuing behind agent Rust builds).
#
# Why: check_sld.sh, check-version-parity.sh and check_capabilities.sh each
#   `exec cargo run` unconditionally. `cargo run` takes the workspace build
#   lock shared with every other cargo invocation on the machine, agent builds
#   included — a docs-only PR that needs no compile can queue behind one and
#   time out (owner-verified: check_sld.sh's 180s attempt timed out; sibling
#   doc checks took 218-338s). When the wrapped binary is already installed
#   (`cargo install`), running it directly takes no lock at all.
#
# What: run_or_cargo <bin-name> <crate> <cargo-bin-name> -- <args...>
#   - <bin-name> is already on PATH: exec it directly with <args...>. Fastest
#     path; takes no cargo lock. NOTE: this trusts whatever build put the
#     binary on PATH — for a self-referential drift check (check_capabilities.sh
#     comparing committed output against the CURRENT source), an installed
#     binary that predates this checkout's own changes can miss drift that a
#     fresh build of the checkout itself would catch. Callers checking a
#     crate's own generator output against its own uncommitted changes should
#     weigh that before relying on this fast path.
#   - Otherwise: `cargo run --locked` for <crate>'s <cargo-bin-name>, scoped to
#     a dedicated CARGO_TARGET_DIR (never the workspace's shared target/, so it
#     cannot contend with an agent's own build) and `--offline` when
#     `cargo metadata --offline` already resolves. A 180s timeout (GNU
#     `timeout`/`gtimeout` if present, else a portable bash watchdog) turns a
#     stuck build-lock wait into a named, non-zero-exit error instead of a
#     silent hang or a bare 124.
#   Reports which path it took in exactly one line on stderr.
#
# Usage (sourced):
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/run_or_cargo.sh"
#   run_or_cargo sld-lint trusty-sld-lint sld-lint -- --root "$REPO_ROOT" "$@"
#
# Exit: never returns on success or on the installed-binary path (execs into
#   the target process). On the cargo-run fallback, exits with the wrapped
#   command's own exit code, or 1 with a named error line on timeout.
#
# Test: manual verification only (shell-only helper, no cargo target of its
#   own); exercised indirectly by running check_sld.sh / check-version-parity.sh
#   / check_capabilities.sh in a tree with sld-lint/publish-guard absent from
#   PATH (their real state on a stock dev machine — see PR description).

TRUSTY_RUN_OR_CARGO_TIMEOUT_SECS="${TRUSTY_RUN_OR_CARGO_TIMEOUT_SECS:-180}"

# _run_or_cargo_bash_timeout <secs> <cmd...> — portable fallback for hosts with
# neither GNU `timeout` nor `gtimeout` (e.g. stock macOS system bash). Runs
# <cmd...> in the background, races a watchdog `sleep`, and SIGTERMs (then
# SIGKILLs) the child if the watchdog wins. Reports 124 on a timeout, exactly
# like GNU `timeout`, so the caller has one exit code to check regardless of
# which mechanism ran.
_run_or_cargo_bash_timeout() {
  local secs="$1"
  shift
  "$@" &
  local pid=$!
  (
    sleep "$secs"
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null
      sleep 1
      kill -KILL "$pid" 2>/dev/null
    fi
  ) &
  local watchdog=$!
  local rc=0
  wait "$pid" 2>/dev/null || rc=$?
  kill "$watchdog" 2>/dev/null
  wait "$watchdog" 2>/dev/null
  if [ "$rc" -eq 143 ] || [ "$rc" -eq 137 ]; then
    return 124
  fi
  return "$rc"
}

run_or_cargo() {
  local bin_name="$1" crate="$2" cargo_bin="$3"
  shift 3
  if [ "${1:-}" = "--" ]; then shift; fi

  local installed
  if installed="$(command -v "$bin_name" 2>/dev/null)"; then
    echo "run_or_cargo: using installed '${installed}' (PATH) — no cargo build lock taken" >&2
    exec "$installed" "$@"
  fi

  local target_dir="${TRUSTY_SCRIPTS_TARGET_DIR:-$HOME/.trusty-tools/cargo-target/scripts}"
  echo "run_or_cargo: '${bin_name}' not found on PATH; falling back to 'cargo run -p ${crate} --bin ${cargo_bin}' (CARGO_TARGET_DIR=${target_dir})" >&2

  local offline_flags=()
  if cargo metadata --offline --no-deps >/dev/null 2>&1; then
    offline_flags=(--offline)
  fi

  local tcmd=""
  if command -v timeout >/dev/null 2>&1; then
    tcmd="timeout"
  elif command -v gtimeout >/dev/null 2>&1; then
    tcmd="gtimeout"
  fi

  local rc=0
  if [ -n "$tcmd" ]; then
    CARGO_TARGET_DIR="$target_dir" "$tcmd" "$TRUSTY_RUN_OR_CARGO_TIMEOUT_SECS" \
      cargo run --quiet --locked "${offline_flags[@]}" -p "$crate" --bin "$cargo_bin" -- "$@"
    rc=$?
  else
    CARGO_TARGET_DIR="$target_dir" _run_or_cargo_bash_timeout "$TRUSTY_RUN_OR_CARGO_TIMEOUT_SECS" \
      cargo run --quiet --locked "${offline_flags[@]}" -p "$crate" --bin "$cargo_bin" -- "$@"
    rc=$?
  fi

  if [ "$rc" -eq 124 ]; then
    echo "FAIL: doc gate timed out waiting for the build lock; install ${bin_name} or set TRUSTY_SCRIPTS_TARGET_DIR" >&2
    exit 1
  fi
  exit "$rc"
}

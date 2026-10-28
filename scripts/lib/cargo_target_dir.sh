#!/usr/bin/env bash
# cargo_target_dir.sh — ask cargo which target directory it uses.
#
# Why: `${CARGO_TARGET_DIR:-<repo>/target}` is only a guess. The tm build lease
#   redirects a cargo run to a pool slot (`~/.trusty-tools/cargo-target-pool/
#   <org>/<repo>/slot-N/`, #8261), a `--target-dir` flag or a `build.target-dir`
#   config outranks the env var, and `cargo package` then writes its `.crate`
#   somewhere the guess never looks. Only cargo, run through the same cargo and
#   environment as the package step, knows. #10372 (the parity script's
#   `--vcs-info auto`) has the same defect and can source this file.
#
# What: defines `cargo_target_dir <repo-dir>`. It prints the `target_directory`
#   of `cargo metadata --format-version 1 --no-deps` run in <repo-dir> through
#   the `cargo` on PATH. Returns 1 with a reason on stderr when cargo fails or
#   the value is empty or not an existing path-shaped string. Never falls back
#   to a guessed path: a caller that cannot learn the directory must fail.
#
# Test: scripts/check-crate-size-selftest.sh (leased-dir cases).

cargo_target_dir() {
  local repo="$1" json dir
  json="$(cd "$repo" && cargo metadata --format-version 1 --no-deps 2>/dev/null)" || {
    echo "cargo_target_dir: 'cargo metadata' failed in ${repo}" >&2
    return 1
  }
  dir="$(printf '%s' "$json" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
  if [ -z "$dir" ]; then
    echo "cargo_target_dir: no target_directory in 'cargo metadata' output" >&2
    return 1
  fi
  printf '%s\n' "$dir"
}

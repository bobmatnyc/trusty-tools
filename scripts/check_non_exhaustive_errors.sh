#!/usr/bin/env bash
#
# check_non_exhaustive_errors.sh — every public error enum in trusty-memory
#   carries `#[non_exhaustive]` (issue #9273).
#
# Why: trusty-memory heads for 1.0 (#9271 plan, E5). An error enum without
#   `#[non_exhaustive]` makes every new variant a semver break, because a
#   downstream crate may match it exhaustively. Marking the existing enums is
#   a one-time change; this gate stops a NEW unmarked one from landing.
#
# What: lists the tracked `*.rs` files under SCAN_ROOT (test files excluded:
#   a `tests/` or `benches/` segment, or a basename `tests.rs` / `*_test.rs` /
#   `*_tests.rs`) and reads each `pub enum` with the attribute block above it.
#   The block is every contiguous `#[...]` attribute (multi-line ones too),
#   `//` comment and blank line directly above the `pub enum` line. The enum is
#   an ERROR enum when any of these holds:
#     - a `derive(...)` in the block names `Error` (bare or `thiserror::Error`);
#     - its name ends in `Error`;
#     - a scanned file holds `impl ... Error for <Name>` (a hand-written impl).
#   An error enum whose block has no `non_exhaustive` attribute is a failure,
#   printed as `path:line: pub enum <Name>`. `pub(crate)` and narrower enums
#   are not public and are not read.
#
#   Fails CLOSED. A `git ls-files` error is exit 2, and a scan over fewer than
#   NONEXH_MIN_FILES files is a SCAN FLOOR failure (#4618), never a pass over
#   nothing.
#
# Usage:
#   bash scripts/check_non_exhaustive_errors.sh
#   bash scripts/check_non_exhaustive_errors.sh --help
#
# Env (fixtures only):
#   NONEXH_MIN_FILES  scan floor (default 100)
#
# Exit: 0 clean; 1 on an unmarked public error enum or the scan floor;
#   2 on a usage or tool error.
#
# Test: scripts/check_non_exhaustive_errors_selftest.sh — an unmarked thiserror
#   derive, a path-qualified and a multi-line derive, a name-only and a
#   hand-written-impl error enum, the marked forms, a non-error enum, a
#   `pub(crate)` enum, a test file, a file outside the root, and an empty scan.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git, awk.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCAN_ROOT='crates/trusty-memory/src'

case "${1:-}" in
  -h|--help)
    sed -n '2,/^# Portability/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") ;;
  *)
    echo "check_non_exhaustive_errors: unknown argument '$1' (try --help)" >&2
    exit 2
    ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if ! listed="$(git ls-files -- "$SCAN_ROOT/*.rs")"; then
  echo "check_non_exhaustive_errors: git ls-files failed" >&2
  exit 2
fi
files="$(printf '%s\n' "$listed" | awk '
  NF == 0 { next }
  /(^|\/)(tests|benches)\// { next }
  /(^|\/)tests\.rs$/ || /_tests?\.rs$/ { next }
  { print }')"

min_files="${NONEXH_MIN_FILES:-100}"
scanned="$(printf '%s\n' "$files" | awk 'NF > 0' | wc -l | tr -d ' ')"
if [ "$scanned" -lt "$min_files" ]; then
  echo "FAIL: SCAN FLOOR — $scanned source files under $SCAN_ROOT, need at least $min_files"
  exit 1
fi

# Bash 3.2 has no mapfile; the file list holds no spaces (tracked Rust paths).
# shellcheck disable=SC2086
offending="$(awk '
  # Two passes over the same file list: pass 1 collects hand-written
  # `impl ... Error for <Name>` targets, pass 2 reads each `pub enum`.
  FNR == 1 { if (FILENAME == first) pass++; else if (first == "") { first = FILENAME; pass = 1 } }
  pass == 1 {
    if (match($0, /impl[^{]*(^|[^A-Za-z0-9_])Error for [A-Za-z_][A-Za-z0-9_:]*/)) {
      s = substr($0, RSTART, RLENGTH); sub(/.* for /, "", s); sub(/.*::/, "", s)
      manual[s] = 1
    }
    next
  }
  FNR == 1 { attrs = ""; in_attr = 0 }
  {
    t = $0; sub(/^[ \t]+/, "", t)
    # Count brackets outside string literals: `#[error("a [ b")]` is balanced.
    u = t; gsub(/"([^"\\]|\\.)*"/, "", u)
    if (in_attr) {
      attrs = attrs " " t; depth += gsub(/\[/, "[", u) - gsub(/\]/, "]", u)
      if (depth <= 0) in_attr = 0
      next
    }
    if (t ~ /^#\[/) {
      attrs = attrs " " t; depth = gsub(/\[/, "[", u) - gsub(/\]/, "]", u)
      if (depth > 0) in_attr = 1
      next
    }
    if (t == "" || t ~ /^\/\//) next
    if (t ~ /^pub enum [A-Za-z_]/) {
      name = t; sub(/^pub enum /, "", name); sub(/[^A-Za-z0-9_].*/, "", name)
      is_err = (name ~ /Error$/) || (name in manual) ||
               (attrs ~ /derive\(([^)]*[^A-Za-z0-9_])?Error[^A-Za-z0-9_]/)
      if (is_err && attrs !~ /#\[[ \t]*non_exhaustive[ \t]*\]/) print FILENAME ":" FNR ": pub enum " name
    }
    attrs = ""
  }' $files $files)"

if [ -n "$offending" ]; then
  echo "FAIL: a public error enum lacks #[non_exhaustive] (#9273). Without it a"
  echo "      new variant is a semver break for every exhaustive downstream match:"
  printf '%s\n' "$offending" | sed 's/^/  /'
  exit 1
fi
echo "OK: every public error enum under $SCAN_ROOT is #[non_exhaustive] ($scanned files)"

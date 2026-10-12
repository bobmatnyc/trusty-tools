#!/usr/bin/env bash
#
# check_engine_import_path.sh — consumers reach the memory engine through
#   `trusty_memory::engine`, never `trusty_common::memory_core` (issue #9270).
#
# Why: the engine is moving from trusty-common into trusty-memory (#9271).
#   trusty-memory re-exports it as `trusty_memory::engine`, so a consumer that
#   imports that path survives the move with no second import sweep. A consumer
#   that still names `trusty_common::memory_core` breaks when the module moves.
#   This gate is the #9270 acceptance criterion: once every consumer has moved,
#   it stops a new reference to the old path from landing.
#
#   The same issue keeps trusty-mpm's engine edge OPTIONAL: a default
#   trusty-mpm build must not compile trusty-memory or the engine's heavy
#   crates (owner ruling 2026-09-27). `--dep-graph` proves that from the resolved
#   dependency graph.
#
# What (default mode): `git grep` for the literal `trusty_common::memory_core`
#   over tracked files under `crates/`, excluding `crates/trusty-common/` (the
#   module's home), `crates/trusty-memory/` (the re-export) and every
#   `changelog.d/` directory (fragments quote the old path as history). A hit
#   on a comment line is not an import and is skipped: a line whose first
#   non-blank text is `//`, `/*`, `*`, or `#` followed by a space or the line
#   end. A Rust attribute (`#[`, `#![`) is code and is still reported. Every
#   other hit is a failure, printed as `path:line: text`.
#
#   Fails CLOSED. A `git grep` error is exit 2, and a scan over fewer than
#   ENGINE_PATH_MIN_FILES tracked files under `crates/` is a SCAN FLOOR
#   failure (#4618), never a pass over nothing.
#
# What (--dep-graph): runs `cargo tree -p trusty-mpm -e normal --prefix none`
#   twice, with default features and with `--features sm-memory,manager-memory`.
#   The default graph must name none of trusty-memory, hnsw_rs, fastembed, ort,
#   ort-sys or usearch (the engine's former index crate, kept on the list so it
#   cannot return unseen). The feature graph MUST name trusty-memory and
#   hnsw_rs (pure Rust, so present on every target): the same probe that
#   reports the default graph clean has to see the engine when it is enabled,
#   so an empty or broken tree cannot pass. Each tree must start with a
#   `trusty-mpm v` root line.
#
# Usage:
#   bash scripts/check_engine_import_path.sh              # source scan
#   bash scripts/check_engine_import_path.sh --dep-graph  # needs cargo
#   bash scripts/check_engine_import_path.sh --help
#
# Env (fixtures only):
#   ENGINE_PATH_MIN_FILES  source-scan floor (default 200)
#   ENGINE_TREE_DEFAULT    read this file as the default-feature `cargo tree`
#   ENGINE_TREE_FEATURES   read this file as the feature-enabled `cargo tree`
#
# Exit: 0 clean; 1 on any offending hit, forbidden or missing crate, or scan
#   floor; 2 on a usage or tool error.
#
# Test: scripts/check_engine_import_path_selftest.sh — a code hit, an inline
#   path, an attribute, comment-only hits, a changelog.d hit, hits inside the
#   two owning crates, an empty scan, and each --dep-graph verdict.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git, awk, grep.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

PATTERN='trusty_common::memory_core'

case "${1:-}" in
  -h|--help)
    sed -n '2,58p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  ""|--dep-graph) ;;
  *)
    echo "check_engine_import_path: unknown argument '$1' (try --help)" >&2
    exit 2
    ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# tree_text <fixture-env-value> <cargo args...>: the fixture file when set,
#   else the live `cargo tree` output. A cargo failure is a tool error.
tree_text() {
  local fixture="$1"
  shift
  if [ -n "$fixture" ]; then
    cat "$fixture"
    return
  fi
  if ! cargo tree -p trusty-mpm -e normal --prefix none --locked "$@"; then
    echo "check_engine_import_path: cargo tree $* failed" >&2
    exit 2
  fi
}

# has_crate <tree-text> <crate>: the tree names <crate> as a package line.
has_crate() {
  printf '%s\n' "$1" | grep -qE "^$2 v"
}

if [ "${1:-}" = "--dep-graph" ]; then
  default_tree="$(tree_text "${ENGINE_TREE_DEFAULT:-}")"
  feature_tree="$(tree_text "${ENGINE_TREE_FEATURES:-}" --features sm-memory,manager-memory)"
  fail=0
  for label in default features; do
    if [ "$label" = default ]; then tree="$default_tree"; else tree="$feature_tree"; fi
    if ! printf '%s\n' "$tree" | head -n 1 | grep -qE '^trusty-mpm v'; then
      echo "FAIL: the $label tree has no 'trusty-mpm v' root line (SCAN FLOOR)"
      fail=1
    fi
  done
  for crate in trusty-memory hnsw_rs fastembed ort ort-sys usearch; do
    if has_crate "$default_tree" "$crate"; then
      echo "FAIL: a default trusty-mpm build pulls in '$crate' (owner ruling 2026-09-27)"
      fail=1
    fi
  done
  for crate in trusty-memory hnsw_rs; do
    if ! has_crate "$feature_tree" "$crate"; then
      echo "FAIL: --features sm-memory,manager-memory does not pull in '$crate'"
      fail=1
    fi
  done
  if [ "$fail" -ne 0 ]; then exit 1; fi
  echo "OK: default trusty-mpm graph has no trusty-memory or engine crate; the memory features add trusty-memory and hnsw_rs"
  exit 0
fi

min_files="${ENGINE_PATH_MIN_FILES:-200}"
scanned="$(git ls-files -- 'crates/' | wc -l | tr -d ' ')"
if [ "$scanned" -lt "$min_files" ]; then
  echo "FAIL: SCAN FLOOR — $scanned tracked files under crates/, need at least $min_files"
  exit 1
fi

set +e
hits="$(git grep -n -I -F -e "$PATTERN" -- 'crates/' \
  ':(exclude)crates/trusty-common/' \
  ':(exclude)crates/trusty-memory/' \
  ':(exclude,glob)**/changelog.d/**')"
status=$?
set -e
if [ "$status" -gt 1 ]; then
  echo "check_engine_import_path: git grep failed (exit $status)" >&2
  exit 2
fi

# `path:line:text` -> keep only hits whose text is not a comment line.
offending="$(printf '%s\n' "$hits" | awk '
  NF == 0 { next }
  {
    rest = $0
    p = index(rest, ":"); rest = substr(rest, p + 1)
    p = index(rest, ":"); text = substr(rest, p + 1)
    if (text ~ /^[ \t]*(\/\/|\/\*|\*|#[ \t]|#$)/) next
    print
  }')"

if [ -n "$offending" ]; then
  echo "FAIL: '$PATTERN' outside crates/trusty-common and crates/trusty-memory."
  echo "      Import through 'trusty_memory::engine' instead (#9270):"
  printf '%s\n' "$offending" | sed 's/^/  /'
  exit 1
fi
echo "OK: no '$PATTERN' path outside crates/trusty-common and crates/trusty-memory ($scanned files under crates/)"

#!/usr/bin/env bash
#
# source_class.sh — the one definition of "crate source" and "test file" that
# the release-adjacent gates share (issue #5765).
#
# Why: two gates that run on nearly every PR classified the SAME path
# differently, and nothing said which was meant to govern.
# `scripts/check-pr-version-bump.sh` matched `^crates/[^/]+/src/` with no test
# exemption; `scripts/check_changelog_fragment.sh` carried its own copy of a
# test-file match and exempted those paths. PR #5764 edited
# `crates/trusty-review/src/report/reporter_tests.rs` and hit both readings at
# once: the version-bump gate demanded a 0.17.0 -> 0.18.0 bump while the
# changelog gate said no fragment was owed. Neither answer is wrong on its own.
# What was wrong is that the two definitions lived in two places and could drift
# without anyone deciding they should.
#
# What: three predicates, and the ruling on which gate applies which.
#
#   is_test_path <path>       test / benchmark / testdata file
#   is_crate_src_path <path>  crates/<crate>/src/**, exactly one level under
#                             crates/
#   crate_of_src_path <path>  the <crate> of such a path, on stdout
#
# THE RULING (#5765). The two gates ask different questions, so they apply
# different predicates — deliberately, and stated here rather than implied by
# two independent regexes:
#
#   check-pr-version-bump.sh  applies is_crate_src_path ALONE. A crates.io
#                             tarball ships a crate's test files, so editing one
#                             under an already-published version is exactly the
#                             drift that gate exists to catch. A test-only edit
#                             legitimately earns a version bump.
#   check_changelog_fragment.sh  applies is_crate_src_path AND is_test_path. A
#                             fragment describes a user-visible change, and a
#                             test-only edit has none to describe.
#
# So a test file under src/ IS crate source and is NOT a user-visible change.
# Both gates now say that in the same words.
#
# SCOPE, honestly. This file holds the definitions, not every path each gate
# reaches. `check_changelog_fragment.sh` additionally attributes NESTED crate
# members (`crates/trusty-audit/ui/src-tauri/src/**`) by walking up to the
# nearest Cargo.toml (#4576); `check-pr-version-bump.sh` deliberately does not,
# because `cargo package` excludes a nested package from its parent's tarball,
# so a nested edit is not drift against the parent's published version. That
# difference is about REACH and stays in the gates. `is_crate_src_path` is the
# depth-1 shape both agree on.
#
# `scripts/check_line_cap.sh` keeps its own `cap_for_path`, which is not this
# question: it decides which SLOC cap a file gets, has no `testdata/` arm, and
# runs against every tracked `.rs` file rather than a diff.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). Parameter
# expansion and `case` only — no external commands, so this is cheap to call
# once per changed path.
#
# Usage: `. "$(dirname "${BASH_SOURCE[0]}")/lib/source_class.sh"` from a script
# in scripts/. Defines functions only; sets no variables and runs nothing.
#
# Test: scripts/check_source_class_selftest.sh, which also asserts that both
# gates still source this file rather than growing a private copy again.

# is_test_path <path> — 0 when the path is a test, benchmark, or testdata file.
#
# A file qualifies when its basename is `tests.rs`, ends `_test.rs` or
# `_tests.rs`, or when any directory segment is `tests/`, `benches/`, or
# `testdata/`. Language-agnostic on purpose: the changelog gate sees non-Rust
# fixtures under `testdata/` too.
is_test_path() {
  local path="$1" base
  base="${path##*/}"
  case "$base" in
    tests.rs | *_test.rs | *_tests.rs) return 0 ;;
  esac
  case "$path" in
    */tests/* | */benches/* | */testdata/*) return 0 ;;
  esac
  return 1
}

# is_crate_src_path <path> — 0 when the path is `crates/<crate>/src/<...>`.
#
# `case` globs let `*` span `/`, which is how a nested member's source once
# slipped through a pattern meant for depth 1 (#4576). The check is written as
# a strip-and-compare so exactly one directory level sits under `crates/`.
is_crate_src_path() {
  local path="$1" rest crate sub
  case "$path" in
    crates/*) rest="${path#crates/}" ;;
    *) return 1 ;;
  esac
  crate="${rest%%/*}"
  # Empty means `crates//…`; equal means `crates/<name>` with nothing under it.
  [ -n "$crate" ] || return 1
  [ "$crate" != "$rest" ] || return 1
  sub="${rest#*/}"
  case "$sub" in
    src/?*) return 0 ;;
  esac
  return 1
}

# crate_of_src_path <path> — prints the crate directory name, or fails.
crate_of_src_path() {
  local path="$1" rest
  is_crate_src_path "$path" || return 1
  rest="${path#crates/}"
  printf '%s\n' "${rest%%/*}"
}

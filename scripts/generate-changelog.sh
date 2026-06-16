#!/usr/bin/env bash
# scripts/generate-changelog.sh
#
# Why: trusty-tools releases each crate independently (tag `<crate>-v<version>`).
# git-cliff drives per-crate CHANGELOG.md sections from conventional commits, but
# the per-crate scoping (which commits belong to which crate, and which tag series
# to walk) is supplied at invocation time. This wrapper encodes the correct
# --include-path / --tag-pattern pair for each in-scope crate so contributors do
# not have to remember them — especially the trusty-git-analytics gotcha (its tag
# prefix is `trusty-git-analytics`, NOT its crate name `tga`).
#
# What: Runs `git cliff` against the root cliff.toml, scoped to one crate via
# `--include-path crates/<crate-dir>/** --tag-pattern <tag-prefix>-v* --unreleased`,
# then either prepends the generated section to crates/<crate-dir>/CHANGELOG.md
# (forward-only — existing history is never rewritten) or prints it to stdout.
#
# Test: `bash -n scripts/generate-changelog.sh` for syntax; functionally,
#   scripts/generate-changelog.sh trusty-search trusty-search --stdout
#   should emit the unreleased trusty-search section without touching any file.
#
# Usage:
#   scripts/generate-changelog.sh <crate-dir> <tag-prefix> [--stdout]
#
# The 8 in-scope (release-workflow) crates — <crate-dir> <tag-prefix> pairs:
#   trusty-search        trusty-search
#   trusty-memory        trusty-memory
#   trusty-analyze       trusty-analyze
#   trusty-review        trusty-review
#   trusty-mpm           trusty-mpm
#   trusty-console       trusty-console
#   trusty-git-analytics trusty-git-analytics   <-- dir is trusty-git-analytics,
#                                                    crate name is `tga`, but the
#                                                    release TAG prefix is
#                                                    trusty-git-analytics (NOT tga).
#   trusty-code          trusty-code

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  echo "Usage: scripts/generate-changelog.sh <crate-dir> <tag-prefix> [--stdout]" >&2
  echo "" >&2
  echo "  <crate-dir>   directory under crates/ (e.g. trusty-search)" >&2
  echo "  <tag-prefix>  release tag prefix without -v (e.g. trusty-git-analytics)" >&2
  echo "  --stdout      print to stdout instead of prepending to the CHANGELOG" >&2
  exit 2
}

[[ $# -lt 2 ]] && usage

CRATE_DIR="$1"
TAG_PREFIX="$2"
MODE="${3:-prepend}"

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "ERROR: git-cliff not found on PATH. Install it with: cargo install git-cliff --locked" >&2
  exit 1
fi

CONFIG="${WORKSPACE_ROOT}/cliff.toml"
CRATE_PATH="${WORKSPACE_ROOT}/crates/${CRATE_DIR}"

if [[ ! -d "${CRATE_PATH}" ]]; then
  echo "ERROR: crate directory not found: crates/${CRATE_DIR}" >&2
  exit 1
fi

# Common git-cliff args: scope commits to this crate's tree, walk only this
# crate's tag series, and emit only the unreleased (since-last-tag) section.
CLIFF_ARGS=(
  --config "${CONFIG}"
  --include-path "crates/${CRATE_DIR}/**"
  --tag-pattern "${TAG_PREFIX}-v*"
  --unreleased
)

case "${MODE}" in
  --stdout)
    # Release-notes mode: strip the Keep-a-Changelog header/footer.
    ( cd "${WORKSPACE_ROOT}" && git-cliff "${CLIFF_ARGS[@]}" --strip all )
    ;;
  prepend)
    CHANGELOG="${CRATE_PATH}/CHANGELOG.md"
    # --prepend prepends the new section ABOVE existing history (forward-only;
    # hand-written entries are never regenerated or rewritten).
    ( cd "${WORKSPACE_ROOT}" && git-cliff "${CLIFF_ARGS[@]}" --prepend "${CHANGELOG}" )
    echo "Prepended unreleased section to crates/${CRATE_DIR}/CHANGELOG.md" >&2
    ;;
  *)
    usage
    ;;
esac

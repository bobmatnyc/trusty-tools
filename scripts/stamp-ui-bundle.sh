#!/usr/bin/env bash
#
# stamp-ui-bundle.sh — record which UI source a freshly-built bundle came from
# (#3606).
#
# Why: scripts/check-ui-bundle-freshness.sh needs something in the bundle that
#   only a rebuild produces. Commit ancestry was not that: any commit touching
#   any byte under the bundle directory cleared the gate whether or not a build
#   had run. A digest of the source, written by the build, cannot be cleared by
#   editing the bundle.
#
# What: computes the digest of the crate's bundle-affecting UI source from the
#   WORKING TREE — which is what a build just consumed, committed or not — and
#   writes it to <bundle_dir>/ui-source-hash.txt. Run it immediately after
#   building the bundle, never on its own; the file is a claim about a build
#   that happened.
#
#   trusty-search's `make sync-ui` calls it, so `make release-prep` covers that
#   crate end to end. The other three commit ui/dist/ straight out of
#   `pnpm build` with no make target in between, so there it is a second command
#   — which the gate's failure message names when it fires.
#
# Usage:
#   scripts/stamp-ui-bundle.sh <crate-name-or-dir>   # after building the bundle
#   scripts/stamp-ui-bundle.sh --all                 # every manifest row
#
# Exit codes: 0 = stamped. 1 = the crate has no manifest row, or its source tree
#   is empty (nothing to stamp). 2 = usage error.
#
# Test: scripts/check-ui-bundle-freshness-selftest.sh case 21 stamps a fixture
#   and asserts the gate then passes; case 22 asserts a re-stamp after a
#   comment-only source edit changes the file, which is what gives the
#   byte-identical-rebuild case a remedy that produces a commit.

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h | --help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/ui_source_digest.sh
. "${SCRIPT_DIR}/lib/ui_source_digest.sh"

REPO_ROOT=""
TARGET=""
ALL=0

usage() {
  echo "usage: scripts/stamp-ui-bundle.sh [--repo <path>] (<crate-name-or-dir> | --all)" >&2
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      [ $# -ge 2 ] || usage
      REPO_ROOT="$2"
      shift 2
      ;;
    --all)
      ALL=1
      shift
      ;;
    -*)
      echo "stamp-ui-bundle: unknown argument: $1" >&2
      usage
      ;;
    *)
      [ -n "$TARGET" ] && usage
      TARGET="$1"
      shift
      ;;
  esac
done

[ "$ALL" -eq 0 ] && [ -z "$TARGET" ] && usage

if [ -z "$REPO_ROOT" ]; then
  REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
fi
REPO_ROOT="$(cd "$REPO_ROOT" && pwd)"
MANIFEST="${REPO_ROOT}/scripts/ui-bundle-manifest.tsv"

if [ ! -f "$MANIFEST" ]; then
  echo "stamp-ui-bundle: ERROR: ${MANIFEST} does not exist" >&2
  exit 1
fi

# Accept the crates.io package name or the crates/ directory name, the same way
# preflight-publish.sh and check-ui-bundle-freshness.sh do.
resolve_crate_dir() {
  local input="$1"
  if [ -f "${REPO_ROOT}/crates/${input}/Cargo.toml" ]; then
    echo "$input"
    return 0
  fi
  local manifest dir
  for manifest in "${REPO_ROOT}"/crates/*/Cargo.toml; do
    [ -f "$manifest" ] || continue
    if grep -qE "^name[[:space:]]*=[[:space:]]*\"${input}\"" "$manifest"; then
      dir="$(basename "$(dirname "$manifest")")"
      echo "$dir"
      return 0
    fi
  done
  echo "$input"
}

[ -n "$TARGET" ] && TARGET="$(resolve_crate_dir "$TARGET")"

ROWS="$(grep -v '^[[:space:]]*#' "$MANIFEST" | grep -v '^[[:space:]]*$' || true)"
STAMPED=0

while IFS="$(printf '\t')" read -r crate src_dir bundle_dir _rest; do
  [ -z "${crate:-}" ] && continue
  [ "$ALL" -eq 0 ] && [ "$crate" != "$TARGET" ] && continue

  if [ ! -d "${REPO_ROOT}/${bundle_dir}" ]; then
    echo "stamp-ui-bundle: ERROR: ${bundle_dir} does not exist — build the bundle first" >&2
    exit 1
  fi

  pair="$(ui_source_digest "$REPO_ROOT" --worktree "$src_dir" "$bundle_dir")" || {
    echo "stamp-ui-bundle: ERROR: ${crate}: no bundle-affecting source files under ${src_dir}" >&2
    exit 1
  }
  digest="${pair%% *}"
  count="${pair##* }"
  stamp="${REPO_ROOT}/$(ui_stamp_path "$bundle_dir")"

  {
    echo "# Digest of the UI source this bundle was built from (#3606)."
    echo "# Written by scripts/stamp-ui-bundle.sh — regenerate by rebuilding, never by hand."
    echo "# Verified by scripts/check-ui-bundle-freshness.sh (preflight-publish.sh CHECK 7)."
    echo "${digest} ${count}"
  } > "$stamp"

  echo "stamp-ui-bundle: ${crate} — ${digest} over ${count} source file(s) -> $(ui_stamp_path "$bundle_dir")" >&2
  STAMPED=$((STAMPED + 1))
done <<EOF_ROWS
$ROWS
EOF_ROWS

if [ "$STAMPED" -eq 0 ]; then
  echo "stamp-ui-bundle: ERROR: no manifest row for '${TARGET}' in ${MANIFEST}" >&2
  exit 1
fi
exit 0

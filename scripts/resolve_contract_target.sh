#!/usr/bin/env bash
#
# resolve_contract_target.sh — the crate a pre-publish.yml run names.
#
# Why: #9539. Gates 5-6 hardcoded trusty-common, so a pre-publish run for any
#   other crate checked trusty-common's contracts under that crate's name.
#   Gate 6 now takes the crate, its version and its contracts.json path from
#   here. Gate 5 checks every crates/*/contracts.json and calls this only so
#   an unknown crate input fails (exit 1); it ignores 10 and 11.
#
# What: resolves the crate in this order, and never defaults to trusty-common:
#     1. INPUT_CRATE, when set (workflow_dispatch's `crate` input);
#     2. otherwise, when REF_TYPE is `tag`, the crate parsed from a
#        `<crate>-v<major>.<minor>.<patch>[-pre][+build]` REF_NAME;
#     3. otherwise, unresolved — a loud SKIP.
#   On exit 0 and 10 it prints three lines on stdout:
#     crate=<name>
#     version=<[package] version from crates/<name>/Cargo.toml>
#     contracts=crates/<name>/contracts.json
#   Annotations (::warning:: / ::error::) go to stderr, labelled with --gate.
#
# Usage: INPUT_CRATE=… REF_TYPE=… REF_NAME=… \
#          bash scripts/resolve_contract_target.sh --gate <label>
# Exit:
#   0   resolved, and the crate has a contracts.json
#   1   unknown crate: no crates/<name>/Cargo.toml, or not a crate name (error)
#   2   usage error
#   10  resolved, but the crate has no contracts.json (SKIP naming the crate)
#   11  the crate could not be resolved (SKIP)
#
# Test: `scripts/contract_target_selftest.sh`.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

GATE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --gate)
      [[ $# -lt 2 ]] && { echo "ERROR: --gate needs a label" >&2; exit 2; }
      GATE="$2"
      shift 2
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done
[[ -z "$GATE" ]] && { echo "ERROR: give --gate <label>" >&2; exit 2; }

INPUT_CRATE="${INPUT_CRATE:-}"
REF_TYPE="${REF_TYPE:-}"
REF_NAME="${REF_NAME:-}"

TAG_RE='^([A-Za-z0-9_-]+)-v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]*)?$'

crate=""
if [[ -n "$INPUT_CRATE" ]]; then
  crate="$INPUT_CRATE"
elif [[ "$REF_TYPE" == "tag" && "$REF_NAME" =~ $TAG_RE ]]; then
  crate="${BASH_REMATCH[1]}"
else
  {
    echo "::warning::[SKIP] ${GATE}: the crate to check could not be resolved — no 'crate' input, and ref '${REF_NAME}' (${REF_TYPE:-no ref type}) is not a <crate>-v<semver> tag."
    echo "  RECORDED SKIP, not a pass: no crate's contracts were checked."
    echo "  Re-run via workflow_dispatch with the 'crate' input set."
  } >&2
  exit 11
fi

if [[ ! "$crate" =~ ^[A-Za-z0-9_-]+$ || ! -f "${REPO_ROOT}/crates/${crate}/Cargo.toml" ]]; then
  echo "::error::${GATE}: unknown crate '${crate}' — crates/${crate}/Cargo.toml does not exist in this tree." >&2
  exit 1
fi

version="$(awk -F'"' '/^\[package\]$/ { p = 1; next } /^\[/ { p = 0 }
                      p && /^version[[:space:]]*=/ { print $2; exit }' \
  "${REPO_ROOT}/crates/${crate}/Cargo.toml")"
if [[ -z "$version" ]]; then
  echo "::error::${GATE}: crates/${crate}/Cargo.toml has no [package] version." >&2
  exit 1
fi

contracts="crates/${crate}/contracts.json"
echo "crate=${crate}"
echo "version=${version}"
echo "contracts=${contracts}"

if [[ ! -f "${REPO_ROOT}/${contracts}" ]]; then
  {
    echo "::warning::[SKIP] ${GATE}: ${crate} ${version} has no ${contracts}."
    echo "  RECORDED SKIP, not a pass: ${crate} publishes no Code Contracts artifact, so there is nothing to check."
  } >&2
  exit 10
fi
exit 0

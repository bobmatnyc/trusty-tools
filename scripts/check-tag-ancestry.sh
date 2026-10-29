#!/usr/bin/env bash
#
# check-tag-ancestry.sh — may this commit be released?
#
# Why: the release-ancestry rule (#2227: never publish from an unmerged feature
#   branch) was main-only, copied into release.yml, check-publish-ready.sh
#   GUARD 1/2 and preflight-publish.sh CHECK 1. Owner ruling D96 on #10388
#   (2026-10-28) widens it: a release may come from `main` OR from a
#   `release/<crate>-<semver>` branch. This script is the ONE copy of that rule;
#   every gate calls it.
# What: resolves <commit-ish> (tag, branch or SHA; an annotated tag is
#   dereferenced to its commit), fetches `main` and every `release/*` head from
#   <remote>, and passes when the commit is reachable from <remote>/main or from
#   a <remote>/release/* branch whose name matches RELEASE_BRANCH_RE below.
#   Fails closed on everything else: unresolvable commit, failed fetch, missing
#   <remote>/main, or no qualifying ref containing the commit.
#   --at-tip narrows "reachable from" to "equal to the tip of" (preflight CHECK 1).
# Test: scripts/check-tag-ancestry-selftest.sh
#
# Usage: check-tag-ancestry.sh [--repo DIR] [--remote NAME] [--no-fetch] [--at-tip] <commit-ish>
# Exit: 0 releasable, 1 not releasable (or fetch/ref failure), 2 usage error.

set -euo pipefail

# release/<crate>-<MAJOR>.<MINOR>.<PATCH>[-<pre-release>]
# <crate> = [a-z0-9]+(-[a-z0-9]+)*  (workspace crate names are [a-z0-9-]+)
# Rejects release/d66 (no semver) and release/2026-07-07-workspace (no semver tail).
RELEASE_BRANCH_RE='^release/[a-z0-9]+(-[a-z0-9]+)*-(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'

usage() {
  echo "usage: $0 [--repo DIR] [--remote NAME] [--no-fetch] [--at-tip] <commit-ish>" >&2
  exit 2
}

repo="."; remote="origin"; fetch=1; at_tip=0; target=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) [[ $# -ge 2 ]] || usage; repo="$2"; shift 2 ;;
    --remote) [[ $# -ge 2 ]] || usage; remote="$2"; shift 2 ;;
    --no-fetch) fetch=0; shift ;;
    --at-tip) at_tip=1; shift ;;
    --print-regex) echo "${RELEASE_BRANCH_RE}"; exit 0 ;;
    -h|--help) usage ;;
    -*) usage ;;
    *) [[ -z "${target}" ]] || usage; target="$1"; shift ;;
  esac
done
[[ -n "${target}" ]] || usage

g() { git -C "${repo}" "$@"; }
fail() { echo "check-tag-ancestry: FAIL — $*" >&2; exit 1; }

if [[ "${fetch}" -eq 1 ]]; then
  if ! g fetch --quiet "${remote}" \
      "+refs/heads/main:refs/remotes/${remote}/main" \
      "+refs/heads/release/*:refs/remotes/${remote}/release/*" 2>/dev/null; then
    fail "could not fetch main and release/* from '${remote}'; refusing to decide on stale refs."
  fi
fi

sha="$(g rev-parse --verify --quiet "${target}^{commit}" 2>/dev/null || true)"
[[ -n "${sha}" ]] || fail "'${target}' does not resolve to a commit in ${repo}."

main_sha="$(g rev-parse --verify --quiet "refs/remotes/${remote}/main^{commit}" 2>/dev/null || true)"
[[ -n "${main_sha}" ]] || fail "ref ${remote}/main is missing; cannot check '${target}' (${sha})."

checked=("${remote}/main")
contains() { # <ref-sha>
  if [[ "${at_tip}" -eq 1 ]]; then [[ "${sha}" == "$1" ]]; else g merge-base --is-ancestor "${sha}" "$1"; fi
}

if contains "${main_sha}"; then
  echo "check-tag-ancestry: PASS — '${target}' (${sha}) is on ${remote}/main (${main_sha})." >&2
  exit 0
fi

while IFS=' ' read -r ref_sha ref_name; do
  [[ -n "${ref_name}" ]] || continue
  branch="${ref_name#refs/remotes/"${remote}"/}"
  [[ "${branch}" =~ ${RELEASE_BRANCH_RE} ]] || continue
  checked+=("${remote}/${branch}")
  if contains "${ref_sha}"; then
    echo "check-tag-ancestry: PASS — '${target}' (${sha}) is on ${remote}/${branch} (${ref_sha}; D96, #10388)." >&2
    exit 0
  fi
done < <(g for-each-ref --format='%(objectname) %(refname)' "refs/remotes/${remote}/release/")

mode="reachable from"; [[ "${at_tip}" -eq 1 ]] && mode="the tip of"
fail "'${target}' (${sha}) is not ${mode} any releasable ref. Checked: ${checked[*]}. Release only from main or release/<crate>-<semver> (D96, #10388; #2227)."

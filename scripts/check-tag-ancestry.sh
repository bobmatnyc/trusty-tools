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
# What: resolves <commit-ish> (an annotated tag is dereferenced), fetches
#   `main` and every `release/*` head from <remote> with --prune, then:
#   - PASS when the commit is reachable from <remote>/main (unchanged rule);
#   - else PASS on a release branch only when ALL hold, given the release
#     identity (--tag <crate>-vX.Y.Z, or --crate C --version X.Y.Z):
#     (i) the branch matches RELEASE_BRANCH_RE;
#     (ii) `git show <commit>:crates/<crate>/Cargo.toml` [package] version == X.Y.Z;
#     (iii) the commit IS the tip of that branch.
#   Fails closed on everything else: unresolvable commit, failed fetch, missing
#   <remote>/main, no identity, unreadable Cargo.toml, no qualifying branch.
#   --at-tip narrows the main rule to "equal to the tip" (preflight CHECK 1).
# Test: scripts/check-tag-ancestry-selftest.sh
#
# Usage: check-tag-ancestry.sh [--repo DIR] [--remote NAME] [--no-fetch] [--at-tip]
#          [--tag <crate>-vX.Y.Z | --crate C --version X.Y.Z] <commit-ish>
# Exit: 0 releasable, 1 not releasable (or fetch/ref failure), 2 usage error.

set -euo pipefail

# release/<crate>-<MAJOR>.<MINOR>.<PATCH>[-<pre-release>]
# <crate> = [a-z0-9]+(-[a-z0-9]+)*  (workspace crate names are [a-z0-9-]+)
# Rejects release/d66 (no semver) and release/2026-07-07-workspace (no semver tail).
RELEASE_BRANCH_RE='^release/[a-z0-9]+(-[a-z0-9]+)*-(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'
SEMVER_RE='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?'

usage() {
  echo "usage: $0 [--repo DIR] [--remote NAME] [--no-fetch] [--at-tip] [--tag T | --crate C --version V] <commit-ish>" >&2
  exit 2
}

repo="."; remote="origin"; fetch=1; at_tip=0; target=""; tag=""; crate=""; version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) [[ $# -ge 2 ]] || usage; repo="$2"; shift 2 ;;
    --remote) [[ $# -ge 2 ]] || usage; remote="$2"; shift 2 ;;
    --tag) [[ $# -ge 2 ]] || usage; tag="$2"; shift 2 ;;
    --crate) [[ $# -ge 2 ]] || usage; crate="$2"; shift 2 ;;
    --version) [[ $# -ge 2 ]] || usage; version="$2"; shift 2 ;;
    --no-fetch) fetch=0; shift ;;
    --at-tip) at_tip=1; shift ;;
    --print-regex) echo "${RELEASE_BRANCH_RE}"; exit 0 ;;
    -h|--help|-*) usage ;;
    *) [[ -z "${target}" ]] || usage; target="$1"; shift ;;
  esac
done
[[ -n "${target}" ]] || usage
if [[ -n "${tag}" ]]; then
  [[ -z "${crate}${version}" ]] || usage
  if [[ "${tag}" =~ ^([a-z0-9]+(-[a-z0-9]+)*)-v(${SEMVER_RE})$ ]]; then
    crate="${BASH_REMATCH[1]}"; version="${BASH_REMATCH[3]}"
  else
    crate="?"; version="?"   # unparseable tag: the release-branch path fails below
  fi
fi
[[ -z "${crate}" && -z "${version}" ]] || [[ -n "${crate}" && -n "${version}" ]] || usage

g() { git -C "${repo}" "$@"; }
fail() { echo "check-tag-ancestry: FAIL — $*" >&2; exit 1; }

if [[ "${fetch}" -eq 1 ]]; then
  if ! g fetch --quiet --prune "${remote}" \
      "+refs/heads/main:refs/remotes/${remote}/main" \
      "+refs/heads/release/*:refs/remotes/${remote}/release/*" 2>/dev/null; then
    fail "could not fetch main and release/* from '${remote}'; refusing to decide on stale refs."
  fi
fi

sha="$(g rev-parse --verify --quiet "${target}^{commit}" 2>/dev/null || true)"
[[ -n "${sha}" ]] || fail "'${target}' does not resolve to a commit in ${repo}."

main_sha="$(g rev-parse --verify --quiet "refs/remotes/${remote}/main^{commit}" 2>/dev/null || true)"
[[ -n "${main_sha}" ]] || fail "ref ${remote}/main is missing; cannot check '${target}' (${sha})."

if { [[ "${at_tip}" -eq 1 ]] && [[ "${sha}" == "${main_sha}" ]]; } \
   || { [[ "${at_tip}" -eq 0 ]] && g merge-base --is-ancestor "${sha}" "${main_sha}"; }; then
  echo "check-tag-ancestry: PASS — '${target}' (${sha}) is on ${remote}/main (${main_sha})." >&2
  exit 0
fi

# Release-branch path (D96). Collect the qualifying branches (condition i).
checked=("${remote}/main"); tip_branch=""
while IFS=' ' read -r ref_sha ref_name; do
  [[ -n "${ref_name}" ]] || continue
  branch="${ref_name#refs/remotes/"${remote}"/}"
  [[ "${branch}" =~ ${RELEASE_BRANCH_RE} ]] || continue
  checked+=("${remote}/${branch}")
  [[ "${ref_sha}" == "${sha}" ]] && tip_branch="${branch}"
done < <(g for-each-ref --format='%(objectname) %(refname)' "refs/remotes/${remote}/release/")

ctx="'${target}' (${sha}); checked ${checked[*]} (D96, #10388)"
[[ "${#checked[@]}" -gt 1 ]] \
  || fail "(i) not on ${remote}/main and no release/<crate>-<semver> branch exists: ${ctx}."
[[ -n "${tip_branch}" ]] \
  || fail "(iii) not on ${remote}/main and not the TIP of any release/<crate>-<semver> branch: ${ctx}."
[[ -n "${crate}" ]] \
  || fail "(ii) on ${remote}/${tip_branch} but no release identity given (--tag or --crate/--version): ${ctx}."
[[ "${crate}" != "?" ]] \
  || fail "(ii) tag '${tag}' is not <crate>-vX.Y.Z: ${ctx}."
[[ "${crate}" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] || fail "(ii) crate '${crate}' is not a crate name: ${ctx}."

manifest="crates/${crate}/Cargo.toml"
toml="$(g show "${sha}:${manifest}" 2>/dev/null)" \
  || fail "(ii) cannot read ${manifest} at ${sha} (missing crate directory?): ${ctx}."
# First `version = "..."` inside the [package] table.
pkg_version="$(printf '%s\n' "${toml}" | awk '
  /^[[:space:]]*\[/ { inpkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*$/); next }
  inpkg && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
    sub(/^[^"]*"/, ""); sub(/".*$/, ""); print; exit }')"
[[ -n "${pkg_version}" ]] \
  || fail "(ii) no parseable [package] version in ${manifest} at ${sha}: ${ctx}."
[[ "${pkg_version}" == "${version}" ]] \
  || fail "(ii) ${manifest} at ${sha} says version ${pkg_version}, release wants ${version}: ${ctx}."

echo "check-tag-ancestry: PASS — '${target}' (${sha}) is the tip of ${remote}/${tip_branch}; ${manifest} version ${pkg_version} (D96, #10388)." >&2

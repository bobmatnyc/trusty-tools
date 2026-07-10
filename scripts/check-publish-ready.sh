#!/usr/bin/env bash
#
# check-publish-ready.sh — publish-only-from-merged-main preflight guard
# (issue #2227).
#
# Why: issue #2209 was caused by `cargo publish` running from an UNMERGED
#   feature branch. That branch was missing a P0 fix, so the publish made a
#   regressed build the crates.io "latest" version — every concurrent
#   session and fresh `cargo install` picked up the broken build until the
#   mistake was discovered and a follow-up release corrected it. With
#   multiple concurrent worktree sessions in this repo, "publish from
#   whatever branch happens to be checked out" is a correctness hazard, not
#   just a process nicety. This script turns "publish only from merged
#   main" from a documented convention into a mechanical gate that the
#   cargo-publish workflow runs BEFORE `cargo publish`.
#
# What: given a crate name or directory, resolves its Cargo.toml version and
#   release-tag prefix, then enforces two guards against the TRUE
#   `origin/main` tip (fetched fresh — never trusted from a possibly-stale
#   local `refs/remotes/origin/main`):
#
#     GUARD 1 (merged-main):  current HEAD must be origin/main itself or an
#       ancestor of it (`git merge-base --is-ancestor`). Publishing from a
#       commit that is not yet merged is refused.
#
#     GUARD 2 (version tag on main): the release tag `<prefix>-v<version>`
#       must exist on origin AND that tag's commit must be an ancestor of
#       origin/main. This confirms the version about to be published was
#       actually tagged and merged, not just bumped locally.
#
#   `ALLOW_UNMERGED_PUBLISH=1` downgrades both guard failures to a loud
#   WARN and exits 0 — a deliberate, explicit escape hatch for the rare
#   intentional case (e.g. republishing an already-merged historical tag
#   from a detached checkout where merge-base lookup is inconvenient).
#
# Usage:
#   scripts/check-publish-ready.sh <crate-name-or-dir>
#
# Examples:
#   scripts/check-publish-ready.sh trusty-mpm
#   scripts/check-publish-ready.sh tga                    # short package name
#   scripts/check-publish-ready.sh trusty-git-analytics   # directory name
#   ALLOW_UNMERGED_PUBLISH=1 scripts/check-publish-ready.sh trusty-mpm
#
# Test: see the PR description for this script (issue #2227) for raw
#   terminal output proving: (a) PASS from a merged+tagged commit,
#   (b) FAIL from an unmerged branch HEAD, (c) ALLOW_UNMERGED_PUBLISH=1
#   downgrades both failures to a warning and exits 0. No shell-test harness
#   exists in this repo (scripts/ has none); this is exercised manually.

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${WORKSPACE_ROOT}"

usage() {
  echo "Usage: scripts/check-publish-ready.sh <crate-name-or-dir>" >&2
  echo "" >&2
  echo "  <crate-name-or-dir>   crate package name (e.g. tga) OR the" >&2
  echo "                        directory under crates/ (e.g. trusty-git-analytics)" >&2
  echo "" >&2
  echo "Refuses to report ready unless HEAD is on merged origin/main and the" >&2
  echo "release tag <prefix>-v<version> exists there too. Escape hatch (rare," >&2
  echo "deliberate use only): ALLOW_UNMERGED_PUBLISH=1 scripts/check-publish-ready.sh <crate>" >&2
  exit 2
}

# Why: callers may pass either the crates.io package name (e.g. `tga`) or the
# crates/ directory name (e.g. `trusty-git-analytics`) — both are used
# interchangeably throughout .trusty-mpm/INSTRUCTIONS.md and the cargo-publish skill.
# What: resolves the input to a crate directory under crates/, trying it
# directly first, then falling back to a `name = "<input>"` grep across every
# crate manifest (handles the tga / trusty-git-analytics exception).
resolve_crate_dir() {
  local input="$1"
  if [[ -f "${WORKSPACE_ROOT}/crates/${input}/Cargo.toml" ]]; then
    echo "${input}"
    return 0
  fi
  local manifest dir
  for manifest in "${WORKSPACE_ROOT}"/crates/*/Cargo.toml; do
    if grep -qE "^name[[:space:]]*=[[:space:]]*\"${input}\"" "${manifest}"; then
      dir="$(basename "$(dirname "${manifest}")")"
      echo "${dir}"
      return 0
    fi
  done
  return 1
}

# Why: mirrors read_package_version() in scripts/bump-version.sh — anchor on
# the FIRST `version = "X.Y.Z"` line so a dependency pin is never matched.
# What: prints the package version from a crate's Cargo.toml, or fails.
read_package_version() {
  local manifest="$1"
  local version
  version="$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' "${manifest}" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
  if [[ -z "${version}" ]]; then
    echo "ERROR: could not find a package version (version = \"X.Y.Z\") in ${manifest}" >&2
    return 1
  fi
  echo "${version}"
}

# Why: the tag prefix equals the crate directory name for every current
# crate EXCEPT trusty-git-analytics, whose tags are `trusty-git-analytics-v*`
# (never `tga-v*`), matching bump-version.sh's tag_prefix_for() and the
# .trusty-mpm/INSTRUCTIONS.md abbreviations table.
# What: prints the canonical tag prefix for a given crate directory.
tag_prefix_for() {
  echo "$1"
}

# Why: resolves the true origin/main tip fresh every run. The `.base` bare
# repo (and possibly other checkouts) can carry a STALE
# refs/remotes/origin/main that silently diverges from what's actually on
# GitHub — trusting it would make this guard meaningless. `git ls-remote`
# always talks to the remote directly.
# What: prints the origin/main commit SHA and ensures it exists locally as a
# fetched object (so merge-base can reason about it).
resolve_true_origin_main() {
  local sha
  sha="$(git ls-remote origin refs/heads/main | cut -f1)"
  if [[ -z "${sha}" ]]; then
    echo "ERROR: could not resolve origin/main via 'git ls-remote origin refs/heads/main'" >&2
    return 1
  fi
  git fetch origin "${sha}" --quiet 2>/dev/null || git fetch origin main --quiet
  if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
    echo "ERROR: origin/main (${sha}) is not a local object after fetch" >&2
    return 1
  fi
  echo "${sha}"
}

main() {
  [[ $# -ne 1 ]] && usage
  local input="$1"

  local crate_dir
  if ! crate_dir="$(resolve_crate_dir "${input}")"; then
    echo "ERROR: no crate found matching '${input}' (checked crates/${input}/ and every" >&2
    echo "       crate's 'name = ' field)" >&2
    exit 1
  fi

  local manifest="${WORKSPACE_ROOT}/crates/${crate_dir}/Cargo.toml"
  local version
  version="$(read_package_version "${manifest}")"

  local prefix
  prefix="$(tag_prefix_for "${crate_dir}")"
  local tag="${prefix}-v${version}"

  # tga alias (issue #1128): also accept the short `tga-v<version>` form.
  local tag_alt=""
  if [[ "${crate_dir}" == "trusty-git-analytics" ]]; then
    tag_alt="tga-v${version}"
  fi

  echo "check-publish-ready: crate=${crate_dir} version=${version} tag=${tag}" >&2

  local origin_main_sha
  if ! origin_main_sha="$(resolve_true_origin_main)"; then
    exit 1
  fi
  echo "check-publish-ready: true origin/main = ${origin_main_sha}" >&2

  local override_active=0
  if [[ "${ALLOW_UNMERGED_PUBLISH:-0}" == "1" ]]; then
    override_active=1
  fi

  local failures=0

  # GUARD 1: current HEAD must be origin/main or an ancestor of it.
  local head_sha
  head_sha="$(git rev-parse HEAD)"
  if git merge-base --is-ancestor "${head_sha}" "${origin_main_sha}"; then
    echo "PASS: GUARD 1 (merged-main) — HEAD (${head_sha}) is on origin/main" >&2
  else
    if [[ "${override_active}" -eq 1 ]]; then
      echo "WARN: GUARD 1 (merged-main) FAILED but ALLOW_UNMERGED_PUBLISH=1 — publishing" >&2
      echo "      from an unmerged commit. This is a deliberate escape hatch; make sure" >&2
      echo "      you understand issue #2209 before relying on it in normal workflow." >&2
    else
      echo "FAIL: GUARD 1 (merged-main) — HEAD (${head_sha}) is NOT an ancestor of" >&2
      echo "      origin/main (${origin_main_sha}). Publishing from an unmerged commit is" >&2
      echo "      forbidden: issue #2209 published from an unmerged branch, making a" >&2
      echo "      P0-missing build the crates.io 'latest' for every concurrent session." >&2
      echo "      Merge this branch to main first, then publish from a checkout of" >&2
      echo "      merged main. Escape hatch: ALLOW_UNMERGED_PUBLISH=1 (deliberate use only)." >&2
      failures=$((failures + 1))
    fi
  fi

  # GUARD 2: the release tag <prefix>-v<version> must exist on origin AND be
  # an ancestor of origin/main.
  local tag_sha=""
  local matched_tag=""
  local candidate
  for candidate in "${tag}" "${tag_alt}"; do
    [[ -z "${candidate}" ]] && continue
    tag_sha="$(git ls-remote --tags origin "refs/tags/${candidate}" | cut -f1)"
    if [[ -n "${tag_sha}" ]]; then
      matched_tag="${candidate}"
      break
    fi
  done

  if [[ -z "${tag_sha}" ]]; then
    if [[ "${override_active}" -eq 1 ]]; then
      echo "WARN: GUARD 2 (version tag) FAILED but ALLOW_UNMERGED_PUBLISH=1 — no pushed" >&2
      echo "      tag '${tag}' found on origin. Publishing anyway per override." >&2
    else
      echo "FAIL: GUARD 2 (version tag) — no pushed tag '${tag}' found on origin" >&2
      echo "      (checked 'git ls-remote --tags origin'). Bump, commit, merge, tag, and" >&2
      echo "      push before publishing: 'git tag ${tag} && git push origin ${tag}'." >&2
      failures=$((failures + 1))
    fi
  else
    git fetch origin "${tag_sha}" --quiet 2>/dev/null || true
    if git cat-file -e "${tag_sha}^{commit}" 2>/dev/null \
      && git merge-base --is-ancestor "${tag_sha}" "${origin_main_sha}"; then
      echo "PASS: GUARD 2 (version tag) — '${matched_tag}' (${tag_sha}) is on origin/main" >&2
    else
      if [[ "${override_active}" -eq 1 ]]; then
        echo "WARN: GUARD 2 (version tag) FAILED but ALLOW_UNMERGED_PUBLISH=1 — tag" >&2
        echo "      '${matched_tag}' exists but is not on merged origin/main. Publishing" >&2
        echo "      anyway per override." >&2
      else
        echo "FAIL: GUARD 2 (version tag) — tag '${matched_tag}' (${tag_sha}) exists but is" >&2
        echo "      NOT an ancestor of origin/main (${origin_main_sha}). The tagged commit" >&2
        echo "      must be merged to main before publishing." >&2
        failures=$((failures + 1))
      fi
    fi
  fi

  if [[ "${failures}" -gt 0 ]]; then
    echo "check-publish-ready: FAILED (${failures} guard(s) tripped) — do not run 'cargo publish'." >&2
    exit 1
  fi

  echo "check-publish-ready: OK — ${crate_dir} ${version} is safe to publish (merged main, tag present)." >&2
  exit 0
}

main "$@"

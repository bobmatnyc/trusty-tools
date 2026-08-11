#!/usr/bin/env bash
#
# detect-version-bumps.sh — which crates does this change set declare a NEW
# version for? (issue #5311)
#
# Why: `.github/workflows/semver-checks.yml` fired on tag pushes only, so on a
#   pull request the `Public API / SemVer` context did not exist at all. A
#   required branch-protection context that never reports leaves every PR
#   permanently pending, which is why #5311 could not be closed by a
#   required-checks toggle. The workflow now runs on pull requests too — and
#   this script is what tells it whether there is anything to compare, so the
#   no-op path is a fact about the DIFF rather than about the event type
#   (#5407, the defect that shape produced in ci.yml).
#
#   The predicate is "a crate declares a version this branch did not start
#   with", not "a crate's source changed", and #5149 is why. Installing the
#   pinned `cargo-semver-checks` and warming a cold `target/semver-checks`
#   cache costs 20+ minutes; paying that on every PR that touches Rust is the
#   cost #5149 removed, and this must not restore it. A SemVer break becomes a
#   defect when something is PUBLISHED, and in this workspace the version bump
#   that precedes a publish lands in its own PR before the `<crate>-v<version>`
#   tag is cut. Keying on the bump therefore costs an ordinary PR one git diff,
#   and moves the verdict for a release PR one step earlier than the tag push —
#   while the release can still be reworked without a tag in flight.
#
# What: resolves the merge base against $VERSION_BUMP_BASE, then for every
#   changed `crates/<dir>/Cargo.toml` compares the `[package]` table's `version`
#   at the merge base against the one at HEAD. Emits, on stdout and to
#   $GITHUB_OUTPUT when set:
#     bumped=true|false
#     bumped_crates=<space-separated crates/ DIRECTORY names>
#
#   Directory names, not package names, are deliberate: `check_semver.sh
#   --crate` accepts either form (#1128), so no alias table is duplicated here.
#
#   Decision table:
#     manifest absent at the merge base -> BUMPED (a crate this branch adds;
#                                          check_semver.sh then records the
#                                          "never published" skip itself)
#     manifest absent at HEAD           -> not bumped (deleted by this branch)
#     `version.workspace = true` / no
#       readable `[package]` version    -> not bumped (nothing is declared here)
#     version differs from the base     -> BUMPED
#     version identical                 -> not bumped
#
#   SCAN FLOOR (#4618 shape, same wording as check_semver.sh). A diff listing
#   zero paths means the base ref is wrong or the checkout is shallow — never
#   that the branch is clean. Reporting "nothing to check" off a failed lookup
#   is the exact way a gate turns into a rubber stamp, so that case exits
#   non-zero instead.
#
# Usage:
#   VERSION_BUMP_BASE=origin/main bash scripts/detect-version-bumps.sh
#
# Exit: 0 on a successful classification; 2 when the merge base cannot be
#   resolved or the diff is empty (fail closed).
#
# Test: scripts/check-ci-helpers-selftest.sh (`detect-version-bumps:` cases)
#   drives it against throwaway repos covering bumped, unbumped, source-only,
#   added, removed and multi-crate branches.

set -euo pipefail

BASE="${VERSION_BUMP_BASE:-origin/main}"

# package_version — read the `[package]` table's literal `version = "..."` from
# a manifest on stdin. Prints nothing when the crate inherits the version or
# declares none, which the caller reads as "declares nothing here".
package_version() {
  awk '
    /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*$/); next }
    in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
      if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }
  '
}

main() {
  local merge_base
  if ! merge_base="$(git merge-base "$BASE" HEAD 2>/dev/null)"; then
    echo "detect-version-bumps: cannot resolve merge-base against '${BASE}'" >&2
    return 2
  fi

  local changed count
  changed="$(git diff --name-only --no-renames "$merge_base" HEAD)"
  count="$(printf '%s\n' "$changed" | grep -c '[^[:space:]]' || true)"
  if [ "${count:-0}" -lt 1 ]; then
    echo "detect-version-bumps: SCAN FLOOR — the diff ${merge_base}..HEAD lists 0 changed path(s)." >&2
    echo "      Nothing was examined, so 'no release under test' would be a guess. Check that" >&2
    echo "      '${BASE}' is the right base and that CI checked out with fetch-depth: 0." >&2
    return 2
  fi

  local bumped_crates="" path dir head_version base_version
  while IFS= read -r path; do
    case "$path" in
      crates/*/Cargo.toml) ;;
      *) continue ;;
    esac
    dir="${path#crates/}"
    dir="${dir%/Cargo.toml}"
    # A nested manifest (crates/<a>/<b>/Cargo.toml) is not a workspace member.
    case "$dir" in */*) continue ;; esac

    head_version=""
    if [ -f "$path" ]; then
      head_version="$(package_version <"$path")"
    fi
    if [ -z "$head_version" ]; then
      echo "  no declared version at HEAD: ${path}" >&2
      continue
    fi

    base_version="$(git show "${merge_base}:${path}" 2>/dev/null | package_version || true)"
    if [ "$head_version" = "$base_version" ]; then
      echo "  unchanged: ${dir} ${head_version}" >&2
      continue
    fi
    echo "  bumped: ${dir} ${base_version:-<absent>} -> ${head_version}" >&2
    bumped_crates="${bumped_crates}${bumped_crates:+ }${dir}"
  done <<<"$changed"

  local bumped=false
  [ -n "$bumped_crates" ] && bumped=true

  echo "detect-version-bumps: ${count} changed path(s) -> bumped=${bumped} (${bumped_crates:-none})" >&2
  echo "bumped=${bumped}"
  echo "bumped_crates=${bumped_crates}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      echo "bumped=${bumped}"
      echo "bumped_crates=${bumped_crates}"
    } >>"${GITHUB_OUTPUT}"
  fi
}

main "$@"

#!/usr/bin/env bash
# scripts/bump-version.sh
#
# Why: trusty-tools releases each crate independently (tag `<prefix>-v<version>`),
# and the manual ritual — read the current version, hand-compute the next semver,
# edit Cargo.toml, regenerate the unreleased CHANGELOG section, then recall the
# exact tag/push commands — is error-prone. This helper does the mechanical bump
# and changelog staging, then PRINTS (never runs) the tag/push commands so the
# human stays in the loop per the repo's manual-tag release convention.
#
# What: Given a crate directory under crates/ and a bump level
# (major|minor|patch), reads the package `version = "X.Y.Z"` from
# crates/<crate-dir>/Cargo.toml, computes the next semver, edits that line in
# place, then calls scripts/generate-changelog.sh <crate-dir> <tag-prefix> to
# prepend the unreleased CHANGELOG section. For every current crate the tag
# prefix equals the crate-dir name (tag_prefix_for() is the single, easy-to-
# extend place that derives it). Finally it prints — but does NOT execute — the
# `git tag` and `git push` commands.
#
# Test: `bash -n scripts/bump-version.sh` for syntax and `shellcheck
# scripts/bump-version.sh` for lint. Functionally, the pure version-bump logic
# lives in bump_semver()/read_package_version()/write_package_version(), which
# can be exercised against a throwaway copy of a Cargo.toml without mutating any
# real crate manifest (see the PR's verification notes).
#
# Usage:
#   scripts/bump-version.sh <crate-dir> <major|minor|patch>
#
# Example:
#   scripts/bump-version.sh trusty-search patch
#   scripts/bump-version.sh trusty-git-analytics minor

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Why: keep a single place where the release tag prefix is derived so it is easy
# to extend if a crate ever needs a prefix that differs from its directory name.
# For every CURRENT crate the tag prefix IS the crate directory name — including
# trusty-git-analytics, whose tags are `trusty-git-analytics-v*` (the cargo
# *package* short-name `tga` is never used as a tag prefix). The caller already
# passes the crate-dir, so today this is an identity mapping.
# What: prints the tag prefix for a given crate-dir.
tag_prefix_for() {
  local crate_dir="$1"
  echo "${crate_dir}"
}

usage() {
  echo "Usage: scripts/bump-version.sh <crate-dir> <major|minor|patch>" >&2
  echo "" >&2
  echo "  <crate-dir>            directory under crates/ (e.g. trusty-search)" >&2
  echo "  <major|minor|patch>    semver component to increment" >&2
  echo "" >&2
  echo "Reads the package version from crates/<crate-dir>/Cargo.toml, bumps it," >&2
  echo "stages the unreleased CHANGELOG section, and PRINTS the tag/push commands" >&2
  echo "for you to run (it never tags or pushes itself)." >&2
  exit 2
}

# Why: the package version is the FIRST `version = "..."` line in a crate
# Cargo.toml (it sits in the [package] table, above any dependency version
# pins). Anchoring on the first occurrence avoids accidentally matching a
# dependency's `version = "..."`.
# What: prints the X.Y.Z string from crates/<crate-dir>/Cargo.toml, or fails.
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

# Why: centralise the semver arithmetic so it is unit-testable in isolation.
# What: given X.Y.Z and a level, returns the next version (resetting lower
# components: a minor bump zeroes patch; a major bump zeroes minor and patch).
bump_semver() {
  local current="$1" level="$2"
  local major minor patch
  IFS='.' read -r major minor patch <<<"${current}"
  case "${level}" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
    *)
      echo "ERROR: invalid bump level '${level}' (expected major|minor|patch)" >&2
      return 1
      ;;
  esac
  echo "${major}.${minor}.${patch}"
}

# Why: edit only the package version line, leaving dependency pins untouched.
# What: rewrites the FIRST matching `version = "<old>"` line to <new> in place.
# Robustness details:
#   - The match is RIGHT-anchored (`"<old>"$`) so a trailing-comment-free
#     package version line matches exactly and a longer string such as
#     "<old>-beta" never matches.
#   - `old` is regex-escaped before being interpolated into awk's `~` pattern,
#     so the dots in a semver like 1.2.3 match literal dots (awk treats `.` as
#     "any char" otherwise).
#   - The replacement uses index()/substr() (literal), not sub() (regex), so
#     the new version is inserted verbatim with no metacharacter surprises.
#   - The temp file is created in the SAME directory as the manifest so `mv` is
#     a guaranteed-atomic rename (never a cross-filesystem copy), and a trap
#     removes it if awk fails or the process is interrupted under `set -e`.
write_package_version() {
  local manifest="$1" old="$2" new="$3"
  local tmp="${manifest}.bump.$$"
  # shellcheck disable=SC2064  # expand tmp now so the trap targets this exact file
  trap "rm -f '${tmp}'" RETURN
  awk -v old="${old}" -v new="${new}" '
    BEGIN {
      # Escape regex metacharacters in old so dots match literally.
      old_re = old
      gsub(/[][(){}.^$*+?|\\]/, "\\\\&", old_re)
    }
    !done && $0 ~ "^version[[:space:]]*=[[:space:]]*\"" old_re "\"$" {
      pos = index($0, "\"" old "\"")
      if (pos > 0) {
        $0 = substr($0, 1, pos - 1) "\"" new "\"" substr($0, pos + length(old) + 2)
        done = 1
      }
    }
    { print }
  ' "${manifest}" >"${tmp}"
  mv "${tmp}" "${manifest}"
}

main() {
  [[ $# -ne 2 ]] && usage

  local crate_dir="$1" level="$2"

  case "${level}" in
    major | minor | patch) ;;
    *)
      echo "ERROR: invalid bump level '${level}' (expected major|minor|patch)" >&2
      usage
      ;;
  esac

  local crate_path="${WORKSPACE_ROOT}/crates/${crate_dir}"
  if [[ ! -d "${crate_path}" ]]; then
    echo "ERROR: crate directory not found: crates/${crate_dir}" >&2
    exit 1
  fi

  local manifest="${crate_path}/Cargo.toml"
  if [[ ! -f "${manifest}" ]]; then
    echo "ERROR: Cargo.toml not found: crates/${crate_dir}/Cargo.toml" >&2
    exit 1
  fi

  # Pre-flight (BEFORE mutating any Cargo.toml): the changelog generator must
  # exist and be executable. Failing here keeps the repo unmodified rather than
  # leaving it half-bumped (version edited but CHANGELOG never staged).
  local changelog_script="${WORKSPACE_ROOT}/scripts/generate-changelog.sh"
  if [[ ! -x "${changelog_script}" ]]; then
    echo "ERROR: ${changelog_script} is missing or not executable — refusing to" >&2
    echo "       bump ${manifest} to avoid leaving the repo half-bumped." >&2
    exit 1
  fi

  local current next prefix
  current="$(read_package_version "${manifest}")"
  next="$(bump_semver "${current}" "${level}")"
  prefix="$(tag_prefix_for "${crate_dir}")"

  # Defensive no-op guard: a computed version equal to the current one means
  # nothing would change — abort rather than print a misleading "Bumped" line.
  if [[ "${next}" == "${current}" ]]; then
    echo "ERROR: computed version ${next} equals current version ${current}; nothing to bump" >&2
    exit 1
  fi

  write_package_version "${manifest}" "${current}" "${next}"

  # Verify the rewrite actually landed: a silent awk non-match (e.g. an
  # unexpected manifest layout) must abort here instead of printing "Bumped".
  #
  # Why flexible whitespace: write_package_version()/read_package_version()
  # already tolerate arbitrary spacing around `=` (`[[:space:]]*`) because some
  # crate manifests use column-aligned fields, e.g. `version     = "0.6.4"` in
  # crates/trusty-review/Cargo.toml. This check used to be a literal
  # `grep -qF "version = \"${next}\""` (single space only), which produced a
  # false-negative "ERROR: version rewrite failed" on trusty-review's aligned
  # Cargo.toml during the 0.6.4 patch release even though the awk rewrite above
  # succeeded — see issue #1888. Match with the same [[:space:]]* tolerance the
  # rest of this script uses so the verification can't diverge from the write.
  local next_re="${next//./\\.}"
  if ! grep -qE "^version[[:space:]]*=[[:space:]]*\"${next_re}\"" "${manifest}"; then
    echo "ERROR: version rewrite failed — ${manifest} still contains ${current}" >&2
    exit 1
  fi
  echo "Bumped crates/${crate_dir}/Cargo.toml: ${current} -> ${next} (${level})" >&2

  # Stage the unreleased CHANGELOG section for this crate's tag series.
  echo "Staging unreleased CHANGELOG section via generate-changelog.sh ..." >&2
  "${changelog_script}" "${crate_dir}" "${prefix}"

  # Print — but DO NOT RUN — the manual tag/push commands (human stays in loop).
  local tag="${prefix}-v${next}"
  echo "" >&2
  echo "Next steps (review, then run these yourself):" >&2
  echo "" >&2
  echo "  git add crates/${crate_dir}/Cargo.toml crates/${crate_dir}/CHANGELOG.md  # add Cargo.lock too if cargo regenerated it" >&2
  echo "  git commit -m \"chore(release): ${crate_dir} ${next}\"" >&2
  echo "  git tag ${tag}" >&2
  echo "  git push origin ${tag}" >&2
}

main "$@"

#!/usr/bin/env bash
# release_notes_from_changelog.sh <crate-dir> <version> — print one release's
# section of crates/<crate-dir>/CHANGELOG.md, from its `## [<version>]` heading
# up to (not including) the next `## ` heading.
#
# Why: the GitHub Release body used to come from git-cliff with
#   `--include-path crates/<crate>/**`. That flag drops every commit outside the
#   crate, including the commit a tag points at when it is a release-gate fix
#   or a cross-crate change. The tag then attaches to no commit, git-cliff finds
#   no release, and `--latest` renders the `## [Unreleased]` section (#11452).
#   The assembled CHANGELOG.md section is written per release by
#   scripts/assemble-changelog.sh and does not depend on which commit is tagged.
# What: awk prints from the matching heading to the next `## `. Exit 1 with a
#   message on stderr when the section is missing or empty.
# Test: scripts/release_notes_from_changelog_selftest.sh
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <crate-dir> <version>" >&2
  exit 2
fi
crate="$1"
version="$2"
root="${REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
file="$root/crates/$crate/CHANGELOG.md"

if [ ! -f "$file" ]; then
  echo "release_notes_from_changelog: $file not found" >&2
  exit 1
fi

out="$(awk -v want="## [$version]" '
  index($0, want) == 1 { on = 1; print; next }
  on && /^## / { exit }
  on { print }
' "$file")"

if [ -z "$out" ]; then
  echo "release_notes_from_changelog: no '## [$version]' section in $file" >&2
  exit 1
fi
printf '%s\n' "$out"

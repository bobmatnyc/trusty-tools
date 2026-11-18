#!/usr/bin/env bash
# Selftest for release_notes_from_changelog.sh (#11452): a section is cut at the
# next `## ` heading, starts with its own version heading, and a missing version
# fails instead of printing an empty body.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/crates/demo"
cat > "$tmp/crates/demo/CHANGELOG.md" <<'EOF'
# Changelog

## [1.2.0] — 2026-01-02

### Added

- new thing (#2)

## [1.1.0] — 2026-01-01

### Fixed

- old thing (#1)
EOF
export REPO_ROOT="$tmp"

got="$("$here/release_notes_from_changelog.sh" demo 1.2.0)"
[ "$(printf '%s\n' "$got" | head -n1)" = "## [1.2.0] — 2026-01-02" ] || { echo "FAIL: header" >&2; exit 1; }
printf '%s\n' "$got" | grep -q 'new thing' || { echo "FAIL: body" >&2; exit 1; }
if printf '%s\n' "$got" | grep -q 'old thing\|Unreleased'; then echo "FAIL: leaked section" >&2; exit 1; fi
if "$here/release_notes_from_changelog.sh" demo 9.9.9 2>/dev/null; then echo "FAIL: missing version passed" >&2; exit 1; fi
echo "release_notes_from_changelog_selftest: ok"

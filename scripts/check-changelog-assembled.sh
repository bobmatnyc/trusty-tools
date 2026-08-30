#!/usr/bin/env bash
#
# check-changelog-assembled.sh — the changelog assembler becomes a publish gate
# (#6406).
#
# Why: six `trusty-audit` tags (0.8.0 -> 0.12.0) were cut by hand-editing
#   `Cargo.toml` directly, skipping `scripts/bump-version.sh` and therefore
#   `scripts/assemble-changelog.sh` — the ONLY thing that ever writes a
#   `## [<version>]` section or deletes a consumed fragment (#5919, repaired in
#   PR #6400). Fragments sat unconsumed across all six releases and
#   `CHANGELOG.md` never gained a section for any of them. Nothing mechanical
#   noticed, because nothing asked: `preflight-publish.sh`'s eight checks cover
#   identity, tree cleanliness, tag/commit parity, public-API SemVer and the
#   UI bundle, but none of them reads `changelog.d/` or `CHANGELOG.md` at all.
#
# What: given a crate and the version about to ship, asserts the two facts
#   that are only both true when the assembler actually ran for THIS version:
#
#   STRANDED-FRAGMENTS   `crates/<crate>/changelog.d/` holds a fragment file
#                        (anything but the tracked `README.md` placeholder).
#                        A successful assemble run always deletes every
#                        fragment it consumes in the same operation — see
#                        `delete_consumed_fragments()` in
#                        `scripts/assemble-changelog.sh` — so any survivor
#                        means either the assembler never ran, or it ran and
#                        failed partway (which itself prints a loud error and
#                        refuses to report success). Either way, publishing
#                        now ships a release with unrecorded changes still
#                        sitting in the working tree.
#
#   NO-SECTION           `crates/<crate>/CHANGELOG.md` has no
#                        `## [<version>]` heading for the exact version being
#                        published. The assembler is the only writer of that
#                        heading; its absence means either it never ran for
#                        this version, or the version in `Cargo.toml` was
#                        bumped by hand after the last real assemble run.
#
#   Both findings are checked unconditionally and reported together — a
#   half-bypassed release (say, a hand-added section with fragments still
#   left behind, or vice versa) is exactly the state a partial workaround
#   would produce, and naming only one finding would leave the other
#   invisible.
#
#   NOT what this checks: whether `changelog.d/` is empty in general. An
#   empty `changelog.d/` (or one holding only `README.md`) between releases is
#   the STEADY state — see `scripts/assemble-changelog.sh`'s own note on this —
#   and a NON-empty one is completely normal for a crate whose next release
#   has not been cut yet (trusty-mpm's `changelog.d/` holds dozens of pending
#   fragments at any given time). This check only makes sense asked AT THE
#   MOMENT a specific version is about to be tagged or published — which is
#   exactly when `scripts/preflight-publish.sh` calls it, immediately before
#   `cargo publish`, as its own CHECK 9.
#
# Usage:
#   scripts/check-changelog-assembled.sh [--repo <dir>] <crate-name-or-dir> [version]
#
#   <crate-name-or-dir>  the crates.io package name (e.g. `tga`) or the
#                        crates/ directory name (e.g. `trusty-git-analytics`),
#                        resolved the same way preflight-publish.sh does.
#   [version]            defaults to the first `version = "X.Y.Z"` line in
#                        that crate's Cargo.toml — the version `cargo publish`
#                        will actually ship.
#   --repo <dir>         operate on a different repo root (self-test only).
#
# Exit codes: 0 = the assembler ran for this version — safe on this axis.
#   1 = at least one finding (STRANDED-FRAGMENTS and/or NO-SECTION) — do NOT
#   tag or publish. 2 = usage error.
#
# Test: scripts/check-changelog-assembled-selftest.sh builds synthetic repos
#   for a stranded-fragment crate, a crate missing its version section, and a
#   correctly-assembled crate, and asserts both the exit status and the
#   finding code for each. It also runs this script against the REAL
#   trusty-audit crate at its current (post-#6400-repair) state to prove the
#   live repo passes.
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

usage() {
  echo "usage: scripts/check-changelog-assembled.sh [--repo <dir>] <crate-name-or-dir> [version]" >&2
  exit 2
}

REPO_ARG=""
POSITIONAL=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || usage
      REPO_ARG="$2"
      shift 2
      ;;
    -*)
      echo "check-changelog-assembled: unknown argument: $1" >&2
      usage
      ;;
    *)
      POSITIONAL="${POSITIONAL}${POSITIONAL:+ }$1"
      shift
      ;;
  esac
done

# shellcheck disable=SC2086
set -- $POSITIONAL
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
CRATE_INPUT="$1"
VERSION_ARG="${2:-}"

if [ -n "$REPO_ARG" ]; then
  REPO_ROOT="$(cd "$REPO_ARG" && pwd)"
else
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
fi
cd "$REPO_ROOT"

# resolve_crate_dir: accept either the crates.io package name or the crates/
# directory name. A deliberate standalone copy of preflight-publish.sh's and
# check-tag-publish-parity.sh's resolver — each of these scripts must run
# independently from any cwd with no sourcing contract between them, and the
# logic is two lines over a manifest glob.
resolve_crate_dir() {
  local input="$1" manifest dir
  if [ -f "${REPO_ROOT}/crates/${input}/Cargo.toml" ]; then
    echo "$input"
    return 0
  fi
  for manifest in "${REPO_ROOT}"/crates/*/Cargo.toml; do
    [ -f "$manifest" ] || continue
    if grep -qE "^name[[:space:]]*=[[:space:]]*\"${input}\"" "$manifest"; then
      dir="$(basename "$(dirname "$manifest")")"
      echo "$dir"
      return 0
    fi
  done
  return 1
}

CRATE_DIR=""
if ! CRATE_DIR="$(resolve_crate_dir "$CRATE_INPUT")"; then
  echo "check-changelog-assembled: ERROR: no crate found matching '${CRATE_INPUT}'" >&2
  exit 2
fi
CRATE_PATH="${REPO_ROOT}/crates/${CRATE_DIR}"
MANIFEST="${CRATE_PATH}/Cargo.toml"

PKG_NAME="$(grep -m1 -E '^name[[:space:]]*=[[:space:]]*"' "$MANIFEST" \
  | sed -E 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
if [ -z "$PKG_NAME" ]; then
  echo "check-changelog-assembled: ERROR: could not read 'name' from ${MANIFEST}" >&2
  exit 2
fi

if [ -n "$VERSION_ARG" ]; then
  VERSION="$VERSION_ARG"
else
  VERSION="$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' "$MANIFEST" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
  if [ -z "$VERSION" ]; then
    echo "check-changelog-assembled: ERROR: could not find version = \"X.Y.Z\" in ${MANIFEST}" >&2
    exit 2
  fi
fi

echo "check-changelog-assembled: crate=${CRATE_DIR} package=${PKG_NAME} version=${VERSION}" >&2

FINDINGS=0

# ---------------------------------------------------------------------------
# STRANDED-FRAGMENTS: changelog.d/ holds anything but the README.md
# placeholder. Mirrors the fragment-collection glob in
# scripts/assemble-changelog.sh's main() exactly, so this check and the
# assembler can never disagree about what counts as a fragment.
# ---------------------------------------------------------------------------
FRAG_DIR="${CRATE_PATH}/changelog.d"
if [ -d "$FRAG_DIR" ]; then
  STRANDED="$(find "$FRAG_DIR" -maxdepth 1 -type f -name '*.md' ! -name 'README.md' | LC_ALL=C sort || true)"
else
  STRANDED=""
fi

if [ -n "$STRANDED" ]; then
  FINDINGS=$((FINDINGS + 1))
  echo "FAIL: STRANDED-FRAGMENTS — crates/${CRATE_DIR}/changelog.d/ still holds" >&2
  echo "      fragment(s) that ${PKG_NAME} ${VERSION} was supposed to consume:" >&2
  printf '%s\n' "$STRANDED" | sed "s#^${CRATE_PATH}/#         #" >&2
  echo "      A successful 'scripts/assemble-changelog.sh ${CRATE_DIR} <version>' run" >&2
  echo "      deletes every fragment it folds into CHANGELOG.md in the same" >&2
  echo "      operation, so a survivor means the assembler either never ran for" >&2
  echo "      this release or was bypassed by hand-editing ${MANIFEST#"${REPO_ROOT}"/}." >&2
  echo "      This is exactly the #5919 shape: six trusty-audit tags shipped with" >&2
  echo "      changelog.d/ fragments nobody ever folded in." >&2
  echo "      Fix: run the real bump path, which calls the assembler for you:" >&2
  echo "        scripts/bump-version.sh ${CRATE_DIR} <major|minor|patch>" >&2
  echo "      Already at the version you intend to ship? Assemble directly:" >&2
  echo "        scripts/assemble-changelog.sh ${CRATE_DIR} ${VERSION}" >&2
else
  echo "PASS: no stranded fragments in crates/${CRATE_DIR}/changelog.d/." >&2
fi

# ---------------------------------------------------------------------------
# NO-SECTION: CHANGELOG.md has no '## [<version>]' heading. Same anchor and
# escaping as scripts/assemble-changelog.sh uses to detect an EXISTING
# section, so "the assembler would consider this version already cut" and
# "this check finds the section" can never disagree.
# ---------------------------------------------------------------------------
CHANGELOG="${CRATE_PATH}/CHANGELOG.md"
if [ ! -f "$CHANGELOG" ]; then
  FINDINGS=$((FINDINGS + 1))
  echo "FAIL: NO-SECTION — crates/${CRATE_DIR}/CHANGELOG.md does not exist." >&2
  echo "      There is nowhere a '## [${VERSION}]' section could have been written." >&2
elif ! grep -qE "^## \[${VERSION//./\\.}\]" "$CHANGELOG"; then
  FINDINGS=$((FINDINGS + 1))
  echo "FAIL: NO-SECTION — crates/${CRATE_DIR}/CHANGELOG.md has no '## [${VERSION}]'" >&2
  echo "      heading. The assembler is the only writer of that heading" >&2
  echo "      (scripts/assemble-changelog.sh); its absence means either it never" >&2
  echo "      ran for this version, or Cargo.toml's version was hand-edited after" >&2
  echo "      the last real assemble run." >&2
  echo "      Fix: run the real bump path, which writes the section for you:" >&2
  echo "        scripts/bump-version.sh ${CRATE_DIR} <major|minor|patch>" >&2
  echo "      Already at the version you intend to ship? Assemble directly:" >&2
  echo "        scripts/assemble-changelog.sh ${CRATE_DIR} ${VERSION}" >&2
else
  echo "PASS: crates/${CRATE_DIR}/CHANGELOG.md has a '## [${VERSION}]' section." >&2
fi

if [ "$FINDINGS" -gt 0 ]; then
  echo "check-changelog-assembled: FAILED (${FINDINGS} finding(s)) for ${PKG_NAME} ${VERSION}." >&2
  exit 1
fi

echo "check-changelog-assembled: OK — ${PKG_NAME} ${VERSION} was assembled by the real tooling." >&2
exit 0

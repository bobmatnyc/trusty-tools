#!/usr/bin/env bash
#
# check-tag-publish-parity.sh — the release tag must name the commit that
# `cargo publish` actually ships.
#
# Why: nothing in this repo's release path bound the two together. The
#   ordering (docs/reference/release-workflow.md steps 5-8) is: tag, push tag,
#   check-publish-ready.sh, cargo publish. check-publish-ready.sh's GUARD 2
#   asks only whether the tag's commit is an ANCESTOR of origin/main, and
#   preflight-publish.sh's CHECK 1 asks only whether HEAD EQUALS origin/main.
#   Both pass when the tag sits several commits behind HEAD, so a release run
#   that gets fast-forwarded between tagging and publishing — which is the
#   normal way to satisfy CHECK 1 after main moves — publishes one commit and
#   tags another, and every gate stays green.
#
#   That happened on 2026-08-11. `tga-v2.17.0` points at 246e4ca2, while the
#   published crate's .cargo_vcs_info.json records 7d5cf82e1; 246e4ca2 is an
#   ancestor of 7d5cf82e1 (two trusty-search commits landed in between). Blast
#   radius was zero only because `git diff 246e4ca2 7d5cf82e1 --
#   crates/trusty-git-analytics/` is empty. Had any of those intervening
#   commits touched the crate, `git checkout tga-v2.17.0` would show a tree
#   that is not what shipped, and nothing would ever have said so.
#
#   The sibling release that night, trusty-review-v0.15.0, is at the SAME
#   commit 246e4ca2 and is consistent — it published before the fast-forward.
#   Consistency was a matter of timing, not of any check.
#
# What: resolves the release tag(s) for <crate>+<version> and asserts the
#   commit they name is exactly the commit a publish from this checkout would
#   record. Four findings, each named on stderr so a failure cannot be mistaken
#   for a different one:
#
#     TAG-MISSING         no candidate tag resolves, on origin or locally.
#                         Publishing would ship an untagged commit.
#     TAG-SPLIT           two accepted alias tags exist at DIFFERENT commits
#                         (`tga-v*` and `trusty-git-analytics-v*` — both are
#                         accepted release tags, #1128). Whichever one a reader
#                         checks out, one of them is lying.
#     TAG-DRIFT           the tag resolves, but names a commit other than HEAD.
#                         This is the fast-forward case above.
#     VCS-INFO-MISMATCH   --vcs-info only: the sha1 cargo recorded in the
#                         packaged .cargo_vcs_info.json is not the tag's commit.
#                         This is the defect observed directly rather than
#                         inferred, and is the one check that still works after
#                         the upload has happened.
#
#   The publish commit is `git rev-parse HEAD^{commit}`, because that is the
#   sha1 `cargo package` writes into .cargo_vcs_info.json — the same field that
#   recorded 7d5cf82e1 above. preflight-publish.sh's CHECK 3 already requires a
#   clean tree, so HEAD is the whole content of the shipped tarball.
#
#   Tag resolution prefers origin over local refs: origin is what a reader
#   fetches, and a local-only tag is not a release tag. Annotated tags are
#   dereferenced (`refs/tags/<t>^{}`) so a tag OBJECT sha is never compared
#   against a commit sha. `--no-fetch` reads local refs only.
#
# WHY --vcs-info IS NOT AUTOMATIC IN preflight-publish.sh: cargo leaves
#   target/package/<pkg>-<version>/.cargo_vcs_info.json behind after any
#   `cargo package` / `cargo publish --dry-run` / `cargo publish`, and it is
#   not invalidated by a later checkout. Reading whatever happens to be on disk
#   would fail a legitimate release whose dry-run predated a rebase, and a gate
#   that cries wolf gets bypassed. The git-level TAG-DRIFT check is the
#   fail-closed one and it runs unconditionally; --vcs-info is the explicit
#   confirmation, run against a fresh artifact — see "Usage" below.
#
#   Residual window this does NOT close: a human could pass preflight, then
#   fast-forward, then publish. Nothing short of making the tag and the upload
#   one command can prevent that, and `cargo publish` is not ours to wrap. The
#   post-publish --vcs-info run is what detects it, before the tag is anything
#   anyone has relied on.
#
# Usage:
#   scripts/check-tag-publish-parity.sh <crate-name-or-dir> [version]
#   scripts/check-tag-publish-parity.sh --vcs-info auto <crate-name-or-dir>
#   scripts/check-tag-publish-parity.sh --vcs-info <path> <crate> [version]
#
#     --vcs-info <path|auto>  additionally compare the sha1 recorded in a
#                             .cargo_vcs_info.json against the tag's commit.
#                             `auto` resolves
#                             target/package/<pkg>-<version>/.cargo_vcs_info.json
#                             and FAILS if it is absent or unreadable — an
#                             explicit request to verify cannot degrade into a
#                             skip. Run this immediately after `cargo publish`.
#     --repo <dir>            operate on a different repo root (self-test).
#     --no-fetch              never contact origin; resolve tags from local
#                             refs only (offline, and the self-test).
#     -h|--help               print this header.
#
#   Version defaults to the first `version = "X.Y.Z"` in the crate's
#   Cargo.toml, matching what `cargo publish` will ship.
#
# Exit codes: 0 = tag and publish commit agree. 1 = they do not (DO NOT
#   PUBLISH, or if already published, the tag needs correcting). 2 = usage.
#
# Test: scripts/check-tag-publish-parity-selftest.sh, which builds synthetic
#   repos for each finding — including the fast-forward sequence replayed with
#   the real 2026-08-11 SHAs — and asserts both the exit status and the finding
#   code. Run it directly: bash scripts/check-tag-publish-parity-selftest.sh
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
  echo "usage: scripts/check-tag-publish-parity.sh [--vcs-info <path|auto>] [--repo <dir>]" >&2
  echo "                                           [--no-fetch] <crate-name-or-dir> [version]" >&2
  exit 2
}

VCS_INFO=""
REPO_ARG=""
NO_FETCH=0
POSITIONAL=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --vcs-info)
      [ "$#" -ge 2 ] || usage
      VCS_INFO="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || usage
      REPO_ARG="$2"
      shift 2
      ;;
    --no-fetch)
      NO_FETCH=1
      shift
      ;;
    -*)
      echo "check-tag-publish-parity: unknown argument: $1" >&2
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

# resolve_crate_dir / the version read below are deliberate third copies of
# check-publish-ready.sh's and preflight-publish.sh's resolvers. Each of the
# three has to run standalone from any cwd with no sourcing contract between
# them, and the logic is a two-line grep over a manifest; a shared lib here
# would buy nothing and add a load-order dependency to the release path.
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
  echo "check-tag-publish-parity: ERROR: no crate found matching '${CRATE_INPUT}'" >&2
  exit 2
fi
MANIFEST="${REPO_ROOT}/crates/${CRATE_DIR}/Cargo.toml"

PKG_NAME="$(grep -m1 -E '^name[[:space:]]*=[[:space:]]*"' "$MANIFEST" \
  | sed -E 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
if [ -z "$PKG_NAME" ]; then
  echo "check-tag-publish-parity: ERROR: could not read 'name' from ${MANIFEST}" >&2
  exit 2
fi

if [ -n "$VERSION_ARG" ]; then
  VERSION="$VERSION_ARG"
else
  VERSION="$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' "$MANIFEST" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
  if [ -z "$VERSION" ]; then
    echo "check-tag-publish-parity: ERROR: could not find version = \"X.Y.Z\" in ${MANIFEST}" >&2
    exit 2
  fi
fi

# Candidate tags. The crate-dir prefix is bump-version.sh's tag_prefix_for();
# the package-name prefix covers the tga alias series, which #1128 made a real,
# accepted release tag and which is the form 2026-08-11 actually pushed. Both
# are accepted, so both are checked — and disagreeing aliases are TAG-SPLIT
# rather than a coin flip over which one a reader believes.
CANDIDATES="${CRATE_DIR}-v${VERSION}"
if [ "$PKG_NAME" != "$CRATE_DIR" ]; then
  CANDIDATES="${CANDIDATES} ${PKG_NAME}-v${VERSION}"
fi

echo "check-tag-publish-parity: crate=${CRATE_DIR} package=${PKG_NAME} version=${VERSION}" >&2

# resolve_tag_commit: prints the COMMIT a tag names, or nothing.
# Annotated tags: `git ls-remote --tags` lists both refs/tags/<t> (the tag
# object) and refs/tags/<t>^{} (the commit it wraps). Preferring the ^{} line
# is what keeps a tag-object sha from being compared against a commit sha and
# reported as drift that does not exist.
resolve_tag_commit() {
  local tag="$1" lines deref plain
  if [ "$NO_FETCH" -eq 0 ]; then
    lines="$(git ls-remote --tags origin "refs/tags/${tag}" "refs/tags/${tag}^{}" 2>/dev/null || true)"
    # `|| true` on each: a no-match grep exits 1, and under `set -o pipefail`
    # that aborts the whole script instead of reporting TAG-MISSING. An
    # unresolvable tag is a finding this gate must be able to PRINT.
    deref="$(printf '%s\n' "$lines" | { grep '\^{}$' || true; } | cut -f1 | head -1)"
    plain="$(printf '%s\n' "$lines" | { grep -v '\^{}$' || true; } | cut -f1 | head -1)"
    if [ -n "$deref" ]; then
      echo "$deref"
      return 0
    fi
    if [ -n "$plain" ]; then
      echo "$plain"
      return 0
    fi
  fi
  git rev-parse --verify --quiet "refs/tags/${tag}^{commit}" 2>/dev/null || true
}

FOUND_TAGS=""
FOUND_SHAS=""
for tag in $CANDIDATES; do
  sha="$(resolve_tag_commit "$tag")"
  if [ -n "$sha" ]; then
    echo "check-tag-publish-parity: tag ${tag} -> ${sha}" >&2
    FOUND_TAGS="${FOUND_TAGS}${FOUND_TAGS:+ }${tag}"
    FOUND_SHAS="${FOUND_SHAS}${FOUND_SHAS:+ }${sha}"
  fi
done

if [ -z "$FOUND_SHAS" ]; then
  echo "FAIL: TAG-MISSING — no release tag resolves for ${PKG_NAME} ${VERSION}." >&2
  echo "      Looked for: ${CANDIDATES}" >&2
  echo "      Publishing now would ship a commit no tag names. Tag the commit you" >&2
  echo "      are about to publish and push it, then re-run:" >&2
  echo "        git tag ${CRATE_DIR}-v${VERSION} && git push origin ${CRATE_DIR}-v${VERSION}" >&2
  exit 1
fi

DISTINCT="$(printf '%s\n' $FOUND_SHAS | sort -u | wc -l | tr -d ' ')"
if [ "$DISTINCT" -gt 1 ]; then
  echo "FAIL: TAG-SPLIT — the accepted alias tags for ${PKG_NAME} ${VERSION} name" >&2
  echo "      DIFFERENT commits, so at least one of them misrepresents the release:" >&2
  paste -d' ' <(printf '%s\n' $FOUND_TAGS) <(printf '%s\n' $FOUND_SHAS) | sed 's/^/        /' >&2
  echo "      Delete and re-push the wrong one so both name the publish commit." >&2
  exit 1
fi

TAG_SHA="$(printf '%s\n' $FOUND_SHAS | head -1)"
MATCHED_TAG="$(printf '%s\n' $FOUND_TAGS | head -1)"
PUBLISH_SHA="$(git rev-parse "HEAD^{commit}")"

if [ "$TAG_SHA" != "$PUBLISH_SHA" ]; then
  echo "FAIL: TAG-DRIFT — ${MATCHED_TAG} names ${TAG_SHA}, but a publish from this" >&2
  echo "      checkout ships ${PUBLISH_SHA}." >&2
  if git merge-base --is-ancestor "$TAG_SHA" "$PUBLISH_SHA" 2>/dev/null; then
    echo "      The tag is an ANCESTOR of HEAD: this checkout was fast-forwarded after" >&2
    echo "      tagging. That is the 2026-08-11 sequence — tga-v2.17.0 tagged 246e4ca2," >&2
    echo "      published 7d5cf82e1 — and every other release gate stays green through" >&2
    echo "      it. Commits added since the tag:" >&2
    git log --oneline "${TAG_SHA}..${PUBLISH_SHA}" 2>/dev/null | sed 's/^/        /' >&2
    echo "      Move the tag onto ${PUBLISH_SHA} and re-push it, or reset this" >&2
    echo "      checkout back to ${TAG_SHA}. Do not publish with them disagreeing:" >&2
    echo "        git tag -f ${MATCHED_TAG} ${PUBLISH_SHA}" >&2
    echo "        git push --force origin ${MATCHED_TAG}" >&2
  else
    echo "      The tag is NOT an ancestor of HEAD — these are divergent histories," >&2
    echo "      so the tagged tree and the tree about to ship differ by more than" >&2
    echo "      added commits. Re-tag the commit you intend to publish." >&2
  fi
  exit 1
fi

echo "PASS: ${MATCHED_TAG} names ${TAG_SHA}, which is the commit this publish ships." >&2

# ---------------------------------------------------------------------------
# --vcs-info: compare against what cargo actually recorded.
# ---------------------------------------------------------------------------
if [ -n "$VCS_INFO" ]; then
  if [ "$VCS_INFO" = "auto" ]; then
    VCS_INFO="${REPO_ROOT}/target/package/${PKG_NAME}-${VERSION}/.cargo_vcs_info.json"
  fi
  if [ ! -f "$VCS_INFO" ]; then
    echo "FAIL: VCS-INFO-MISSING — no .cargo_vcs_info.json at ${VCS_INFO}." >&2
    echo "      --vcs-info is an explicit request to verify the recorded commit and" >&2
    echo "      cannot degrade into a skip. Run 'cargo package -p ${PKG_NAME}' (or" >&2
    echo "      publish) first, then re-run." >&2
    exit 1
  fi
  # Same `|| true` reason as resolve_tag_commit: a file with no git.sha1 must
  # reach the VCS-INFO-UNREADABLE branch, not die on pipefail with no message.
  recorded="$({ grep -o '"sha1"[[:space:]]*:[[:space:]]*"[0-9a-fA-F]\{7,40\}"' "$VCS_INFO" || true; } \
    | head -1 | sed -E 's/.*"([0-9a-fA-F]{7,40})".*/\1/')"
  if [ -z "$recorded" ]; then
    echo "FAIL: VCS-INFO-UNREADABLE — no git.sha1 field in ${VCS_INFO}:" >&2
    sed 's/^/        /' "$VCS_INFO" >&2
    exit 1
  fi
  # Compare on the recorded length so an abbreviated sha1 still decides.
  n="${#recorded}"
  if [ "$(printf '%s' "$TAG_SHA" | cut -c1-"$n")" != "$(printf '%s' "$recorded" | tr 'A-F' 'a-f')" ]; then
    echo "FAIL: VCS-INFO-MISMATCH — ${VCS_INFO} records ${recorded}, but" >&2
    echo "      ${MATCHED_TAG} names ${TAG_SHA}. The tag does not describe the" >&2
    echo "      published tree. This is the 2026-08-11 defect observed directly." >&2
    echo "      Move the tag onto the published commit — leaving it is a permanent" >&2
    echo "      misstatement of what shipped:" >&2
    echo "        git tag -f ${MATCHED_TAG} ${recorded} && git push --force origin ${MATCHED_TAG}" >&2
    exit 1
  fi
  echo "PASS: ${VCS_INFO} records ${recorded}, matching ${MATCHED_TAG}." >&2
fi

echo "check-tag-publish-parity: OK — ${PKG_NAME} ${VERSION} tag and publish commit agree." >&2
exit 0

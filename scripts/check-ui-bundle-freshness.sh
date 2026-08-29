#!/usr/bin/env bash
#
# check-ui-bundle-freshness.sh — refuse to publish a crate whose committed UI
# bundle was built from older source than the crate currently carries (#3606).
#
# Why: `cargo publish` ships whatever is COMMITTED under a crate's bundle
#   directory, verbatim. crates/trusty-search/Cargo.toml lists "ui-dist/**/*"
#   in `include`, build.rs is short-circuited by SKIP_UI_BUILD=1 on every
#   release path, and the mirror step that refreshes the bundle
#   (`make -C crates/trusty-search sync-ui`) is a human-remembered one. Skip it
#   and the tarball carries a bundle built from source that no longer exists.
#   That has now happened three times — v0.12.1 (2026-05-26), v0.13.1
#   (2026-05-27), and 0.37.0 (2026-07-21), which shipped an admin dashboard
#   with no dark mode because #3509's tokens.css/theme-bootstrap.js rewrite
#   never reached ui-dist/. Nothing in CI or the publish path noticed any of
#   the three.
#
# What: compares CONTENT. Each bundle carries ui-source-hash.txt, a digest of
#   the UI source it was built from, written by scripts/stamp-ui-bundle.sh as
#   part of the build. This gate recomputes that digest from the source at <rev>
#   and fails as BUNDLE-STALE when the two differ.
#
#   It compared commit ANCESTRY until a review broke it in three commits: change
#   the source without rebuilding (correctly caught), then hand-edit one
#   unrelated line of the bundle's index.html — and the still-stale bundle
#   passed, because any commit touching any byte under the bundle directory
#   satisfied "the bundle was written after the source." Content cannot be
#   laundered that way: an edit to the bundle does not move the source digest,
#   so the mismatch survives.
#
#   Not rebuilding is still deliberate. .github/workflows/ci.yml's ui-checks job
#   rejected rebuild-then-diff correctly — Vite emits content-hashed filenames
#   (index-CPNos4BT.js) that are not byte-stable across toolchain patch
#   versions, so a byte diff is a flake generator. A source digest has no such
#   instability and needs no Node.
#
#   The limit, stated plainly: the stamp records an assertion the maintainer
#   makes by running the build. Re-stamping without rebuilding falsifies it, and
#   nothing here can detect that. What it does remove is the accidental pass —
#   a stale bundle no longer clears the gate because some unrelated commit
#   happened to touch the directory.
#
#   Four further findings guard the ways a commit comparison could pass while
#   inspecting nothing:
#
#     MANIFEST-GAP     a crate tracks a ui-dist/ or ui/dist/ tree but has no
#                      manifest row — a new UI crate cannot silently sit
#                      outside the gate.
#     MANIFEST-STALE   a manifest row whose bundle directory tracks no files.
#     NO-SOURCES       a row whose UI source tree tracks no files.
#     STAMP-MISSING    a bundle with no ui-source-hash.txt, or one with no
#                      readable digest in it. Never a skip.
#     ASSET-MISSING /  the bundle's index.html references an asset that is not
#     NO-ASSET-REFS    in the bundle, or references none at all. This catches a
#                      half-finished mirror, where index.html is new and the
#                      hashed assets beside it are not — a state ancestry alone
#                      would call fresh.
#
#   Every run prints the counts it inspected (source files, bundle files, blob
#   hashes, asset references) and a digest over the bundle's blob SHAs, so a
#   green line states what was examined rather than only that nothing failed.
#   Zero of anything is a failure, never a pass.
#
#   Without --rev the digest is computed from files on DISK, including
#   uncommitted and untracked-not-ignored ones, so an unstaged source edit is
#   reported the same way a committed one is.
#
#   There is no override flag and none is needed: the remedy always produces a
#   commit. Even a byte-identical rebuild moves the digest, because the digest
#   is over the SOURCE, not the output — so `make release-prep` leaves a changed
#   ui-source-hash.txt to commit.
#
# Usage:
#   scripts/check-ui-bundle-freshness.sh                      # every manifest row
#   scripts/check-ui-bundle-freshness.sh trusty-search        # one crate
#   scripts/check-ui-bundle-freshness.sh tga                  # package name also accepted
#   scripts/check-ui-bundle-freshness.sh --rev <commit> ts    # audit a historical commit
#   scripts/check-ui-bundle-freshness.sh --repo <path>        # another checkout (selftest)
#
#   A crate with no manifest row and no tracked bundle exits 0 with an explicit
#   "no committed UI bundle" line — that path is VERIFIED against the tree, not
#   assumed, so preflight-publish.sh can call this for every crate.
#
# Exit codes: 0 = every inspected row is fresh (or the crate ships no bundle).
#   1 = at least one finding — DO NOT PUBLISH. 2 = usage error.
#
# Test: scripts/check-ui-bundle-freshness-selftest.sh drives every finding code
#   against synthetic repos, including the vacuous-scan refusals. The #3606
#   incident itself replays with
#   `scripts/check-ui-bundle-freshness.sh --rev fc7f396f trusty-search`.
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h | --help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/ui_source_digest.sh
. "${SCRIPT_DIR}/lib/ui_source_digest.sh"
REPO_ROOT=""
REV=""
CRATE_INPUT=""

usage() {
  echo "usage: scripts/check-ui-bundle-freshness.sh [--repo <path>] [--rev <commit>] [<crate-name-or-dir>]" >&2
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      [ $# -ge 2 ] || usage
      REPO_ROOT="$2"
      shift 2
      ;;
    --rev)
      [ $# -ge 2 ] || usage
      REV="$2"
      shift 2
      ;;
    -*)
      echo "check-ui-bundle-freshness: unknown argument: $1" >&2
      usage
      ;;
    *)
      [ -n "$CRATE_INPUT" ] && usage
      CRATE_INPUT="$1"
      shift
      ;;
  esac
done

if [ -z "$REPO_ROOT" ]; then
  REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
fi
REPO_ROOT="$(cd "$REPO_ROOT" && pwd)"

# The working tree is the only place the manifest is read from, even under
# --rev: auditing a commit that predates the manifest is the whole point of
# --rev, and a historical copy of the manifest would describe a layout that is
# no longer the one being defended.
MANIFEST="${REPO_ROOT}/scripts/ui-bundle-manifest.tsv"

WORKTREE_MODE=0
DIGEST_REV=""
if [ -z "$REV" ]; then
  REV="HEAD"
  WORKTREE_MODE=1
  # #3606: without --rev the question is "would publishing THIS tree be safe",
  # so the digest reads files from disk — uncommitted edits count.
  DIGEST_REV="--worktree"
else
  DIGEST_REV="$REV"
fi

git -C "$REPO_ROOT" rev-parse --verify --quiet "${REV}^{commit}" > /dev/null || {
  echo "check-ui-bundle-freshness: ERROR: '${REV}' is not a commit in ${REPO_ROOT}" >&2
  exit 2
}

FINDINGS=0
TOTAL_SRC=0
TOTAL_BUNDLE=0
TOTAL_HASH=0
TOTAL_REFS=0
ROWS_CHECKED=0

fail() {
  echo "FAIL: $1" >&2
  shift
  [ $# -gt 0 ] && printf '%s\n' "$@" | sed 's/^/      /' >&2
  FINDINGS=$((FINDINGS + 1))
}

g() { git -C "$REPO_ROOT" "$@"; }

# ---------------------------------------------------------------------------
# Manifest
# ---------------------------------------------------------------------------
MANIFEST_ROWS=""
if [ ! -f "$MANIFEST" ]; then
  echo "FAIL: MANIFEST-MISSING — ${MANIFEST} does not exist." >&2
  echo "      Without it this gate would inspect nothing and report success." >&2
  exit 1
fi
MANIFEST_ROWS="$(grep -v '^[[:space:]]*#' "$MANIFEST" | grep -v '^[[:space:]]*$' || true)"
if [ -z "$MANIFEST_ROWS" ]; then
  echo "FAIL: MANIFEST-MISSING — ${MANIFEST} declares zero crates." >&2
  echo "      A gate with an empty manifest passes by inspecting nothing." >&2
  exit 1
fi

# resolve_crate_dir: accept the crates.io package name (tga) or the crates/
# directory name (trusty-git-analytics). Mirrors preflight-publish.sh and
# check-publish-ready.sh so this workspace keeps ONE lookup convention.
resolve_crate_dir() {
  local input="$1"
  if [ -f "${REPO_ROOT}/crates/${input}/Cargo.toml" ]; then
    echo "$input"
    return 0
  fi
  local manifest dir
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

TARGET_CRATE=""
if [ -n "$CRATE_INPUT" ]; then
  if ! TARGET_CRATE="$(resolve_crate_dir "$CRATE_INPUT")"; then
    # Not a workspace crate at all. Under --repo fixtures the caller may pass a
    # bare directory name with no manifest; treat the literal as the crate dir
    # so the manifest lookup below decides, rather than erroring here.
    TARGET_CRATE="$CRATE_INPUT"
  fi
fi

# ---------------------------------------------------------------------------
# Discovery — which committed BUNDLE DIRECTORIES actually exist at <rev>.
# Compared against the manifest in both directions so neither a new UI bundle
# nor a vanished one can drop out of the gate silently.
#
# #6155: keyed on the bundle directory, not on the crate. It used to ask "does
# this crate have a row?", which answers yes for a crate that ships TWO bundles
# and has a row for only one — trusty-console ships ui/dist AND ui-search-dist.
# Deleting or mistyping the `trusty-console-search` row would then have left the
# gate inspecting one bundle and reporting OK, which is the #3606 shape this
# whole discovery pass exists to prevent. The pattern also matches any
# `ui-*-dist`, so the next bundle of this shape is covered before it is added.
# Test: check-ui-bundle-freshness-selftest.sh cases 24d and 24e.
# ---------------------------------------------------------------------------
# The `s###` delimiter is deliberate: BUNDLE_DIR_RE contains `|`, so using `|`
# as the delimiter (the habit elsewhere in this file, where paths make `/` a bad
# choice) makes sed read the alternation as the end of the pattern and abort —
# leaving discovery empty and the gate passing having inspected nothing.
BUNDLE_DIR_RE='ui-[^/]*dist|ui/dist'
DISCOVERED_BUNDLES="$(g ls-tree -r --name-only "$REV" \
  | grep -E "^crates/[^/]+/(${BUNDLE_DIR_RE})/" \
  | sed -E "s#^(crates/[^/]+/(${BUNDLE_DIR_RE}))/.*#\1#" \
  | sort -u || true)"

manifest_row_for() {
  printf '%s\n' "$MANIFEST_ROWS" | awk -F'\t' -v c="$1" '$1 == c { print; exit }'
}

# manifest_row_for_bundle <bundle_dir> — the row that declares this directory,
# matching on the bundle_dir column so a crate's second bundle is not covered
# by its first row (#6155).
manifest_row_for_bundle() {
  printf '%s\n' "$MANIFEST_ROWS" | awk -F'\t' -v b="$1" '$3 == b { print; exit }'
}

# row_matches_target <row_crate> — whether a manifest row belongs to the crate
# the caller asked about.
#
# #6155: one crate can package more than one committed bundle. trusty-console
# ships its own ui/dist AND ui-search-dist, the trusty-search SPA it serves at
# /tools/search/. Those two rows cannot share a key: stamp-ui-bundle.sh stamps
# every row matching the name it is given, and the console's build.rs stamps
# `trusty-console` after building only its own UI — a shared key would have it
# certify a bundle it never rebuilt, which is the #3606 laundering the stamp
# exists to prevent. So the second row is keyed `<crate>-<suffix>` and THIS
# gate matches it when asked about `<crate>`. The stamper under-claims, the
# gate over-checks, and preflight-publish.sh CHECK 7 for trusty-console still
# inspects both bundles the tarball carries.
# Test: check-ui-bundle-freshness-selftest.sh case 24.
row_matches_target() {
  [ -z "$TARGET_CRATE" ] && return 0
  [ "$1" = "$TARGET_CRATE" ] && return 0
  case "$1" in "${TARGET_CRATE}-"*) return 0 ;; esac
  return 1
}

for bundle_dir in $DISCOVERED_BUNDLES; do
  crate="$(printf '%s' "$bundle_dir" | sed -E 's|^crates/([^/]+)/.*|\1|')"
  [ -n "$TARGET_CRATE" ] && [ "$crate" != "$TARGET_CRATE" ] && continue
  bundle_row="$(manifest_row_for_bundle "$bundle_dir")"
  if [ -z "$bundle_row" ]; then
    fail "MANIFEST-GAP — ${bundle_dir} is a committed UI bundle with no row in" \
      "scripts/ui-bundle-manifest.tsv, so nothing checks whether it is fresh." \
      "Add: <key><TAB><ui source dir><TAB>${bundle_dir}" \
      "The key is ${crate} for the crate's own UI, or ${crate}-<suffix> for a" \
      "second bundle it packages (#6155)."
    continue
  fi
  # #6155: a row can exist and still be unreachable from THIS invocation. The
  # gate scoped to a crate inspects that crate's key and its `<crate>-<suffix>`
  # keys, so a row keyed anything else declares the bundle without ever
  # checking it under `check-ui-bundle-freshness.sh <crate>` — which is what
  # preflight-publish.sh CHECK 7 runs. A mistyped key would otherwise leave the
  # bundle silently unchecked, the same #3606 shape as a missing row.
  bundle_row_key="$(printf '%s' "$bundle_row" | cut -f1)"
  if ! row_matches_target "$bundle_row_key"; then
    fail "MANIFEST-GAP — ${bundle_dir} has a row keyed '${bundle_row_key}', which" \
      "the gate scoped to '${TARGET_CRATE}' never inspects, so this bundle ships" \
      "unchecked. Key it '${crate}' or '${crate}-<suffix>' (#6155)."
  fi
done

# resolve_asset_ref <bundle_dir> <ref> <bundle_files> — prints the tracked file
# a bundle's index.html reference points at, or fails.
#
# A relative ref (./assets/x.js) resolves against the bundle directory and must
# match exactly. An ABSOLUTE ref resolves against the app's deploy base, which
# lives in vite.config.js and not in the bundle — trusty-console sets
# base: '/ui/', so its index.html says /ui/assets/x.js for a file stored at
# assets/x.js. Rather than parse the config, drop leading segments one at a
# time until a tracked file matches; the trailing segments are the part that
# has to exist either way.
resolve_asset_ref() {
  local bundle_dir="$1" ref="$2" files="$3"
  local candidate="${ref#./}"
  local absolute=0
  case "$ref" in /*) absolute=1 ;; esac
  candidate="${candidate#/}"
  while [ -n "$candidate" ]; do
    if printf '%s\n' "$files" | grep -qx "${bundle_dir}/${candidate}"; then
      echo "${bundle_dir}/${candidate}"
      return 0
    fi
    [ "$absolute" -eq 1 ] || return 1
    case "$candidate" in
      */*) candidate="${candidate#*/}" ;;
      *) return 1 ;;
    esac
  done
  return 1
}

# ---------------------------------------------------------------------------
# check_row <crate> <source_dir> <bundle_dir>
# ---------------------------------------------------------------------------
check_row() {
  local crate="$1" src_dir="$2" bundle_dir="$3"
  local row_findings_before="$FINDINGS"

  # Bundle side: ls-tree carries the blob SHA of every file, so the content
  # hashes come out of the same read that enumerates them.
  local bundle_files bundle_hashes bundle_count hash_count digest
  bundle_files="$(g ls-tree -r --name-only "$REV" -- "$bundle_dir" || true)"
  bundle_hashes="$(g ls-tree -r "$REV" -- "$bundle_dir" | awk 'NF { print $3 }' || true)"
  bundle_count="$(printf '%s\n' "$bundle_files" | grep -c . || true)"
  hash_count="$(printf '%s\n' "$bundle_hashes" | grep -c . || true)"

  if [ "$bundle_count" -eq 0 ]; then
    fail "MANIFEST-STALE — ${crate}: ${bundle_dir} tracks ZERO files at ${REV}." \
      "A row that compares nothing is worse than no row: it reports a green line."
    return
  fi
  if [ "$hash_count" -ne "$bundle_count" ]; then
    fail "MANIFEST-STALE — ${crate}: ${bundle_count} bundle files but ${hash_count} blob hashes."
    return
  fi
  digest="$(printf '%s\n' "$bundle_hashes" | sort | g hash-object --stdin | cut -c1-12)"

  # Source side: the digest over every bundle-affecting source file. The
  # selection rule and the hash both live in scripts/lib/ui_source_digest.sh so
  # the stamper and this gate can never disagree about either.
  local src_digest_pair src_digest src_count
  if src_digest_pair="$(ui_source_digest "$REPO_ROOT" "$DIGEST_REV" "$src_dir" "$bundle_dir")"; then
    src_digest="${src_digest_pair%% *}"
    src_count="${src_digest_pair##* }"
  else
    fail "NO-SOURCES — ${crate}: ${src_dir} holds ZERO bundle-affecting files at ${REV}." \
      "Nothing to compare the bundle against; refusing to call that fresh."
    return
  fi

  # Recorded side: the digest the bundle claims it was built from.
  local stamp_rel stamp_content stamp_digest
  stamp_rel="$(ui_stamp_path "$bundle_dir")"
  if [ "$WORKTREE_MODE" -eq 1 ] && [ -f "${REPO_ROOT}/${stamp_rel}" ]; then
    stamp_content="$(cat "${REPO_ROOT}/${stamp_rel}")"
  else
    stamp_content="$(g show "${REV}:${stamp_rel}" 2>/dev/null || true)"
  fi

  if [ -z "$stamp_content" ] || ! stamp_digest="$(ui_read_stamp "$stamp_content")"; then
    # The commit diagnostic is reported but never decides: it is what an audit
    # of pre-stamp history has to go on, since every commit before this gate
    # landed carries no stamp at all.
    fail "STAMP-MISSING — ${crate}: ${stamp_rel} is absent or carries no digest at ${REV}." \
      "Without it nothing records which source this bundle was built from, and" \
      "the gate would be asserting freshness it never measured." \
      "diagnostic: source last touched in $(g log -1 --format='%h %ci %s' "$REV" -- "$src_dir" \
        ":(exclude)${bundle_dir}" ":(exclude,glob)${src_dir}/**/*.md" 2>/dev/null || echo '<unknown>')" \
      "diagnostic: bundle last touched in $(g log -1 --format='%h %ci %s' "$REV" -- "$bundle_dir" 2>/dev/null || echo '<unknown>')" \
      "Remedy: rebuild, then bash scripts/stamp-ui-bundle.sh ${crate}"
  elif [ "$stamp_digest" != "$src_digest" ]; then
    fail "BUNDLE-STALE — ${crate}: ${bundle_dir} was built from different source than ${src_dir} now holds." \
      "recorded  ${stamp_digest}  (${stamp_rel})" \
      "actual    ${src_digest}  (${src_count} source file(s) at ${REV})" \
      "source last changed in $(g log -1 --format='%h %ci %s' "$REV" -- "$src_dir" \
        ":(exclude)${bundle_dir}" ":(exclude,glob)${src_dir}/**/*.md" 2>/dev/null || echo '<unknown>')" \
      "Publishing now ships a UI built from source that has since changed —" \
      "the #3606 shape (0.37.0 shipped without dark mode)." \
      "Remedy: make -C crates/${crate} release-prep && git add ${bundle_dir}"
  fi

  # Asset integrity: a digest match says the bundle was stamped against this
  # source, not that every file the mirror should have copied arrived. Resolve
  # every local asset the bundle's entry point references.
  local index_path ref_count=0 html refs ref
  index_path="$(printf '%s\n' "$bundle_files" | grep -x "${bundle_dir}/index.html" || true)"
  if [ -z "$index_path" ]; then
    fail "NO-INDEX — ${crate}: ${bundle_dir}/index.html is not tracked at ${REV}." \
      "The embedded UI has no entry point; the bundle cannot be serving anything."
  else
    html="$(g show "${REV}:${index_path}")"
    refs="$(printf '%s\n' "$html" \
      | grep -oE '(src|href)="[^"]+"' \
      | sed -E 's/^(src|href)="//; s/"$//' \
      | grep -v '^[a-zA-Z][a-zA-Z0-9+.-]*:' \
      | grep -v '^//' || true)"
    for ref in $refs; do
      ref_count=$((ref_count + 1))
      if ! resolve_asset_ref "$bundle_dir" "$ref" "$bundle_files" > /dev/null; then
        fail "ASSET-MISSING — ${crate}: index.html references '${ref}' but no matching file" \
          "is tracked under ${bundle_dir} at ${REV}. A partially-mirrored bundle" \
          "serves a blank page — index.html arrived, the hashed assets did not."
      fi
    done
    if [ "$ref_count" -eq 0 ]; then
      fail "NO-ASSET-REFS — ${crate}: ${index_path} references zero local assets." \
        "A Vite build always emits a script and a stylesheet reference; zero means" \
        "this check inspected nothing."
    fi
  fi

  TOTAL_SRC=$((TOTAL_SRC + src_count))
  TOTAL_BUNDLE=$((TOTAL_BUNDLE + bundle_count))
  TOTAL_HASH=$((TOTAL_HASH + hash_count))
  TOTAL_REFS=$((TOTAL_REFS + ref_count))
  ROWS_CHECKED=$((ROWS_CHECKED + 1))

  if [ "$FINDINGS" -eq "$row_findings_before" ]; then
    echo "PASS: ${crate} — ${src_count} source file(s) vs ${bundle_count} bundle file(s)," \
      "${hash_count} blob hash(es) bundle=${digest}, ${ref_count} asset ref(s) resolved;" \
      "recorded source digest ${src_digest:0:12} matches" >&2
  fi
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
MATCHED_ROW=0
while IFS="$(printf '\t')" read -r crate src_dir bundle_dir _rest; do
  [ -z "${crate:-}" ] && continue
  row_matches_target "$crate" || continue
  MATCHED_ROW=1
  if [ -z "${src_dir:-}" ] || [ -z "${bundle_dir:-}" ]; then
    fail "MANIFEST-STALE — ${crate}: row is missing a source_dir or bundle_dir column."
    continue
  fi
  check_row "$crate" "$src_dir" "$bundle_dir"
done <<EOF_ROWS
$MANIFEST_ROWS
EOF_ROWS

if [ -n "$TARGET_CRATE" ] && [ "$MATCHED_ROW" -eq 0 ]; then
  # Not-applicable, but VERIFIED: the discovery scan above already failed with
  # MANIFEST-GAP if this crate does track a bundle. Reaching here means it does
  # not, so there is nothing for the gate to compare.
  if [ "$FINDINGS" -eq 0 ]; then
    echo "check-ui-bundle-freshness: N/A — ${TARGET_CRATE} tracks no committed UI bundle" >&2
    echo "  (verified: no crates/${TARGET_CRATE}/{ui-dist,ui/dist} files at ${REV})" >&2
    exit 0
  fi
fi

if [ "$FINDINGS" -gt 0 ]; then
  echo "check-ui-bundle-freshness: FAILED (${FINDINGS} finding(s)) at ${REV} — do NOT publish." >&2
  exit 1
fi

if [ "$ROWS_CHECKED" -eq 0 ]; then
  echo "FAIL: NO-ROWS — zero crates were inspected at ${REV}. A gate that checks nothing" >&2
  echo "      cannot fail, which makes its green indistinguishable from not running." >&2
  exit 1
fi

echo "check-ui-bundle-freshness: OK at ${REV} — ${ROWS_CHECKED} crate(s), ${TOTAL_SRC} source file(s)," \
  "${TOTAL_BUNDLE} bundle file(s), ${TOTAL_HASH} blob hash(es), ${TOTAL_REFS} asset ref(s)." >&2
exit 0

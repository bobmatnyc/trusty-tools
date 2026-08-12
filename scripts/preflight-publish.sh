#!/usr/bin/env bash
#
# preflight-publish.sh — publish-safety guardrail (out-of-band / wrong-identity
# publish collision, 2026-07-08).
#
# Why: on 2026-07-08 a crate was published to crates.io out-of-band — from an
#   UNMERGED branch, under the WRONG gh account — burning crates.io version
#   0.22.0 with fix-less content. That version number can never be reused
#   (crates.io permanently rejects re-publishing a burned version), and every
#   downstream `cargo install` / dependent build silently picked up the
#   regressed build until the mistake was noticed. `scripts/check-publish-ready.sh`
#   (issue #2227) already gates "HEAD is an ancestor of origin/main" + "the
#   release tag is on main", but it does not check WHO is about to publish nor
#   whether the target version is already live — exactly the two gaps this
#   incident fell through. This script is a complementary, stricter preflight:
#   run it immediately before `cargo publish` and treat any nonzero exit as an
#   absolute stop.
#
# What: given a crate (by package name or crates/ directory name) and,
#   optionally, an explicit version, runs SEVEN independent checks and exits
#   nonzero if any of them fail:
#
#   CHECK 1 (merged-main): after `git fetch origin main`, current HEAD's
#     commit SHA must be EXACTLY `origin/main`'s HEAD SHA (`git rev-parse HEAD`
#     == `git rev-parse origin/main`) — not merely an ancestor. Publishing
#     must only happen from a checkout sitting AT merged main.
#
#       Override: PREFLIGHT_ALLOW_DETACHED=1 downgrades ONLY this check to a
#       WARN and continues. This exists for legitimate release worktrees
#       checked out at some other validated commit (e.g. a hotfix cherry-pick
#       tagged but not yet fast-forwarded locally). Risk: bypassing this check
#       means you are trusting — with no mechanical verification — that the
#       checked-out commit really is validated/intended for release. Misuse of
#       this flag is EXACTLY how the 2026-07-08 incident happened (publish ran
#       from a checkout that was never merged). Only ever set it when you can
#       point at the specific reason origin/main equality doesn't apply.
#
#   CHECK 2 (identity): `gh auth status` must report the ACTIVE account is
#     `bobmatnyc`. Any other active account (including a valid, logged-in,
#     but wrong, account such as a personal/work alias) fails with the exact
#     remedy `gh auth switch --user bobmatnyc`. No override exists for this
#     check — identity is never something a release should "trust the
#     operator" on.
#
#   CHECK 3 (clean tree): `git status --porcelain` must be empty. Gitignored
#     paths never appear in default `git status --porcelain` output, so a
#     plain check is sufficient; no special-casing for ui/node_modules or
#     ui/dist is added or needed.
#
#   CHECK 4 (version not already live): queries
#     `https://crates.io/api/v1/crates/<name>/<version>` (a descriptive
#     User-Agent is required — crates.io's API policy 403s generic/UA-less
#     clients). HTTP 200 means the version is already published — FAIL,
#     bump the version. HTTP 404 means the version isn't live yet — PASS.
#     Any other outcome (network error, unexpected status) fails CLOSED
#     (cannot verify safety, so refuse) rather than silently passing.
#
#   CHECK 5 (public-API SemVer, #5149): runs `scripts/check_semver.sh --crate
#     <pkg>`, which compares this crate's public API against its PREVIOUS
#     crates.io release — the greatest non-yanked version below the one about to
#     be published (#5296) — and fails when a breaking change is not carried by a
#     breaking version bump (Cargo's 0.x rule: for 0.y.z the MINOR position is
#     the breaking one). When the bump is already breaking there is no
#     requirement left to violate, and the run becomes an advisory INVENTORY of
#     what the release breaks (#5297); it reports, and cannot fail this check.
#
#     WHY IT LIVES HERE, and not only in CI: this script is the LAST thing that
#     runs before `cargo publish`, and its nonzero exit is the documented
#     absolute stop — so a break caught here is caught while the upload can
#     still be prevented. A crates.io publish is irreversible except by yank, so
#     a gate that only reports AFTER the upload has no way to undo the damage:
#     that is exactly #4088, where trusty-common 0.22.5 shipped a required new
#     public field on a patch bump and cost trusty-analyze 0.7.3 a yank.
#     `.github/workflows/semver-checks.yml` runs the same command on the tag
#     push, but a CI job cannot stop a `cargo publish` a human runs locally —
#     it reports, this blocks.
#
#     No override flag exists, and none is needed: the correct response to a
#     firing gate is to bump the breaking position, which the gate then records
#     as an already-breaking release and skips. A false positive and a real
#     break have the same safe remedy.
#
#     A NON-VERDICT IS NOT A VERDICT (#5289). check_semver.sh exits 1 only when
#     it computed a verdict that says break, and 3 when it could not compute one
#     at all (rustdoc build failure, a run that executed ZERO checks (#5440),
#     unreachable registry, missing tool). Both
#     stop the publish; only the first is reported as "your API changed". The
#     remedy above applies to exit 1 — for exit 3 the remedy is to fix the gate
#     and re-run, never to bump a version on evidence that does not exist.
#
#     Requires `cargo-semver-checks` (`cargo install cargo-semver-checks@0.50.0
#     --locked`). Its absence is a FAILURE with that remedy, never a skip.
#
#   CHECK 6 (tag names the publish commit): delegates to
#     `scripts/check-tag-publish-parity.sh`, which asserts that the release tag
#     `<crate>-v<version>` (or the accepted `tga-v<version>` alias, #1128) names
#     EXACTLY the commit this publish will ship.
#
#     WHY THIS IS NOT ALREADY COVERED by checks 1-5 or by
#     check-publish-ready.sh: nothing anywhere binds the tag to the upload.
#     check-publish-ready.sh's GUARD 2 asks only whether the tag's commit is an
#     ANCESTOR of origin/main; CHECK 1 above asks only whether HEAD EQUALS
#     origin/main. Both are satisfied when the tag sits several commits behind
#     HEAD — which is the state a release run lands in whenever main moves and
#     the run is fast-forwarded to satisfy CHECK 1. On 2026-08-11 that shipped
#     `tga-v2.17.0` tagged at 246e4ca2 while the published crate's
#     .cargo_vcs_info.json recorded 7d5cf82e1, with every gate green.
#     `git diff 246e4ca2 7d5cf82e1 -- crates/trusty-git-analytics/` is empty, so
#     that tag happens to misrepresent nothing; on a release where the
#     intervening commits touch the crate, `git checkout <tag>` shows a tree
#     that is not what shipped and nothing says so.
#
#     No override flag. The remedy is to move the tag onto the commit being
#     published (or reset the checkout back to the tag) — one of the two is
#     always correct, so there is no case an override would serve.
#
#   CHECK 7 (UI bundle freshness, #3606): delegates to
#     `scripts/check-ui-bundle-freshness.sh <package>`, which refuses to pass
#     when the crate's committed UI bundle was last built before its current UI
#     source. A crate with no committed bundle reports N/A and passes, verified
#     against the tree rather than assumed.
#
#     WHY THIS IS NOT ALREADY COVERED: nothing anywhere compares the two.
#     `cargo publish` ships whatever is committed under `ui-dist/` (listed in
#     trusty-search's `include`), `SKIP_UI_BUILD=1` short-circuits build.rs on
#     every release path, and the mirror step that refreshes the bundle
#     (`make -C crates/trusty-search sync-ui`) is human-remembered. Forget it
#     and every gate above still passes: the tree is clean, the tag is right,
#     the version is free, the public API is unchanged. trusty-search shipped
#     that exact state three times — v0.12.1, v0.13.1, and 0.37.0, which
#     published an admin dashboard with no dark mode because #3509's
#     tokens.css/theme-bootstrap.js rewrite never reached ui-dist/.
#     `.github/workflows/ci.yml` correctly declined a rebuild-then-diff (Vite's
#     content-hashed filenames are not byte-stable across toolchains), so this
#     compares CONTENT instead: each bundle carries ui-source-hash.txt, a digest
#     of the source it was built from, which the gate recomputes and compares.
#     No Node, no rebuild, nothing for a hash to make flaky. It compared commit
#     ancestry first, and a review laundered that in three commits — one
#     unrelated edit under the bundle directory cleared a still-stale bundle.
#
#     No override flag. The remedy is one command:
#     `make -C crates/<crate> release-prep` then commit the regenerated bundle.
#
# Crate + version resolution: accepts EITHER
#     scripts/preflight-publish.sh <crate-name-or-dir> [version]
#   or, when [version] is omitted, reads the version from that crate's
#   Cargo.toml (`crates/<dir>/Cargo.toml`, first `version = "X.Y.Z"` line).
#   Rationale: reading from Cargo.toml matches what `cargo publish` will
#   actually ship (the single source of truth), so the common case is just
#   `scripts/preflight-publish.sh trusty-mpm`. An explicit version argument is
#   still accepted for diagnostics/dry-run against a hypothetical version
#   (e.g. checking availability before bumping). <crate-name-or-dir> accepts
#   either the crates.io package name (e.g. `tga`) or the crates/ directory
#   name (e.g. `trusty-git-analytics`), resolved the same way
#   check-publish-ready.sh does, to avoid a second, divergent lookup
#   convention in this workspace.
#
#   --check-only     run all 7 checks unconditionally (never short-circuits)
#                     and print one [PASS]/[FAIL] line per check, then a
#                     one-line summary. Useful to preview status without
#                     assuming you are mid-publish. Exit code is still
#                     nonzero if any non-overridden check failed.
#   -h|--help         print this header and exit 0.
#
# Exit codes: 0 = all checks passed (or downgraded via PREFLIGHT_ALLOW_DETACHED
#   for check 1 only) — safe to `cargo publish`. Nonzero = at least one check
#   failed — DO NOT PUBLISH. 2 = usage error (bad arguments).
#
# Test: checks 1-4 are exercised manually — they are bound to the network, the
#   real crates.io registry, and the logged-in gh account, none of which a
#   fixture can stand in for. Check 5 has scripts/check_semver_selftest.sh and
#   check 6 has scripts/check-tag-publish-parity-selftest.sh, and check 7 has
#   scripts/check-ui-bundle-freshness-selftest.sh — all three drive every
#   failure branch of the delegated script against fixtures.
#   Verified by construction:
#     (a) FAIL mode — run from an unmerged feature branch (HEAD != origin/main)
#         to demonstrate check 1 failing.
#     (b) The identity-check string-compare logic
#         (`[[ "$active_account" == "bobmatnyc" ]]`) was exercised standalone
#         by feeding a faked `gh auth status`-shaped transcript containing a
#         DIFFERENT active account through the same parsing loop and
#         confirming it reports FAIL with the exact remedy text — without
#         ever running `gh auth switch` against the real, logged-in account.
#     (c) FAIL mode — pointed at a crate + version that IS already live on
#         crates.io, to demonstrate check 4 failing.
#     (d) PASS-mode — demonstrated per-check via a mix of a scratch checkout
#         at true origin/main (or PREFLIGHT_ALLOW_DETACHED=1, explicitly
#         labeled) for check 1, real `gh auth status` output for check 2, a
#         clean tree for check 3, and a not-yet-published version for check 4.
#     (e) BOTH modes for check 5 (#5149), against real source rather than a
#         synthetic break: `fix/5064-redb-flock-collision`'s trusty-review
#         source paired with version 0.11.1 (a MINOR bump over the published
#         0.11.0, so the already-breaking skip does not apply) FAILS on 2 major
#         lints — enum_marked_non_exhaustive on DedupError, and
#         method_receiver_type_changed on DedupStore::{claim,complete,release}
#         — and the script exits 1 with "do NOT run 'cargo publish'". The same
#         crate unmodified at 0.11.1 PASSES: 196 checks, 196 pass.
#     (f) BOTH modes for check 6, via
#         scripts/check-tag-publish-parity-selftest.sh: 15 cases over synthetic
#         repos covering TAG-MISSING, TAG-SPLIT, TAG-DRIFT (fast-forward AND
#         divergent), VCS-INFO-MISMATCH, and the clean/annotated-tag/alias
#         passes. Plus an end-to-end run of THIS script against `tga 2.17.0`,
#         where check 6 reports the real 2026-08-11 drift.
#     (g) BOTH modes for check 7 (#3606), via
#         scripts/check-ui-bundle-freshness-selftest.sh: 27 assertions over
#         synthetic repos covering BUNDLE-STALE in both bundle layouts, a forged
#         stamp, ASSET-MISSING, and the vacuous-scan refusals (MANIFEST-MISSING,
#         MANIFEST-STALE, MANIFEST-GAP, NO-SOURCES, STAMP-MISSING,
#         NO-ASSET-REFS, NO-INDEX). Case 19 is the laundering regression; case 20
#         proves the byte-identical-rebuild remedy leaves something to commit.
#         Case 18 runs against the real fc7f396f — the commit trusty-search
#         0.37.0 was published from — and names #3509's commit 972171e8 as the
#         source change the bundle never picked up.
#   See the PR description for this script for the full raw terminal output.

set -euo pipefail

# ---------------------------------------------------------------------------
# -h/--help (checked before repo-root resolution so it works from anywhere)
# ---------------------------------------------------------------------------
for arg in "$@"; do
  case "$arg" in
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Resolve repo root so the script works from any cwd.
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CRATE_UA="trusty-tools-preflight-publish (github.com/bobmatnyc/trusty-tools)"

usage() {
  echo "usage: scripts/preflight-publish.sh [--check-only] <crate-name-or-dir> [version]" >&2
  echo "       scripts/preflight-publish.sh -h|--help" >&2
  exit 2
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
CHECK_ONLY=0
POSITIONAL=()
for arg in "$@"; do
  case "$arg" in
    --check-only)
      CHECK_ONLY=1
      ;;
    -*)
      echo "preflight-publish: unknown argument: $arg" >&2
      usage
      ;;
    *)
      POSITIONAL+=("$arg")
      ;;
  esac
done

if [ "${#POSITIONAL[@]}" -lt 1 ] || [ "${#POSITIONAL[@]}" -gt 2 ]; then
  usage
fi

CRATE_INPUT="${POSITIONAL[0]}"
VERSION_ARG="${POSITIONAL[1]:-}"

# ---------------------------------------------------------------------------
# resolve_crate_dir: accept either the crates.io package name (e.g. `tga`) or
# the crates/ directory name (e.g. `trusty-git-analytics`). Mirrors
# check-publish-ready.sh's resolver so this workspace has one lookup
# convention, not two.
# ---------------------------------------------------------------------------
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

CRATE_DIR=""
if ! CRATE_DIR="$(resolve_crate_dir "$CRATE_INPUT")"; then
  echo "preflight-publish: ERROR: no crate found matching '${CRATE_INPUT}' (checked" >&2
  echo "  crates/${CRATE_INPUT}/ and every crate's 'name = ' field)" >&2
  exit 2
fi
MANIFEST="${REPO_ROOT}/crates/${CRATE_DIR}/Cargo.toml"

# Resolve the crates.io package name (may differ from the directory, e.g.
# trusty-git-analytics -> tga) so check 4 queries the correct API path.
PKG_NAME="$(grep -m1 -E '^name[[:space:]]*=[[:space:]]*"' "$MANIFEST" \
  | sed -E 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
if [ -z "$PKG_NAME" ]; then
  echo "preflight-publish: ERROR: could not read 'name' from ${MANIFEST}" >&2
  exit 2
fi

if [ -n "$VERSION_ARG" ]; then
  VERSION="$VERSION_ARG"
else
  VERSION="$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' "$MANIFEST" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
  if [ -z "$VERSION" ]; then
    echo "preflight-publish: ERROR: could not find version = \"X.Y.Z\" in ${MANIFEST}" >&2
    echo "  (pass an explicit version argument instead: scripts/preflight-publish.sh ${CRATE_INPUT} <version>)" >&2
    exit 2
  fi
fi

echo "preflight-publish: crate=${CRATE_DIR} package=${PKG_NAME} version=${VERSION}" >&2
[ "$CHECK_ONLY" -eq 1 ] && echo "preflight-publish: --check-only mode (all checks run unconditionally)" >&2

FAILURES=0

# ===========================================================================
# CHECK 1 — merged-main
# ===========================================================================
check1_merged_main() {
  if ! git fetch origin main --quiet 2>/dev/null; then
    echo "[FAIL] merged-main: 'git fetch origin main' failed — check network/remote access." >&2
    return 1
  fi
  local head_sha origin_sha
  head_sha="$(git rev-parse HEAD)"
  origin_sha="$(git rev-parse origin/main)"
  if [ "$head_sha" = "$origin_sha" ]; then
    echo "[PASS] merged-main: HEAD (${head_sha}) == origin/main (${origin_sha})" >&2
    return 0
  fi
  if [ "${PREFLIGHT_ALLOW_DETACHED:-0}" = "1" ]; then
    echo "[WARN] merged-main: HEAD (${head_sha}) != origin/main (${origin_sha}), but" >&2
    echo "       PREFLIGHT_ALLOW_DETACHED=1 — proceeding anyway. You are trusting that" >&2
    echo "       this checkout is genuinely validated for release. Misuse of this" >&2
    echo "       override is exactly how the 2026-07-08 incident happened." >&2
    return 0
  fi
  echo "[FAIL] merged-main: HEAD (${head_sha}) != origin/main (${origin_sha})." >&2
  echo "       Publishing must only happen from a checkout sitting AT merged main —" >&2
  echo "       never an unmerged branch. Merge to main, pull, and re-run from there." >&2
  echo "       Escape hatch (validated release worktrees only): PREFLIGHT_ALLOW_DETACHED=1" >&2
  return 1
}

# ===========================================================================
# CHECK 2 — identity (gh auth status must show bobmatnyc as the active account)
# ===========================================================================
check2_identity() {
  local gh_output
  if ! gh_output="$(gh auth status 2>&1)"; then
    # gh auth status exits nonzero when NOT logged in to any account at all;
    # its output (captured above) still explains why, so surface it.
    echo "[FAIL] identity: 'gh auth status' reported no active session:" >&2
    echo "${gh_output}" | sed 's/^/       /' >&2
    echo "       Remedy: gh auth switch --user bobmatnyc" >&2
    return 1
  fi
  identity_active_account_from_status "$gh_output"
  if [ "$IDENTITY_ACTIVE_ACCOUNT" = "bobmatnyc" ]; then
    echo "[PASS] identity: active gh account is bobmatnyc" >&2
    return 0
  fi
  echo "[FAIL] identity: active gh account is '${IDENTITY_ACTIVE_ACCOUNT:-<none found>}', not bobmatnyc." >&2
  echo "       Remedy: gh auth switch --user bobmatnyc" >&2
  return 1
}

# identity_active_account_from_status: parse `gh auth status` transcript text
# and set IDENTITY_ACTIVE_ACCOUNT to the login of whichever account block has
# "Active account: true". Isolated as its own function (rather than inlined)
# specifically so it can be exercised standalone against a faked transcript —
# see CHECK 2 verification in the header comment above.
IDENTITY_ACTIVE_ACCOUNT=""
identity_active_account_from_status() {
  local transcript="$1"
  local current="" active=""
  local line
  while IFS= read -r line; do
    case "$line" in
      *"Logged in to"*"account"*)
        # e.g. "  ✓ Logged in to github.com account bobmatnyc (keyring)"
        current="$(printf '%s\n' "$line" | sed -E 's/.*account[[:space:]]+([^[:space:]]+).*/\1/')"
        ;;
      *"Active account: true"*)
        active="$current"
        ;;
    esac
  done <<EOF_TRANSCRIPT
$transcript
EOF_TRANSCRIPT
  IDENTITY_ACTIVE_ACCOUNT="$active"
}

# ===========================================================================
# CHECK 3 — clean tree
# ===========================================================================
check3_clean_tree() {
  local dirty
  dirty="$(git status --porcelain)"
  if [ -z "$dirty" ]; then
    echo "[PASS] clean-tree: git status --porcelain is empty" >&2
    return 0
  fi
  echo "[FAIL] clean-tree: working tree is not clean:" >&2
  echo "$dirty" | sed 's/^/       /' >&2
  echo "       Commit, stash, or discard these changes before publishing." >&2
  return 1
}

# ===========================================================================
# CHECK 4 — version not already live on crates.io
# ===========================================================================
# TMP_BODY is created once, up front (see the mktemp+trap block below, right
# before the checks run), and cleaned up on exit — not inside this function —
# so the cleanup follows the same script-scoped `trap ... EXIT` convention as
# check_line_cap.sh rather than a per-function RETURN trap.
check4_version_not_live() {
  local url="https://crates.io/api/v1/crates/${PKG_NAME}/${VERSION}"
  local body http_code curl_rc

  http_code="$(curl -sS -A "$CRATE_UA" -o "$TMP_BODY" -w "%{http_code}" "$url")"
  curl_rc=$?
  body="$(cat "$TMP_BODY")"

  if [ "$curl_rc" -ne 0 ]; then
    echo "[FAIL] version-not-live: curl failed (rc=${curl_rc}) querying ${url}." >&2
    echo "       Cannot verify version safety — refusing to pass this check." >&2
    return 1
  fi

  case "$http_code" in
    200)
      echo "[FAIL] version-not-live: ${PKG_NAME} ${VERSION} is ALREADY LIVE on crates.io." >&2
      echo "       This is exactly the collision that burned 0.22.0 on 2026-07-08." >&2
      echo "       Bump the version in ${MANIFEST} before publishing." >&2
      return 1
      ;;
    404)
      echo "[PASS] version-not-live: ${PKG_NAME} ${VERSION} is not yet published (HTTP 404)" >&2
      return 0
      ;;
    *)
      echo "[FAIL] version-not-live: unexpected HTTP ${http_code} from ${url}:" >&2
      echo "$body" | sed 's/^/       /' >&2
      echo "       Cannot verify version safety — refusing to pass this check." >&2
      return 1
      ;;
  esac
}

# ===========================================================================
# CHECK 5 — public-API SemVer against the latest crates.io release (#5149)
# ===========================================================================
# Output goes to a file rather than the terminal because cargo-semver-checks
# prints a per-lint progress stream; the file is echoed only when the check
# fails, which is the only time any of it carries information.
#
# The two nonzero statuses are DIFFERENT FACTS and are reported as such (#5289).
# This function used to render every nonzero exit as "public-API check failed …
# publishing this would ship a breaking change", so a rustdoc build error at the
# last barrier before `cargo publish` read as a SemVer verdict. Exit 3 means
# check_semver.sh never computed one; saying "your API changed" there would be
# inventing a result, and telling the operator to bump the breaking position
# would be advising a version change on no evidence. Either way the publish
# still stops — both branches return 1.
check5_semver() {
  local log="${TMP_SEMVER}" rc=0

  SKIP_UI_BUILD=1 bash "${REPO_ROOT}/scripts/check_semver.sh" \
    --crate "$PKG_NAME" > "$log" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] semver: $(tail -1 "$log")" >&2
    return 0
  fi

  if [ "$rc" -eq 3 ]; then
    echo "[FAIL] semver: NO VERDICT for ${PKG_NAME} ${VERSION} — the gate could not run:" >&2
    sed 's/^/       /' "$log" >&2
    echo "       cargo-semver-checks never completed a comparison, so whether this" >&2
    echo "       release breaks the public API is UNKNOWN. That is not a reason to" >&2
    echo "       bump the version and not a reason to publish." >&2
    echo "       Fix the gate (see the NO SEMVER VERDICT block above), then re-run." >&2
    return 1
  fi

  echo "[FAIL] semver: public-API check failed for ${PKG_NAME} ${VERSION}:" >&2
  sed 's/^/       /' "$log" >&2
  echo "       Publishing this would ship a breaking change without a breaking" >&2
  echo "       version bump — the #4088 shape that yanked trusty-analyze 0.7.3." >&2
  echo "       Bump the breaking position in ${MANIFEST}, or make the change" >&2
  echo "       non-breaking (#[non_exhaustive] on public structs and enums)." >&2
  return 1
}

# ===========================================================================
# CHECK 6 — the release tag names the commit this publish ships
# ===========================================================================
# Delegated rather than inlined so the comparison has somewhere to be tested:
# scripts/check-tag-publish-parity-selftest.sh drives every failure branch
# against synthetic repos, which is not something this script's network- and
# identity-bound checks can be wrapped in.
check6_tag_parity() {
  local log="${TMP_PARITY}" rc=0

  bash "${REPO_ROOT}/scripts/check-tag-publish-parity.sh" \
    "$PKG_NAME" "$VERSION" > "$log" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] tag-parity: $(grep '^PASS:' "$log" | head -1)" >&2
    return 0
  fi

  echo "[FAIL] tag-parity: the release tag does not name the commit about to be published:" >&2
  sed 's/^/       /' "$log" >&2
  return 1
}

# ===========================================================================
# CHECK 7 — the committed UI bundle was built from the current UI source
# ===========================================================================
# #3606: cargo publish ships the committed bundle verbatim, so a forgotten
# `make release-prep` puts a UI built from deleted source on crates.io with
# every other gate green. Delegated rather than inlined so the comparison has
# somewhere to be tested: check-ui-bundle-freshness-selftest.sh drives every
# finding — including the refusals that keep an empty scan from reporting
# success — against synthetic repos.
check7_ui_bundle() {
  local log="${TMP_UIBUNDLE}" rc=0

  bash "${REPO_ROOT}/scripts/check-ui-bundle-freshness.sh" "$PKG_NAME" > "$log" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] ui-bundle: $(tail -1 "$log")" >&2
    return 0
  fi

  echo "[FAIL] ui-bundle: the committed UI bundle does not match the crate's UI source:" >&2
  sed 's/^/       /' "$log" >&2
  return 1
}

# ---------------------------------------------------------------------------
# Scratch temp file for check 4's curl response body. Created once up front
# and cleaned up via a script-scoped EXIT trap (matches check_line_cap.sh's
# convention: mktemp + trap 'rm -f ...' EXIT).
# ---------------------------------------------------------------------------
TMP_BODY="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.body.XXXXXX")"
TMP_SEMVER="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.semver.XXXXXX")"
TMP_PARITY="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.parity.XXXXXX")"
TMP_UIBUNDLE="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.uibundle.XXXXXX")"
trap 'rm -f "$TMP_BODY" "$TMP_SEMVER" "$TMP_PARITY" "$TMP_UIBUNDLE"' EXIT

# ---------------------------------------------------------------------------
# Run all 7 checks. Always run every check (rather than short-circuiting) so
# --check-only and normal mode share one code path and a single run always
# reports the full picture — a partial preflight is how gaps get missed.
# ---------------------------------------------------------------------------
set +e
check1_merged_main;      [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check2_identity;         [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check3_clean_tree;       [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check4_version_not_live; [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check5_semver;           [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check6_tag_parity;       [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check7_ui_bundle;        [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
set -e

if [ "$FAILURES" -gt 0 ]; then
  echo "preflight-publish: FAILED (${FAILURES} check(s) failed) — do NOT run 'cargo publish'." >&2
  exit 1
fi

echo "preflight-publish: OK — ${PKG_NAME} ${VERSION} passed all 7 checks. Safe to publish." >&2
echo "preflight-publish: after 'cargo publish', confirm what cargo actually recorded:" >&2
echo "  scripts/check-tag-publish-parity.sh --vcs-info auto ${PKG_NAME} ${VERSION}" >&2
exit 0

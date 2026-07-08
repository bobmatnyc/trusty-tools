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
#   optionally, an explicit version, runs FOUR independent checks and exits
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
#   --check-only     run all 4 checks unconditionally (never short-circuits)
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
# Test: exercised manually (this repo has no shell-test harness; see
#   check_line_cap.sh and check-publish-ready.sh for the same convention).
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

# ---------------------------------------------------------------------------
# Scratch temp file for check 4's curl response body. Created once up front
# and cleaned up via a script-scoped EXIT trap (matches check_line_cap.sh's
# convention: mktemp + trap 'rm -f ...' EXIT).
# ---------------------------------------------------------------------------
TMP_BODY="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.body.XXXXXX")"
trap 'rm -f "$TMP_BODY"' EXIT

# ---------------------------------------------------------------------------
# Run all 4 checks. Always run every check (rather than short-circuiting) so
# --check-only and normal mode share one code path and a single run always
# reports the full picture — a partial preflight is how gaps get missed.
# ---------------------------------------------------------------------------
set +e
check1_merged_main;      [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check2_identity;         [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check3_clean_tree;       [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check4_version_not_live; [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
set -e

if [ "$FAILURES" -gt 0 ]; then
  echo "preflight-publish: FAILED (${FAILURES} check(s) failed) — do NOT run 'cargo publish'." >&2
  exit 1
fi

echo "preflight-publish: OK — ${PKG_NAME} ${VERSION} passed all 4 checks. Safe to publish." >&2
exit 0

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
#   optionally, an explicit version, runs NINE independent checks and exits
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
#     A BREAK HAS NO OVERRIDE, and none is needed: the correct response to a
#     firing gate is to bump the breaking position, which the gate then records
#     as an already-breaking release and inventories. A false positive and a
#     real break have the same safe remedy.
#
#     A NON-VERDICT IS NOT A VERDICT (#5289). check_semver.sh exits 1 only when
#     it computed a verdict that says break, and 3 when it could not compute one
#     at all (rustdoc build failure, a run that executed ZERO checks (#5440),
#     unreachable registry, missing tool). Both
#     stop the publish; only the first is reported as "your API changed". The
#     remedy above applies to exit 1 — for exit 3 the remedy is to fix the gate
#     and re-run, never to bump a version on evidence that does not exist.
#
#     NEITHER IS EXIT 0 (#5620). The gate exits 0 on an ADVISORY run it could
#     not compute — an already-breaking release is permitted by its version
#     numbers whatever the run did — and CHECK 5 read that status alone, so
#     `0 crate(s) checked, 0 skipped, 1 inventory NOT computed` printed [PASS]
#     and trusty-review 0.16.0 shipped with its public-API delta unexamined by
#     any tool. The decision now reads the gate's counts, not just its status:
#     `0 compared` and [PASS] are unreachable together. Recorded skips print
#     [SKIP] and permit; a blind gate prints [FAIL] and stops unless
#     PREFLIGHT_SEMVER_UNVERIFIED names a reason, which prints [WARN] and
#     permits. See semver_decide below for why the reason is a string and why a
#     permanent capability gap belongs in the feature-exclusions TSV instead.
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
#   CHECK 8 (the pre-publish gate ran and was green for THIS commit):
#     enumerates recent `Pre-publish gate` workflow runs, asks each one which
#     commit it ACTUALLY gated, and requires a run that names the commit about
#     to be published to have concluded `success`.
#
#     IT DOES NOT ASK `head_sha`, AND THAT IS THE WHOLE POINT (#5755). It used
#     to. `head_sha` is the dispatched ref's tip at dispatch time, and the `sha`
#     input added in #5741 deliberately decouples the gated commit from that
#     tip — run 31874835425 records `head_sha` 3f39b79f having gated 020c139d.
#     So the old query counted a green run as evidence for a commit that run
#     never examined, which is CHECK 6's failure in a new place. The commit a
#     run gated comes from the run's own `resolve-sha` job, read back through
#     its `::notice title=Pre-publish gate target::` annotation.
#
#     WHY A LOCAL SCRIPT HAS TO ASK THIS. `.github/workflows/pre-publish.yml`
#     runs `cargo doc` (broken intra-doc links), `cargo audit`, `cargo deny`, the
#     `#[ignore]`d ONNX/embedder tests and the Code Contracts gates. Every one of
#     them guards something a publish makes permanent. But `cargo publish` runs
#     on a laptop, and a red workflow cannot reach out and stop it — which is
#     exactly the reasoning that put CHECK 5 in this script rather than leaving
#     SemVer to CI. A gate nobody is required to consult is a gate that gets
#     consulted right up until the release that matters.
#
#     IT FAILS CLOSED, and the failing states are DIFFERENT FACTS reported
#     differently. No run attributable to this SHA is [FAIL] "never ran" — not a
#     pass, because "we did not check" and "we checked and it was fine" are the
#     conflation this repo has been bitten by twice (#5620, #5723). A run that
#     gated this SHA and concluded anything other than success — a still-running
#     one included — is [FAIL] "red gate". An unreachable API is [FAIL] too: an
#     answer this check could not obtain is not a green one. A run whose target
#     resolved but could not be read back is the same: unknown, not green.
#
#     A RERUN RECORDS ITS ANSWER ON THE ATTEMPT THAT PRODUCED IT (#6113). After
#     `gh run rerun <id> --failed`, the jobs API's default-attempt view hands
#     back a carried-over copy of every job that did not re-execute — success,
#     no annotations. So this check walks back through earlier attempts when the
#     latest one reports nothing, rather than calling a green gate unreadable
#     and pushing the operator onto the override, which is what happened to
#     trusty-audit 0.7.0 on run 32355453111.
#
#     DISPATCH IT WITH AN EXPLICIT SHA. The remedy this check prints is
#
#         gh workflow run pre-publish.yml --ref <branch> -f sha=$(git rev-parse HEAD)
#
#     because without `-f sha` the run gates whatever the ref tip is when the
#     job starts, and `main` moving mid-release is exactly how the two commits
#     drift apart. Pinned that way, the run's own report names this commit and
#     this check finds it.
#
#     THE OVERRIDE TAKES A REASON, NEVER A BOOLEAN, matching
#     PREFLIGHT_SEMVER_UNVERIFIED:
#
#         PREFLIGHT_GATE_UNVERIFIED="pre-publish gate has never run against this
#                                    tag; dispatched manually and reviewed by hand"
#
#     echoed verbatim into a [WARN] line and into the final summary. `=1` records
#     that a publish bypassed the gate without recording why, and why is the
#     whole content of the disclosure.
#
#   CHECK 9 (the changelog assembler actually ran, #6406): delegates to
#     `scripts/check-changelog-assembled.sh <pkg> <version>`, which fails when
#     `crates/<crate>/changelog.d/` still holds a fragment (STRANDED-FRAGMENTS)
#     or `crates/<crate>/CHANGELOG.md` has no `## [<version>]` heading for the
#     version about to ship (NO-SECTION).
#
#     WHY THIS IS NOT ALREADY COVERED: none of checks 1-8 reads `changelog.d/`
#     or `CHANGELOG.md` at all. Six `trusty-audit` tags (0.8.0 -> 0.12.0) were
#     cut by hand-editing `Cargo.toml` directly — skipping
#     `scripts/bump-version.sh` and therefore `scripts/assemble-changelog.sh`,
#     the only thing that ever writes a release section or deletes a consumed
#     fragment. Fragments sat unconsumed across all six releases and every
#     other check here — identity, clean tree, tag/commit parity, public-API
#     SemVer, the UI bundle, the pre-publish gate — stayed green throughout
#     (#5919, repaired by hand in PR #6400).
#
#     No override flag. The remedy is always the same: run the real bump path
#     (`scripts/bump-version.sh <crate-dir> <major|minor|patch>`), which calls
#     the assembler for you, or assemble directly at the version you intend to
#     ship (`scripts/assemble-changelog.sh <crate-dir> <version>`).
#
#   CHECK 10 (the engagement template's sibling pins, #6772): trusty-audit only.
#     Runs `scripts/refresh-engagement-pins.sh --check`, which compares each
#     `[tools]` pin in `crates/trusty-audit/templates/engagement.template.toml`
#     with that package's current workspace version, then decides per stale pin
#     by asking crates.io whether the workspace version is published.
#
#     THE RULE: a pin must equal the sibling's current workspace version when
#     that version is NOT yet on crates.io — the sibling is shipping in this
#     same release train, so the pin is stale the moment the binary is built.
#     When the sibling's workspace version IS already published, the sibling is
#     not part of this train and a pin naming an older published version is a
#     legitimate engagement choice: reported as a WARN, never a block.
#
#     WHY THIS IS NOT ALREADY COVERED: the template is `include_str!`-ed into
#     `instructions::ENGAGEMENT_TEMPLATE` and written out verbatim by `taudit
#     distribute`, so the packaged copy is only as fresh as the binary — and
#     nothing in checks 1-9 reads it. At 7cfeda52d the template pinned tga
#     6.0.0 / trusty-analyze 0.12.5 / trusty-review 0.33.0 while that same train
#     published tga 7.0.0 / 0.12.6 / 0.33.1, with every other check green
#     (#6772; PR #6723 was the previous instance).
#
#     No override flag. The remedy is one command:
#     `scripts/refresh-engagement-pins.sh`, then commit the template.
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
#   --check-only     run all 10 checks unconditionally (never short-circuits)
#                     and print one [PASS]/[FAIL] line per check, then a
#                     one-line summary. Useful to preview status without
#                     assuming you are mid-publish. Exit code is still
#                     nonzero if any non-overridden check failed.
#   -h|--help         print this header and exit 0.
#
# Exit codes: 0 = all checks passed, or were downgraded by an override that
#   named itself in the output (PREFLIGHT_ALLOW_DETACHED for check 1,
#   PREFLIGHT_SEMVER_UNVERIFIED for check 5) — safe to `cargo publish`, with
#   whatever the WARN lines disclosed. Nonzero = at least one check failed —
#   DO NOT PUBLISH. 2 = usage error (bad arguments).
#
# Test: checks 1-4 are exercised manually — they are bound to the network, the
#   real crates.io registry, and the logged-in gh account, none of which a
#   fixture can stand in for. Check 5 has TWO: check_semver_selftest.sh drives
#   the delegated gate, and preflight-check5-selftest.sh drives THIS script's
#   decision over that gate's output — the half that was wrong in #5620 and the
#   half a four-minute rustdoc run had kept untested. Check 6 has
#   scripts/check-tag-publish-parity-selftest.sh and check 7 has
#   scripts/check-ui-bundle-freshness-selftest.sh; both drive every failure
#   branch of the delegated script against fixtures. Check 10 has
#   scripts/refresh-engagement-pins-selftest.sh, which drives the delegated
#   comparison over synthetic workspaces; THIS script's published/unpublished
#   decision over that comparison's output is bound to the live crates.io
#   registry, like checks 1-4, and is exercised by running the script.
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
#     (h) BOTH modes for check 9 (#6406), via
#         scripts/check-changelog-assembled-selftest.sh: synthetic fixtures for
#         a stranded fragment with no section (the #5919 shape), a section
#         written with a fragment still left behind, and a correctly-assembled
#         crate. Plus an end-to-end run of THIS script against the real,
#         post-#6400-repair `trusty-audit`, where check 9 reports OK.
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
# still stops.
#
# BUILD ACCELERATION. check_semver.sh is the only compiling step on this whole
# path, and when sccache is installed it runs its own `cargo semver-checks`
# subprocess under RUSTC_WRAPPER=sccache — see scripts/lib/build_accel.sh and the
# `build-accel:` line in the log. It is applied as an `env` prefix on that one
# command and never exported, so the `cargo publish` a human runs after this
# script passes inherits nothing from it. It cannot move a verdict: a wrapper
# changes which process invokes rustc, not what rustc is invoked on, and a cache
# that served a wrong object fails the build — which prints no summary line, and
# that is the NO VERDICT above rather than a skip. No sccache on the machine is
# byte-for-byte the previous behaviour.
check5_semver() {
  local log="${TMP_SEMVER}" rc=0 decision=0

  SKIP_UI_BUILD=1 bash "${REPO_ROOT}/scripts/check_semver.sh" \
    --crate "$PKG_NAME" > "$log" 2>&1 || rc=$?

  semver_decide "$rc" "$log" "$PKG_NAME" "$VERSION" || decision=$?

  # The type differ runs SECOND because it reads what the run above cached under
  # target/semver-checks/ — it builds nothing, so ordering is the whole cost.
  # Its outcome never changes `decision`; see semver_types_decide for why that
  # is chosen rather than incidental.
  semver_types_advisory "$PKG_NAME" "$SEMVER_GATE_COMPARED"

  return "$decision"
}

# ---------------------------------------------------------------------------
# semver_types_advisory <package> — run scripts/check_semver_types.sh over the
# rustdoc JSON check_semver.sh just cached, and report. ALWAYS returns 0.
#
# Why this exists: cargo-semver-checks 0.50.0 compares no types, so the check
# above passes a return type changing Vec<T> -> Result<Vec<T>> without comment —
# measured on tga 2.19.0 -> 2.19.1, which reported `223 checks: 223 pass`. The
# differ closes that, and until now nothing executed it: CHECK 5 named it in a
# [PASS] line and left running it to whoever read that line. A check nobody runs
# reports nothing, which is how it was possible for the differ to sit broken for
# its entire life without a single failing run to say so.
#
# WHY ADVISORY, deliberately and not by accident: the differ compares rendered
# types, and a lifetime rename or a re-export path shift is a real signature
# difference that no caller has to care about. Giving that a veto over `cargo
# publish` would buy a release-blocking gate its first false positive, and a
# release gate people learn to override is worth less than no gate. The value
# here is that it EXECUTES against a real crate every release and prints what it
# found; escalating it to a blocker is a separate decision with its own evidence.
semver_types_advisory() {
  local pkg="$1" gate_compared="${2:-0}" log rc=0

  # --- THE CACHE MUST BE ONE THIS RUN BUILT. Every SKIP branch in
  #     check_semver.sh `continue`s BEFORE invoking cargo-semver-checks, so a
  #     skipped crate gets no fresh rustdoc — TSV exclusion, publish = false, no
  #     library target, no baseline on crates.io, all seven of them. The differ
  #     reads target/semver-checks/ off the filesystem and cannot tell a
  #     directory this run wrote from one an out-of-band invocation left behind
  #     at the same version string. trusty-mpm is the live case: TSV-excluded and
  #     published through this very path, so a stale local-trusty_mpm-<ver>-*/
  #     would be diffed against source that is not HEAD and reported as [PASS].
  #
  #     This is a CALL-SITE CONDITION, not a filesystem heuristic — no mtime, no
  #     content hash, nothing to tune or go subtly wrong. The gate already knows
  #     whether it compared this crate; an mtime guard would be a second, weaker
  #     answer to a question already answered exactly. A run that compared
  #     nothing declines to read any cache at all.
  if [ "$gate_compared" -lt 1 ] 2>/dev/null || [ -z "$gate_compared" ]; then
    SEMVER_TYPES_ADVISORY="the type differ was not run — the gate compared nothing this run"
    echo "[WARN] semver-types: NOT RUN — the gate compared 0 crate(s) this run, so no" >&2
    echo "       rustdoc was built for ${pkg} and any cache on disk is left over from" >&2
    echo "       an earlier invocation. Reading it would compare source that is not" >&2
    echo "       HEAD and report the answer as though it were this release's." >&2
    echo "       Nothing is known about whether a type moved. Not a clean result, and" >&2
    echo "       not a blocker — CHECK 5 above already said what it did or did not" >&2
    echo "       verify, and its [SKIP]/[WARN] line is the one to read." >&2
    echo "       A TSV-excluded crate (scripts/semver-checks-crate-exclusions.tsv)" >&2
    echo "       can never produce a cache through the gate; comparing its types" >&2
    echo "       needs two rustdoc JSON documents built by hand and passed with" >&2
    echo "         bash scripts/check_semver_types.sh --baseline-json <a> --current-json <b>" >&2
    return 0
  fi

  log="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.semvertypes.XXXXXX")"
  bash "${REPO_ROOT}/scripts/check_semver_types.sh" --crate "$pkg" > "$log" 2>&1 || rc=$?

  semver_types_decide "$rc" "$log" "$pkg"
  rm -f "$log"
  return 0
}

# ---------------------------------------------------------------------------
# semver_types_decide <differ-exit> <differ-log> <package> — turn one differ run
# into output. Split from the run for the same reason semver_decide is: the
# decision is the part worth testing and the run needs a warm four-minute cache.
# Driven against captured differ output by scripts/preflight-check5-selftest.sh.
#
# THREE OUTCOMES, and the third is the one this function exists to keep
# distinct. #5620 is this repo's instance of "did not examine" and "found
# nothing wrong" being printed with the same word, and an advisory check is
# where that mistake is cheapest to repeat: a differ that cannot run costs
# nothing visible, so a silent skip would look exactly like a clean release
# forever.
#
#   [PASS] the differ ran and compared >= 1 position, and none changed.
#   [WARN] the differ ran and found type changes. Listed, publish PROCEEDS.
#   [WARN] the differ did NOT reach a verdict — cold cache, unreadable document,
#          a format_version it does not understand, or any other nonzero exit.
#          Says so in those words, and never borrows the PASS label.
#
# The count is read, not assumed. `compared: N public item(s); M changed` is the
# differ's own marker and a run that exits 0 without it has malfunctioned, so
# that lands in the third outcome rather than the first — 0 compared and [PASS]
# stay unreachable together, the invariant semver_decide already holds.
semver_types_decide() {
  local rc="$1" log="$2" pkg="$3"
  local marker compared changed

  marker="$(grep -E '^compared: [0-9]+ public item\(s\)' "$log" 2>/dev/null | tail -1)"
  compared="$(printf '%s' "$marker" | sed -n 's/^compared: \([0-9][0-9]*\) public item(s).*/\1/p')"
  changed="$(printf '%s' "$marker" | sed -n 's/.*; \([0-9][0-9]*\) changed.*/\1/p')"

  if [ -n "$compared" ] && [ "$compared" -ge 1 ] && [ -n "$changed" ]; then
    if [ "$rc" -eq 0 ] && [ "$changed" -eq 0 ]; then
      echo "[PASS] semver-types: ${compared} public item position(s) compared for ${pkg}, 0 type change(s)." >&2
      echo "       This is the check cargo-semver-checks cannot make; it ran and found nothing." >&2
      return 0
    fi

    if [ "$rc" -eq 1 ] && [ "$changed" -ge 1 ]; then
      SEMVER_TYPES_ADVISORY="${changed} public type change(s) across ${compared} compared position(s)"
      echo "[WARN] semver-types: ${changed} TYPE CHANGE(S) in ${pkg}'s public API, across" >&2
      echo "       ${compared} compared position(s). ADVISORY — the publish is not blocked." >&2
      grep -E '^CHANGED ' "$log" | sed 's/^/       /' >&2 || true
      echo "       cargo-semver-checks compares no types, so none of these appear in" >&2
      echo "       CHECK 5 above however strict it is. Each is source-breaking for a" >&2
      echo "       caller that named the old type." >&2
      echo "       Confirm every one was intended, and that the version bump carries" >&2
      echo "       them — 0.x crates in the MINOR position, 1.x+ in MAJOR." >&2
      return 0
    fi
  fi

  # --- NO VERDICT. Never [PASS], never a failure. The differ could not answer,
  #     and the only wrong move is letting that read as agreement.
  SEMVER_TYPES_ADVISORY="the type differ reached NO VERDICT (exit ${rc})"
  echo "[WARN] semver-types: NO VERDICT — the type differ did not run to a conclusion" >&2
  echo "       for ${pkg} (exit ${rc}). Nothing is known either way about whether a" >&2
  echo "       type moved. This is NOT a clean result, and it does not block the" >&2
  echo "       publish either: CHECK 5 above is the gate, this is advice that was" >&2
  echo "       unavailable." >&2
  sed 's/^/       /' "$log" >&2
  echo "       Usual causes: a cold target/semver-checks/ cache, a rustdoc" >&2
  echo "       format_version the differ does not list, or an unreadable document." >&2
  echo "       To get the advice back:" >&2
  echo "         bash scripts/check_semver.sh --crate ${pkg}   # warms the cache" >&2
  echo "         bash scripts/check_semver_types.sh --crate ${pkg}" >&2
  return 0
}

# ---------------------------------------------------------------------------
# semver_decide <gate-exit> <gate-log> <package> <version> — turn one
# check_semver.sh run into a publish decision. Returns 0 to permit, 1 to stop.
#
# Why this is a separate function from check5_semver: the decision is the part
# that was wrong, and running the real gate takes four minutes of rustdoc, so
# the decision had no test. Split out, it is driven against captured gate output
# by scripts/preflight-check5-selftest.sh — including the verbatim
# trusty-review 0.16.0 run this function exists because of.
#
# THE DEFECT (#5620, measured on the trusty-review 0.16.0 publish). CHECK 5 used
#   to read check_semver.sh's exit status and nothing else, so exit 0 printed
#
#       [PASS] semver: semver gate: scanned (explicit); 0 crate(s) checked,
#              0 skipped, 1 inventory NOT computed — OK.
#
#   and the publish proceeded. Underneath, cargo-semver-checks had exited 101
#   having compared nothing: trusty-review 0.15.0 cannot be documented, because
#   pipeline/mapreduce/reduce.rs imports a `profile`-gated item unconditionally,
#   so rustdoc never built the baseline. The gate reported that honestly on its
#   own line and in its summary — the loss was here, where "0 examined" and
#   "0 wrong" were rendered with the same word.
#
#   check_semver.sh is right to exit 0 there and that is deliberately unchanged:
#   whether an already-breaking release is PERMITTED is decided by the version
#   numbers, not by the advisory run, and a gate that reddened over a permitted
#   break would teach people to ignore it. What the advisory run carries is the
#   only coverage a 0.x MINOR bump ever gets — every one of them is major under
#   Cargo's rules, so the PASS/FAIL arm never fires and the inventory is it.
#   An inventory that could not be computed is therefore zero coverage, and
#   whether to publish on zero coverage is THIS script's question to answer.
#
# THE INVARIANT: `0 compared` and `[PASS]` are unreachable together. A pass
#   states how many crates it actually compared and refuses to print PASS when
#   that number is zero. Four outcomes, four labels, so a reader can always tell
#   "nothing was wrong" from "nothing was examined":
#
#     [PASS] >= 1 crate compared, and every lint that RAN passed. Narrower than
#            it reads, and the line says so: cargo-semver-checks 0.50.0 checks
#            existence, kind, parameter and generic COUNTS, attributes and trait
#            impls — it compares NO TYPES. Substitute any type and the item still
#            exists with the same name and arity, so every lint passes. Measured
#            against a 9-break probe crate at --release-type patch: 2 caught, 7
#            missed, all 7 type substitutions. trusty-common 0.32.0 -> 0.33.0
#            changed KgStoreRedb::count_active_triples from u64 to Result<u64>
#            and this gate reported `196 checks: 196 pass`.
#            scripts/check_semver_types.sh compares the types and now RUNS from
#            this check — semver_types_advisory below, on its own output line.
#            It is advisory by choice and cannot fail the publish.
#     [SKIP] 0 compared because no comparison was POSSIBLE — no baseline on
#            crates.io, no library target, or a row in
#            semver-checks-crate-exclusions.tsv. A fact about the crate, already
#            recorded in a reviewable file, so it permits without an override.
#            It is not a statement that the API is unchanged.
#     [WARN] 0 compared because the gate was BLIND, and PREFLIGHT_SEMVER_UNVERIFIED
#            named a reason to accept that. Permits; never prints PASS.
#     [FAIL] a computed break, a blind gate with no override, or a gate that
#            malfunctioned.
#
# THE OVERRIDE IS FOR SITUATIONAL BLINDNESS ONLY, and it takes a REASON, not a
#   boolean:
#
#       PREFLIGHT_SEMVER_UNVERIFIED="0.15.0 baseline references the profile
#                                    module removed in #5611"
#
#   echoed verbatim into the WARN line. `=1` records that a publish was allowed
#   but not why, and why is the entire content of the disclosure; a stale reason
#   string also reads as obviously stale where a stale `1` reads as normal. Set
#   with no reason, it is refused rather than honoured.
#
#   A PERMANENT CAPABILITY GAP IS NOT WHAT THIS IS FOR. When a machine class can
#   never build a crate's feature set — no CUDA for trusty-search's `cuda`
#   feature, no libdbus — the lever is a row in
#   scripts/semver-checks-feature-exclusions.tsv: durable, reviewable in a diff,
#   and greppable a year later. Route a standing gap through this variable and
#   within a week it lives in a Makefile target or a shell profile and the WARN
#   scrolls past every publish. An override that is always set is not an
#   override.
#
# A COMPUTED BREAK IS NEVER OVERRIDE-ABLE. The override answers "the gate could
#   not run"; exit 1 is the gate running and saying no. Its remedy is unchanged
#   and is not a variable.
#
# Test: scripts/preflight-check5-selftest.sh.
# ---------------------------------------------------------------------------
SEMVER_NOT_VERIFIED=""

# Set by semver_types_decide when the differ found changes or could not answer.
# Advisory only: it never contributes to FAILURES, it only keeps the final
# summary line from reading as though nothing was outstanding.
SEMVER_TYPES_ADVISORY=""

# How many crates the gate actually COMPARED this run, set by semver_decide.
# The type differ reads it to decide whether a cache exists that THIS run built.
# Starts at 0 so a semver_decide that never reached the count leaves it refusing.
SEMVER_GATE_COMPARED=0

semver_decide() {
  local rc="$1" log="$2" pkg="$3" version="$4"
  local summary checked skipped inventoried blind compared blind_why

  # --- A COMPUTED VERDICT THAT SAYS BREAK. Not override-able.
  if [ "$rc" -eq 1 ]; then
    echo "[FAIL] semver: public-API check failed for ${pkg} ${version}:" >&2
    sed 's/^/       /' "$log" >&2
    echo "       Publishing this would ship a breaking change without a breaking" >&2
    echo "       version bump — the #4088 shape that yanked trusty-analyze 0.7.3." >&2
    echo "       Bump the breaking position in ${MANIFEST:-the crate manifest}," >&2
    echo "       or make the change non-breaking (#[non_exhaustive] on public" >&2
    echo "       structs and enums). PREFLIGHT_SEMVER_UNVERIFIED does not apply to" >&2
    echo "       a verdict — it covers a gate that could not run, not one that ran." >&2
    return 1
  fi

  # --- THE GATE MALFUNCTIONED. 2 is a usage error and anything else is
  #     undocumented; both mean this script invoked the gate wrongly or the gate
  #     itself is broken. Those have a direct remedy, so no override applies.
  if [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ]; then
    echo "[FAIL] semver: check_semver.sh exited ${rc} for ${pkg} ${version}, which is" >&2
    echo "       not one of its documented statuses (0 clean, 1 break, 2 usage, 3 no verdict):" >&2
    sed 's/^/       /' "$log" >&2
    echo "       Nothing was compared. Fix the invocation or the gate, then re-run." >&2
    return 1
  fi

  # --- Read the counts out of the gate's own summary line, e.g.
  #       semver gate: scanned (explicit); 2 crate(s) checked, 1 skipped,
  #       1 inventoried (advisory), 1 inventory NOT computed — OK.
  #     Fails CLOSED: a summary this cannot parse is treated as blindness, so a
  #     future reword of that line turns CHECK 5 red rather than green.
  summary=""
  checked=""
  skipped=""
  inventoried=0
  blind=0
  if [ "$rc" -eq 0 ]; then
    summary="$(grep -E '[0-9]+ crate\(s\) checked' "$log" | tail -1 || true)"
    if [ -n "$summary" ]; then
      checked="$(printf '%s\n' "$summary" | sed -nE 's/.*[^0-9]([0-9]+) crate\(s\) checked.*/\1/p')"
      skipped="$(printf '%s\n' "$summary" | sed -nE 's/.*[^0-9]([0-9]+) skipped.*/\1/p')"
      inventoried="$(printf '%s\n' "$summary" | sed -nE 's/.*[^0-9]([0-9]+) inventoried.*/\1/p')"
      blind="$(printf '%s\n' "$summary" | sed -nE 's/.*[^0-9]([0-9]+) inventory NOT computed.*/\1/p')"
      inventoried="${inventoried:-0}"
      blind="${blind:-0}"
    fi
  fi

  if [ "$rc" -eq 3 ]; then
    blind_why="check_semver.sh reported NO VERDICT (exit 3) — it never completed a comparison"
  elif [ -z "$checked" ] || [ -z "$skipped" ]; then
    blind_why="check_semver.sh exited 0 but printed no summary line this script could read, so how much it compared is unknown"
  elif [ "$blind" -gt 0 ]; then
    blind_why="the advisory inventory could not be computed for ${blind} crate(s) — cargo-semver-checks completed no check run, which for an already-breaking 0.x release is the ONLY coverage there was"
  else
    blind_why=""
  fi

  # --- VERIFIED. Both arms compare: the PASS/FAIL arm runs the bump-requirement
  #     lints, the INVENTORY arm runs the full breaking-change lint set as
  #     advice. Either one examined the API.
  if [ -z "$blind_why" ]; then
    compared=$((checked + inventoried))
    SEMVER_GATE_COMPARED="$compared"
    if [ "$compared" -ge 1 ]; then
      echo "[PASS] semver: ${compared} crate(s) compared against their previous crates.io release." >&2
      echo "       Every existence-and-shape lint passed. NO TYPE WAS COMPARED HERE —" >&2
      echo "       cargo-semver-checks 0.50.0 has no lint that reads one, so a return" >&2
      echo "       type changing u64 -> Result<u64> passes this check. The semver-types" >&2
      echo "       line below is what looked at that, and it is advisory." >&2
      echo "       ${summary}" >&2
      return 0
    fi

    # --- NOTHING WAS COMPARABLE. Recorded skips only: no baseline exists, no
    #     library target, or an exclusion row. Permitted, never called a pass.
    if [ "$skipped" -ge 1 ]; then
      SEMVER_NOT_VERIFIED="0 crate(s) compared — ${skipped} recorded skip(s)"
      echo "[SKIP] semver: NOT VERIFIED — 0 crate(s) were compared for ${pkg} ${version}." >&2
      echo "       ${summary}" >&2
      grep -E '^SKIP ' "$log" | sed 's/^/       /' >&2 || true
      echo "       No comparison was POSSIBLE (no baseline on crates.io, no library" >&2
      echo "       target, or a row in semver-checks-crate-exclusions.tsv), so the" >&2
      echo "       publish is permitted. This is not a statement that the public API" >&2
      echo "       is unchanged — nothing looked at it." >&2
      return 0
    fi

    # 0 compared, 0 skipped, nothing blind: the gate reported on no crate at
    # all. Unreachable via --crate, and if it becomes reachable it is blindness.
    blind_why="check_semver.sh reported on no crate at all — ${summary}"
  fi

  # --- BLIND. Stop, unless an explicit reason says to accept it.
  if [ -n "${PREFLIGHT_SEMVER_UNVERIFIED+x}" ]; then
    if [ -z "$(printf '%s' "${PREFLIGHT_SEMVER_UNVERIFIED}" | tr -d '[:space:]')" ]; then
      echo "[FAIL] semver: PREFLIGHT_SEMVER_UNVERIFIED is set but empty." >&2
      echo "       This override records WHY an unverified publish was accepted, so it" >&2
      echo "       takes a reason, not a flag:" >&2
      echo "         PREFLIGHT_SEMVER_UNVERIFIED=\"<why the gate cannot compare this release>\"" >&2
      echo "       Refusing to proceed on an override with nothing in it." >&2
      return 1
    fi
    SEMVER_NOT_VERIFIED="0 crate(s) compared — overridden: ${PREFLIGHT_SEMVER_UNVERIFIED}"
    echo "[WARN] semver: UNVERIFIED — ${pkg} ${version} is being published without a" >&2
    echo "       public-API comparison, and PREFLIGHT_SEMVER_UNVERIFIED permits it." >&2
    echo "       Blind because: ${blind_why}." >&2
    echo "       Reason given: ${PREFLIGHT_SEMVER_UNVERIFIED}" >&2
    echo "       Nothing compared this release's API against its predecessor. If it" >&2
    echo "       breaks something unintentionally, no tool caught it — the reason" >&2
    echo "       above is the whole record of why that was accepted." >&2
    echo "       A gap that is a property of the MACHINE, not of this release" >&2
    echo "       (no CUDA, no libdbus), belongs in" >&2
    echo "       scripts/semver-checks-feature-exclusions.tsv instead. An override" >&2
    echo "       that is always set is not an override." >&2
    return 0
  fi

  echo "[FAIL] semver: NOT VERIFIED for ${pkg} ${version} — 0 crate(s) were compared:" >&2
  sed 's/^/       /' "$log" >&2
  echo "       Blind because: ${blind_why}." >&2
  echo "       Whether this release breaks the public API is UNKNOWN. That is not a" >&2
  echo "       reason to bump the version and not a reason to publish." >&2
  echo "       Fix the gate and re-run. If it cannot be fixed for THIS release," >&2
  echo "       publish with the reason recorded:" >&2
  echo "         PREFLIGHT_SEMVER_UNVERIFIED=\"<why>\" scripts/preflight-publish.sh ${pkg}" >&2
  echo "       If instead this machine can NEVER check this crate, add a row to" >&2
  echo "       scripts/semver-checks-feature-exclusions.tsv rather than overriding" >&2
  echo "       every publish." >&2
  return 1
}

# ===========================================================================
# Pre-tag guard (#6508) — FULL mode refuses to certify a run without a tag
# ===========================================================================
# Why: the canonical workflow tags and pushes BEFORE this script's full run
#   (`scripts/preflight-publish.sh <crate>`, no --check-only), so a CHECK 5
#   (semver) failure discovered here strands an already-pushed tag — tags on
#   this repo are IMMUTABLE (#6178). trusty-common 0.46.1 and 0.46.3 both
#   burned a version this week exactly this way. `--check-only` runs every
#   check, tag-parity included (see tagparity_decide below), and is the
#   MANDATORY gate before `git tag` — see .claude/skills/cargo-publish and
#   docs/reference/release-workflow.md.
#
#   This guard makes full mode refuse to double as that pre-tag check. It
#   does NOT short-circuit the run — every other check still executes, per
#   this file's existing "always run every check" design (below: "a partial
#   preflight is how gaps get missed") — it only ensures a missing tag costs
#   full mode a [FAIL] and the exact remedy, the same as any other check. A
#   no-op in --check-only mode, and a no-op once the tag has actually been
#   created — the corrected sequence never trips it.
full_mode_requires_tag() {
  [ "$CHECK_ONLY" -eq 1 ] && return 0
  local candidates="${CRATE_DIR}-v${VERSION}" tag
  if [ "$PKG_NAME" != "$CRATE_DIR" ]; then
    candidates="${candidates} ${PKG_NAME}-v${VERSION}"
  fi
  for tag in $candidates; do
    if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null 2>&1; then
      return 0
    fi
  done
  echo "[FAIL] tag-exists: no local tag (${candidates}) for ${PKG_NAME} ${VERSION} yet." >&2
  echo "       Full-mode preflight-publish.sh is the POST-TAG gate — CHECK 6 binds" >&2
  echo "       the tag to the commit about to publish, which is undefined before" >&2
  echo "       one exists. Run the pre-tag gate first, and only tag once it passes" >&2
  echo "       clean (#6508 — this is what keeps a preflight failure from ever" >&2
  echo "       stranding an already-pushed, immutable tag):" >&2
  echo "         scripts/preflight-publish.sh --check-only ${CRATE_INPUT}" >&2
  return 1
}

# ===========================================================================
# CHECK 6 — the release tag names the commit this publish ships
# ===========================================================================
# Delegated rather than inlined so the comparison has somewhere to be tested:
# scripts/check-tag-publish-parity-selftest.sh drives every failure branch
# against synthetic repos, which is not something this script's network- and
# identity-bound checks can be wrapped in.
#
# tagparity_decide is split out (rather than inlined in check6_tag_parity) so
# it can be extracted and driven over canned log content the same way
# semver_decide (CHECK 5) and gate_decide (CHECK 8) are — see
# preflight-check6-tag-gate-selftest.sh. In --check-only mode (#6508), a
# TAG-MISSING finding is the expected pre-tag state, not a failure: the whole
# point of --check-only is previewing the OTHER checks before the tag exists.
# TAG-SPLIT, TAG-DRIFT and VCS-INFO-MISMATCH all still fail in either mode —
# those mean a tag DOES exist and is wrong, never "as expected, not yet."
tagparity_decide() {
  local rc="$1" log="$2"

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] tag-parity: $(grep '^PASS:' "$log" | head -1)" >&2
    return 0
  fi

  if [ "$CHECK_ONLY" -eq 1 ] && grep -q '^FAIL: TAG-MISSING' "$log"; then
    echo "[SKIP] tag-parity: no release tag exists yet for ${PKG_NAME} ${VERSION} —" >&2
    echo "       expected before tagging. --check-only previews the checks that" >&2
    echo "       do not depend on a tag; tag/publish-commit parity is verified for" >&2
    echo "       real by the full, post-tag preflight run once the tag exists." >&2
    return 0
  fi

  echo "[FAIL] tag-parity: the release tag does not name the commit about to be published:" >&2
  sed 's/^/       /' "$log" >&2
  return 1
}

check6_tag_parity() {
  local log="${TMP_PARITY}" rc=0

  bash "${REPO_ROOT}/scripts/check-tag-publish-parity.sh" \
    "$PKG_NAME" "$VERSION" > "$log" 2>&1 || rc=$?

  tagparity_decide "$rc" "$log"
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

# ===========================================================================
# CHECK 8 — the pre-publish gate ran, and was green, for THIS exact commit
# ===========================================================================
# Delegated to `gh` rather than curl because the Actions API needs
# authentication and `gh` already holds the credential CHECK 2 just validated.
#
# THE DEFECT THIS SHAPE EXISTS TO PREVENT (#5755). This check used to ask the
# Actions API for runs whose `head_sha` is HEAD. `head_sha` is the dispatched
# ref's tip AT DISPATCH TIME, which since #5741 is no longer the commit a run
# gated: the `sha` input tells `resolve-sha` to check out an older commit, and
# every other job follows it. Measured on run 31874835425 — `head_sha`
# 3f39b79f, commit actually examined 020c139d. Keying on `head_sha` is
# therefore wrong in BOTH directions at once. It credits HEAD with a green run
# that examined some other commit, and it hides the run that did examine HEAD
# because that run's `head_sha` is a later tip.
#
# The first direction is the one that ships a bad release, and it is the same
# class as CHECK 6: a green result attributed to a commit nobody looked at is
# how `tga-v2.17.0` went out mis-tagged with every gate green.
#
# WHAT IT ASKS INSTEAD. Every `resolve-sha` job emits
# `::notice title=Pre-publish gate target::Verified commit <sha>` carrying the
# commit it resolved and verified — on both of its arms, so every run is
# attributable. That notice is readable as a check-run annotation. So this
# check enumerates recent runs, asks each one which commit IT says it gated,
# and counts only the runs that answer HEAD.
#
# IT STILL FAILS CLOSED, and the failing states stay DIFFERENT FACTS reported
# differently. No run attributable to HEAD is [FAIL] "never ran". A run
# attributable to HEAD that concluded anything but success — including one
# still in progress — is [FAIL] "red gate". A run whose `resolve-sha` job
# SUCCEEDED but whose target could not be read is neither: it is the absence
# of an answer, and it routes to gate_unverified. A run whose `resolve-sha`
# job did not succeed gated no commit at all and is ignored outright rather
# than counted as evidence in either direction.
#
# THE OVERRIDE TAKES A REASON, NEVER A BOOLEAN — unchanged, see the header.
GATE_NOT_VERIFIED=""
GATE_REPO="bobmatnyc/trusty-tools"

# How far back to look, and how many runs to open. The window is anchored to
# HEAD's own commit date because a run cannot have gated a commit that did not
# yet exist; two days of slack covers clock skew and a release that sits over a
# weekend. The cap bounds the API cost — each candidate costs two calls — and
# binding it is reported, never silently swallowed.
GATE_SCAN_DAYS=2
GATE_SCAN_CAP=40

# The display name of the job that resolves and reports the gated commit. It is
# the `name:` in pre-publish.yml, because that is what the jobs API returns —
# the YAML key `resolve-sha` never appears there.
GATE_JOB_NAME="Resolve target commit"

# How many earlier attempts the #6113 walk below will open. A run rerun more
# than a handful of times does not happen, and an unbounded walk would let one
# pathological run spend the whole API budget. Binding the cap yields
# UNREADABLE, never a green.
GATE_ATTEMPT_SCAN_CAP=5

# ---------------------------------------------------------------------------
# gate_api <path> — one authenticated read of the Actions API under
# repos/${GATE_REPO}, printing the raw JSON body. Nonzero exit means the read
# failed and callers route that to UNREADABLE.
#
# Split out from its callers so preflight-check8-selftest.sh can put captured
# API bodies in its place. The attempt walk below is the part #6113 got wrong,
# and a partially-rerun run is not something a test can conjure against the
# live API on demand.
# ---------------------------------------------------------------------------
gate_api() {
  gh api "repos/${GATE_REPO}/$1" 2>/dev/null
}

# ---------------------------------------------------------------------------
# gate_pick_job — stdin: a jobs-API body. Prints "<id>|<conclusion>|<attempt>"
# for the first job named GATE_JOB_NAME.
#
# Exit 0 found, 1 this body has no such job, 3 the body could not be read as a
# jobs list. Three exits because they are three different facts: no job means
# the run never gated anything, an unreadable body means nothing is known, and
# collapsing the second into the first would report an API error as evidence.
# ---------------------------------------------------------------------------
gate_pick_job() {
  python3 -c '
import json, sys
name = sys.argv[1]
try:
    jobs = json.load(sys.stdin)["jobs"]
    if not isinstance(jobs, list):
        raise ValueError("jobs is not a list")
except Exception:
    sys.exit(3)
for j in jobs:
    if isinstance(j, dict) and j.get("name") == name:
        print("%s|%s|%s" % (j.get("id") or "", j.get("conclusion") or "", j.get("run_attempt") or 1))
        sys.exit(0)
sys.exit(1)
' "$GATE_JOB_NAME" 2>/dev/null
}

# ---------------------------------------------------------------------------
# gate_annotation_sha — stdin: a check-run annotations body. Prints the 40-hex
# commit the "Pre-publish gate target" notice names.
#
# Exit 0 printed, 1 this body carries no such notice, 3 unreadable. Exit 1 is
# the #6113 state: a job copy carried over by a partial rerun answers `[]`.
# ---------------------------------------------------------------------------
gate_annotation_sha() {
  python3 -c '
import json, re, sys
try:
    anns = json.load(sys.stdin)
    if not isinstance(anns, list):
        raise ValueError("annotations is not a list")
except Exception:
    sys.exit(3)
for a in anns:
    if isinstance(a, dict) and a.get("title") == "Pre-publish gate target":
        m = re.match(r"^Verified commit ([0-9a-fA-F]{40})$", (a.get("message") or "").strip())
        if m:
            print(m.group(1))
            sys.exit(0)
sys.exit(1)
' 2>/dev/null
}

# ---------------------------------------------------------------------------
# gate_attempt_target <run-id> <attempt> — which commit did ONE attempt of this
# run report gating? An empty <attempt> reads the run's default (latest)
# attempt, which is the only view the jobs API serves without an attempt path.
#
# Prints "<verdict>|<attempt-number>". <verdict> carries gate_run_target's
# vocabulary below; <attempt-number> is the `run_attempt` the jobs API reported
# for that job, and is empty when nothing could be read.
# ---------------------------------------------------------------------------
gate_attempt_target() {
  local run_id="$1" attempt="${2:-}" path body row job_id concl at sha prc=0

  if [ -n "$attempt" ]; then
    path="actions/runs/${run_id}/attempts/${attempt}/jobs?per_page=100"
  else
    path="actions/runs/${run_id}/jobs?per_page=100"
  fi

  body="$(gate_api "$path")" || { printf 'UNREADABLE|\n'; return 0; }

  row="$(printf '%s' "$body" | gate_pick_job)" || prc=$?
  case "$prc" in
    0) : ;;
    1) printf 'NOGATE|\n'; return 0 ;;
    *) printf 'UNREADABLE|\n'; return 0 ;;
  esac

  IFS='|' read -r job_id concl at <<< "$row"

  if [ "$concl" != "success" ] || [ -z "$job_id" ]; then
    printf 'NOGATE|%s\n' "$at"
    return 0
  fi

  body="$(gate_api "check-runs/${job_id}/annotations")" \
    || { printf 'UNREADABLE|%s\n' "$at"; return 0; }

  sha="$(printf '%s' "$body" | gate_annotation_sha)" \
    || { printf 'UNREADABLE|%s\n' "$at"; return 0; }

  if [ -z "$sha" ]; then printf 'UNREADABLE|%s\n' "$at"; return 0; fi
  printf '%s|%s\n' "$sha" "$at"
}

# ---------------------------------------------------------------------------
# gate_run_target <run-id> — which commit did this run ACTUALLY gate?
#
# Prints exactly one of:
#   <40-hex sha>  the run's own resolve-sha job reported this commit
#   NOGATE        the resolve-sha job is absent or did not succeed, so this run
#                 never got as far as choosing a commit. Not evidence.
#   UNREADABLE    resolve-sha SUCCEEDED but its target could not be read.
#                 An answer that could not be obtained, which is not a green one.
#
# IT READS EARLIER ATTEMPTS WHEN THE LATEST ONE ANSWERS NOTHING (#6113). After
# `gh run rerun <id> --failed`, the jobs API's default-attempt view returns a
# CARRIED-OVER COPY of every job that was not re-executed: a fresh job id,
# `conclusion: success`, and zero annotations. Measured on run 32355453111,
# publishing trusty-audit 0.7.0 — attempt-1 job 96383547325 carries
# "Verified commit e0dfd8d7b…", and its attempt-2 copy 96395891848 carries
# `[]`. The commit that run gated is recorded only on the earlier attempt, so
# this asks that attempt rather than reporting a genuinely green gate as
# unreadable and pushing the operator onto the override.
#
# THE WALK STILL FAILS CLOSED, in both of its own directions. Nothing found
# across the scanned attempts stays UNREADABLE. Two attempts that name
# DIFFERENT commits also come back UNREADABLE: attempts of one run share its
# `sha` input and must resolve the same commit, so a disagreement means this is
# reading something other than what it thinks, and an answer that cannot be
# trusted is not a green one.
# ---------------------------------------------------------------------------
gate_run_target() {
  local run_id="$1" res verdict attempt seen a found="" conflict=0 scanned=0

  res="$(gate_attempt_target "$run_id" "")"
  verdict="${res%%|*}"
  attempt="${res#*|}"

  # A sha or NOGATE is this run's own answer, and the latest attempt is the one
  # entitled to give it. Only the absence of an answer reaches the walk.
  if [ "$verdict" != "UNREADABLE" ]; then printf '%s\n' "$verdict"; return 0; fi

  # No attempt number means the jobs list itself could not be read, so there is
  # no earlier attempt to address.
  case "$attempt" in
    ''|*[!0-9]*) printf 'UNREADABLE\n'; return 0 ;;
  esac

  a=$((attempt - 1))
  while [ "$a" -ge 1 ] && [ "$scanned" -lt "$GATE_ATTEMPT_SCAN_CAP" ]; do
    scanned=$((scanned + 1))
    res="$(gate_attempt_target "$run_id" "$a")"
    seen="${res%%|*}"
    a=$((a - 1))
    case "$seen" in
      NOGATE|UNREADABLE) continue ;;
    esac
    if [ -z "$found" ]; then
      found="$seen"
    elif [ "$found" != "$seen" ]; then
      conflict=1
    fi
  done

  if [ "$conflict" -ne 0 ] || [ -z "$found" ]; then printf 'UNREADABLE\n'; return 0; fi
  printf '%s\n' "$found"
}

# ---------------------------------------------------------------------------
# gate_decide <head-sha> <scan-was-capped> — turn the attributed-run table on
# stdin into a publish decision. Returns 0 to permit, 1 to stop.
#
# Separated from check8_prepublish_gate for the same reason semver_decide is:
# the decision is the part that was wrong, and exercising it against the real
# API needs a release-shaped history nobody can conjure on demand. Split out, it
# is driven over captured API output by scripts/preflight-check8-selftest.sh —
# including the verbatim run-31874835425 attribution this function exists
# because of.
#
# Input is one PIPE-separated line per candidate run:
#     <target>|<status>|<conclusion>|<html_url>
# where <target> is gate_run_target's output for that run.
#
# PIPE, NOT TAB, and this is load-free but not arbitrary: tab is IFS whitespace,
# so `read` collapses a RUN of tabs into one delimiter and an empty field
# silently vanishes. A queued or in-progress run has an empty `conclusion`, so
# with a tab delimiter its html_url shifted left into $concl and $url came back
# empty — measured on run 31878535281, which printed `in_progress/<the url>`.
# A non-whitespace delimiter preserves empty fields. No field can contain `|`:
# targets are hex or a keyword, statuses and conclusions are a fixed vocabulary,
# and a GitHub run URL has no pipe in it.
#
# THE INVARIANT: a [PASS] means a run said, in its own output, that it gated
# THIS commit — and no count of runs that merely mention it can substitute.
# ---------------------------------------------------------------------------
gate_decide() {
  local head="$1" capped="${2:-0}"
  local target status concl url
  local green=0 other=0 unreadable=0 attributed=0 matched=""

  # `|| [ -n "$target" ]` so a final row with no trailing newline is still
  # processed: read returns nonzero at EOF even when it filled the variables,
  # and silently dropping the last run would drop the newest one — the run the
  # operator just dispatched.
  while IFS='|' read -r target status concl url || [ -n "$target" ]; do
    [ -n "$target" ] || continue
    case "$target" in
      NOGATE) continue ;;
      UNREADABLE) unreadable=$((unreadable + 1)); continue ;;
    esac
    attributed=$((attributed + 1))
    [ "$target" = "$head" ] || continue
    matched="${matched}       ${status}/${concl:-<none>}  ${url}"$'\n'
    if [ "$status" = "completed" ] && [ "$concl" = "success" ]; then
      green=$((green + 1))
    else
      other=$((other + 1))
    fi
  done

  if [ "$green" -ge 1 ]; then
    echo "[PASS] prepublish-gate: ${green} green 'Pre-publish gate' run(s) gated ${head}." >&2
    echo "       Attributed by each run's own resolve-sha report, not by head_sha" >&2
    echo "       (#5755) — so a green run that examined a different commit cannot" >&2
    echo "       be counted here." >&2
    return 0
  fi

  if [ "$other" -ge 1 ]; then
    echo "[FAIL] prepublish-gate: 'Pre-publish gate' HAS run against ${head}, but NO" >&2
    echo "       run that gated it concluded 'success':" >&2
    printf '%s' "$matched" >&2
    echo "       Publishing over a red release gate is the CHECK 5 mistake in a new" >&2
    echo "       place: the gate ran, said no, and the upload happened anyway." >&2
    echo "       Fix what it found and re-run it. A still-running gate is not a" >&2
    echo "       green one — wait for it." >&2
    return 1
  fi

  if [ "$unreadable" -ge 1 ]; then
    gate_unverified "${unreadable} 'Pre-publish gate' run(s) resolved a target commit that could not be read back, so whether any of them gated ${head} is unknown"
    return $?
  fi

  if [ "$capped" != "0" ]; then
    gate_unverified "the scan stopped at its ${GATE_SCAN_CAP}-run cap before attributing every recent run, so 'no run gated ${head}' is a limit of the search, not a finding"
    return $?
  fi

  echo "[FAIL] prepublish-gate: NO 'Pre-publish gate' run gated ${head}." >&2
  echo "       ${attributed} recent run(s) were attributed to a commit; none named" >&2
  echo "       this one. The broken-link, cargo-audit, cargo-deny, ignored-test and" >&2
  echo "       contract gates have never executed against this tree. That is not the" >&2
  echo "       same as them passing, and this script will not treat it as such." >&2
  echo "       Remedy — dispatch it AT THIS COMMIT and wait for it:" >&2
  echo "         gh workflow run pre-publish.yml --ref \$(git rev-parse --abbrev-ref HEAD) \\" >&2
  echo "           -f sha=\$(git rev-parse HEAD)" >&2
  echo "       The explicit -f sha is what pins the run to this commit even after" >&2
  echo "       main moves underneath it; without it the run gates whatever the ref" >&2
  echo "       tip happens to be when the job starts." >&2
  echo "       If it genuinely cannot run for this release, record why:" >&2
  echo "         PREFLIGHT_GATE_UNVERIFIED=\"<why>\" scripts/preflight-publish.sh ${PKG_NAME}" >&2
  return 1
}

check8_prepublish_gate() {
  local sha runs cutoff head_date table="" capped=0 examined=0 rc=0
  local run_id created status concl url target

  if ! command -v gh >/dev/null 2>&1; then
    gate_unverified "the 'gh' CLI is not installed, so the gate's status could not be read"
    return $?
  fi

  sha="$(git rev-parse HEAD)"

  # `|| rc=$?` rather than a bare call: a network failure must reach the
  # unverified path below, not abort the script under `set -e`.
  runs="$(gh api "repos/${GATE_REPO}/actions/workflows/pre-publish.yml/runs?per_page=100" \
    --jq '.workflow_runs[] | [(.id|tostring), .created_at, .status, (.conclusion // ""), .html_url] | join("|")' 2>/dev/null)" || rc=$?

  if [ "$rc" -ne 0 ]; then
    gate_unverified "the GitHub Actions API could not be reached (gh exited ${rc})"
    return $?
  fi

  # A run cannot have gated a commit that did not exist when it started, so
  # anchor the window to HEAD's own date. An unparseable date yields an empty
  # cutoff, which disables the filter — scanning too much is the safe failure.
  head_date="$(TZ=UTC0 git show -s --format=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ HEAD 2>/dev/null || echo "")"
  cutoff="$(python3 -c '
import datetime, sys
d = datetime.datetime.fromisoformat(sys.argv[1].replace("Z", "+00:00"))
print((d - datetime.timedelta(days=int(sys.argv[2]))).strftime("%Y-%m-%dT%H:%M:%SZ"))
' "$head_date" "$GATE_SCAN_DAYS" 2>/dev/null || echo "")"

  while IFS='|' read -r run_id created status concl url; do
    [ -n "$run_id" ] || continue
    if [ -n "$cutoff" ] && [[ "$created" < "$cutoff" ]]; then continue; fi
    if [ "$examined" -ge "$GATE_SCAN_CAP" ]; then capped=1; break; fi
    examined=$((examined + 1))
    target="$(gate_run_target "$run_id")"
    table="${table}${target}|${status}|${concl}|${url}"$'\n'
    # Newest-first from the API, so the run just dispatched is normally the
    # first one opened. Stop there rather than paying two API calls per run for
    # a verdict that cannot change.
    if [ "$target" = "$sha" ] && [ "$status" = "completed" ] && [ "$concl" = "success" ]; then
      break
    fi
  done <<< "$runs"

  # Here-string, NOT a pipe: gate_decide can reach gate_unverified, which sets
  # GATE_NOT_VERIFIED for the final summary, and a pipeline would run it in a
  # subshell where that assignment is discarded — a publish that bypassed the
  # gate would then print no disclosure at the line an operator actually reads.
  gate_decide "$sha" "$capped" <<< "$table"
}

# gate_unverified <why> — the gate's status could not be READ (no gh, no
# network). Distinct from a red gate and from an absent one: those are answers,
# this is the absence of one. Permitted only with a recorded reason, on the same
# terms as PREFLIGHT_SEMVER_UNVERIFIED.
gate_unverified() {
  local why="$1"
  if [ -n "${PREFLIGHT_GATE_UNVERIFIED+x}" ]; then
    if [ -z "$(printf '%s' "${PREFLIGHT_GATE_UNVERIFIED}" | tr -d '[:space:]')" ]; then
      echo "[FAIL] prepublish-gate: PREFLIGHT_GATE_UNVERIFIED is set but empty." >&2
      echo "       This override records WHY a publish skipped the release gate, so" >&2
      echo "       it takes a reason, not a flag." >&2
      return 1
    fi
    GATE_NOT_VERIFIED="pre-publish gate NOT consulted — ${PREFLIGHT_GATE_UNVERIFIED}"
    echo "[WARN] prepublish-gate: UNVERIFIED — ${why}." >&2
    echo "       Reason given: ${PREFLIGHT_GATE_UNVERIFIED}" >&2
    echo "       Nothing confirmed the broken-link, audit, deny, ignored-test or" >&2
    echo "       contract gates ran against this commit." >&2
    return 0
  fi
  echo "[FAIL] prepublish-gate: could not determine whether the release gate passed —" >&2
  echo "       ${why}." >&2
  echo "       An answer this check could not obtain is not a green one." >&2
  echo "       If this release must proceed anyway, record why:" >&2
  echo "         PREFLIGHT_GATE_UNVERIFIED=\"<why>\" scripts/preflight-publish.sh ${PKG_NAME}" >&2
  return 1
}

# ===========================================================================
# CHECK 9 — the changelog assembler actually ran for this version (#6406)
# ===========================================================================
# Delegated rather than inlined so the comparison has somewhere to be tested:
# scripts/check-changelog-assembled-selftest.sh drives every finding against
# synthetic fixtures. No override — see the header comment for why.
check9_changelog_assembled() {
  local log="${TMP_CHANGELOG}" rc=0

  bash "${REPO_ROOT}/scripts/check-changelog-assembled.sh" "$PKG_NAME" "$VERSION" > "$log" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] changelog-assembled: $(tail -1 "$log")" >&2
    return 0
  fi

  echo "[FAIL] changelog-assembled: the changelog assembler was bypassed for ${PKG_NAME} ${VERSION}:" >&2
  sed 's/^/       /' "$log" >&2
  return 1
}

# ===========================================================================
# CHECK 10 — the engagement template's sibling pins (#6772), trusty-audit only
# ===========================================================================
# The comparison is delegated to scripts/refresh-engagement-pins.sh, which has
# its own self-test; what lives HERE is the one judgement that needs the
# network — whether a stale pin names a sibling that is shipping in this same
# release train. See the header comment for the rule and the #6772 history.
check10_engagement_pins() {
  if [ "$PKG_NAME" != "trusty-audit" ]; then
    echo "[PASS] engagement-pins: n/a — only trusty-audit compiles the engagement template." >&2
    return 0
  fi

  local log="${TMP_PINS}" rc=0
  bash "${REPO_ROOT}/scripts/refresh-engagement-pins.sh" --check > "$log" 2>&1 || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "[PASS] engagement-pins: every [tools] pin names its crate's workspace version." >&2
    return 0
  fi

  # Any exit but 1 means the gate could not READ the pins (missing template,
  # unreadable [tools] table, cargo metadata failure). Fail closed — an
  # unreadable table is exactly the state a silent pass would hide.
  if [ "$rc" -ne 1 ]; then
    echo "[FAIL] engagement-pins: could not read the template's [tools] pins (rc=${rc}):" >&2
    sed 's/^/       /' "$log" >&2
    return 1
  fi

  local blocking="" lagging="" unverified=""
  local name pinned wanted http url
  while read -r _tag name pinned_kv wanted_kv; do
    pinned="${pinned_kv#pinned=}"
    wanted="${wanted_kv#workspace=}"
    url="https://crates.io/api/v1/crates/${name}/${wanted}"
    http="$(curl -sS -A "$CRATE_UA" -o "$TMP_PIN_BODY" -w "%{http_code}" "$url")" || http="000"
    case "$http" in
      404)
        blocking="${blocking}         ${name}: pinned ${pinned}, workspace ${wanted} — ${wanted} is NOT published, so it ships with this train"$'\n'
        ;;
      200)
        lagging="${lagging}         ${name}: pinned ${pinned}, workspace ${wanted} — ${wanted} is already published, so this pin may lag"$'\n'
        ;;
      *)
        unverified="${unverified}         ${name}: HTTP ${http} from ${url}"$'\n'
        ;;
    esac
  done < <(grep '^STALE ' "$log" || true)

  if [ -n "$unverified" ]; then
    echo "[FAIL] engagement-pins: crates.io would not say whether a stale pin's sibling" >&2
    echo "       is shipping in this train:" >&2
    printf '%s' "$unverified" >&2
    echo "       Cannot verify pin freshness — refusing to pass this check." >&2
    return 1
  fi

  if [ -n "$blocking" ]; then
    echo "[FAIL] engagement-pins: crates/trusty-audit/templates/engagement.template.toml" >&2
    echo "       pins a sibling BEHIND the version shipping in this same release train:" >&2
    printf '%s' "$blocking" >&2
    echo "       THE RULE: a [tools] pin must equal the sibling's current workspace" >&2
    echo "       version when that version is not yet on crates.io — the sibling is" >&2
    echo "       about to ship beside this publish, so the pin is stale the moment the" >&2
    echo "       binary is built. When the sibling's workspace version IS already" >&2
    echo "       published it is not part of this train, and a pin naming an older" >&2
    echo "       published version is a legitimate engagement choice (WARN, not FAIL)." >&2
    echo "       This matters because the template is include_str!-ed into" >&2
    echo "       instructions::ENGAGEMENT_TEMPLATE and written out verbatim by" >&2
    echo "       'taudit distribute', so a stale pin ships inside the binary (#6772)." >&2
    echo "       Fix: scripts/refresh-engagement-pins.sh, then commit the template and" >&2
    echo "       rebuild." >&2
    return 1
  fi

  echo "[WARN] engagement-pins: pin(s) lag a sibling that is NOT in this release train:" >&2
  printf '%s' "$lagging" >&2
  echo "       Permitted — each named version is already published, so the pin is a" >&2
  echo "       deliberate engagement choice rather than release drift. Run" >&2
  echo "       scripts/refresh-engagement-pins.sh if you meant to track the workspace." >&2
  return 0
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
TMP_CHANGELOG="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.changelog.XXXXXX")"
TMP_PINS="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.pins.XXXXXX")"
TMP_PIN_BODY="$(mktemp "${TMPDIR:-/tmp}/preflight-publish.pinbody.XXXXXX")"
trap 'rm -f "$TMP_BODY" "$TMP_SEMVER" "$TMP_PARITY" "$TMP_UIBUNDLE" "$TMP_CHANGELOG" "$TMP_PINS" "$TMP_PIN_BODY"' EXIT

# ---------------------------------------------------------------------------
# Run all 10 checks. Always run every check (rather than short-circuiting) so
# --check-only and normal mode share one code path and a single run always
# reports the full picture — a partial preflight is how gaps get missed.
# ---------------------------------------------------------------------------
set +e
check1_merged_main;         [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check2_identity;            [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check3_clean_tree;          [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check4_version_not_live;    [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check5_semver;              [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
full_mode_requires_tag;     [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check6_tag_parity;          [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check7_ui_bundle;           [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check8_prepublish_gate;     [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check9_changelog_assembled; [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
check10_engagement_pins;    [ $? -eq 0 ] || FAILURES=$((FAILURES + 1))
set -e

if [ "$FAILURES" -gt 0 ]; then
  echo "preflight-publish: FAILED (${FAILURES} check(s) failed) — do NOT run 'cargo publish'." >&2
  exit 1
fi

if [ -n "${SEMVER_NOT_VERIFIED:-}" ]; then
  # #5620: "passed all 7 checks" must not absorb a check-5 outcome that verified
  # nothing. The same distinction the check line draws, drawn again at the line
  # an operator is most likely to read on its own.
  echo "preflight-publish: OK — ${PKG_NAME} ${VERSION} passed all 10 checks, but the" >&2
  echo "  public API was NOT VERIFIED: ${SEMVER_NOT_VERIFIED}. See the check 5 line above." >&2
elif [ -n "${SEMVER_TYPES_ADVISORY:-}" ]; then
  # The type differ blocks nothing, so without this the summary would say
  # "safe to publish" over a listed set of type changes nobody has confirmed.
  echo "preflight-publish: OK — ${PKG_NAME} ${VERSION} passed all 10 checks." >&2
  echo "  ADVISORY, not blocking: ${SEMVER_TYPES_ADVISORY}. See the semver-types line above." >&2
else
  echo "preflight-publish: OK — ${PKG_NAME} ${VERSION} passed all 10 checks. Safe to publish." >&2
fi
if [ -n "${GATE_NOT_VERIFIED:-}" ]; then
  # Same reasoning as the SEMVER_NOT_VERIFIED line above: "passed all 10 checks"
  # must not absorb a check-8 outcome that read nothing.
  echo "preflight-publish: NOTE — ${GATE_NOT_VERIFIED}. See the check 8 line above." >&2
fi
echo "preflight-publish: after 'cargo publish', confirm what cargo actually recorded:" >&2
echo "  scripts/check-tag-publish-parity.sh --vcs-info auto ${PKG_NAME} ${VERSION}" >&2
exit 0

#!/usr/bin/env bash
#
# preflight-check6-tag-gate-selftest.sh — the pre-tag guard around preflight
# CHECK 6 (#6508).
#
# Why: the canonical workflow tags and pushes BEFORE preflight-publish.sh's
#   full run, so a CHECK 5 (semver) failure discovered there strands an
#   already-pushed tag — tags on this repo are IMMUTABLE (#6178).
#   trusty-common 0.46.1 and 0.46.3 both burned a version this week exactly
#   this way. The fix is procedural: `scripts/preflight-publish.sh
#   --check-only <crate>` is now the MANDATORY gate before `git tag` (see
#   .claude/skills/cargo-publish and docs/reference/release-workflow.md).
#   Two decisions in preflight-publish.sh make that hold together:
#
#     tagparity_decide()      CHECK 6 itself. --check-only must be able to
#                              PASS before a tag exists — a TAG-MISSING
#                              finding there is the expected pre-tag state,
#                              not a failure. Before this fix, --check-only
#                              always failed pre-tag (CHECK 6 always found no
#                              tag), which made the new mandatory gate
#                              impossible to satisfy honestly.
#
#     full_mode_requires_tag() the new guard. FULL mode (no --check-only)
#                              must still refuse to certify a run with no
#                              tag — it is the post-tag gate, and a missing
#                              tag there means the canonical sequence was not
#                              followed.
#
# What: drives both functions directly — tagparity_decide over canned
#   rc/log content (no shell-out, no network), full_mode_requires_tag against
#   a real scratch git repo (it calls `git rev-parse --verify refs/tags/...`
#   directly, so a real repo is cheaper than mocking git). Both are lifted out
#   of preflight-publish.sh BY PATTERN (the same awk-extraction
#   preflight-check5-selftest.sh and preflight-check8-selftest.sh use), so
#   this exercises the shipped definitions rather than a copy that can drift.
#
#   Cases:
#     1.  tag-parity clean            rc=0. [PASS] in either mode.
#     2.  TAG-MISSING, --check-only   the expected pre-tag state. [SKIP],
#                                     and the overall decision still permits
#                                     (return 0) — this is the case that used
#                                     to make --check-only unsatisfiable
#                                     pre-tag.
#     3.  TAG-MISSING, full mode      [FAIL] — full mode never treats a
#                                     missing tag as expected.
#     4.  TAG-SPLIT, --check-only     a tag DOES exist and disagrees with its
#                                     alias. Must still [FAIL] even in
#                                     --check-only — this is not the
#                                     expected-pre-tag shape the SKIP exists
#                                     for.
#     5.  TAG-DRIFT, --check-only     same reasoning: a real tag naming the
#                                     wrong commit is a real problem, not a
#                                     pre-tag preview artifact.
#     6.  tag exists locally          full_mode_requires_tag passes once the
#                                     candidate tag is present.
#     7.  tag missing locally         full_mode_requires_tag fails, and names
#                                     the exact --check-only remedy.
#     8.  --check-only is a no-op     full_mode_requires_tag always passes in
#                                     --check-only mode, tag or no tag — it
#                                     exists to gate FULL mode only.
#     9.  package-name alias          a tga-style crate (PKG_NAME != CRATE_DIR)
#                                     passes on either candidate tag name.
#
# Usage:  bash scripts/preflight-check6-tag-gate-selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). No cargo,
# no network.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_TOP="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
UNDER_TEST="${PREFLIGHT_SELFTEST_SCRIPT:-${REPO_TOP}/scripts/preflight-publish.sh}"

if [ ! -r "$UNDER_TEST" ]; then
  echo "preflight-check6-tag-gate-selftest: cannot read ${UNDER_TEST}" >&2
  exit 1
fi

PASSED=0
FAILED=0

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# ---------------------------------------------------------------------------
# run_tagparity_decide <rc> <log-content> <check-only> -> "<exit>|<stderr>"
# ---------------------------------------------------------------------------
run_tagparity_decide() {
  local rc="$1" content="$2" check_only="$3" log out xrc
  log="$(mktemp "${TMPDIR:-/tmp}/preflight-check6-gate.XXXXXX")"
  printf '%s\n' "$content" > "$log"
  out="$(
    set +e
    PKG_NAME="trusty-selftest"
    VERSION="9.9.9"
    CHECK_ONLY="$check_only"
    eval "$(awk '/^tagparity_decide\(\) \{/,/^\}/' "$UNDER_TEST")"
    tagparity_decide "$rc" "$log" 2>&1 >/dev/null
    printf '\nEXIT=%s' "$?"
  )"
  rm -f "$log"
  xrc="$(printf '%s' "$out" | sed -n 's/^EXIT=//p' | tail -n1)"
  out="$(printf '%s' "$out" | grep -v '^EXIT=' | tr '\n' ' ' | tr -s ' ')"
  printf '%s|%s' "${xrc:-?}" "$out"
}

status_of() { printf '%s' "$1" | cut -d'|' -f1; }
text_of() { printf '%s' "$1" | cut -d'|' -f2-; }

echo "tagparity_decide:"

# --- 1. clean --------------------------------------------------------------
raw="$(run_tagparity_decide 0 "PASS: trusty-common-v9.9.9 names abc123, which is the commit this publish ships." 1)"
if [ "$(status_of "$raw")" = "0" ] && case "$(text_of "$raw")" in *"[PASS]"*) true ;; *) false ;; esac; then
  pass_case "1 clean: PASS"
else
  fail_case "1 clean: PASS" "$raw"
fi

# --- 2. TAG-MISSING, --check-only: SKIP and permit --------------------------
raw="$(run_tagparity_decide 1 "FAIL: TAG-MISSING — no release tag resolves for trusty-selftest 9.9.9." 1)"
if [ "$(status_of "$raw")" = "0" ] && case "$(text_of "$raw")" in *"[SKIP]"*) true ;; *) false ;; esac; then
  pass_case "2 TAG-MISSING + check-only: SKIP, permits"
else
  fail_case "2 TAG-MISSING + check-only: SKIP, permits" "$raw"
fi

# --- 3. TAG-MISSING, full mode: FAIL ----------------------------------------
raw="$(run_tagparity_decide 1 "FAIL: TAG-MISSING — no release tag resolves for trusty-selftest 9.9.9." 0)"
if [ "$(status_of "$raw")" = "1" ] && case "$(text_of "$raw")" in *"[FAIL]"*) true ;; *) false ;; esac; then
  pass_case "3 TAG-MISSING + full mode: FAIL"
else
  fail_case "3 TAG-MISSING + full mode: FAIL" "$raw"
fi

# --- 4. TAG-SPLIT, --check-only: still FAIL ---------------------------------
raw="$(run_tagparity_decide 1 "FAIL: TAG-SPLIT — the accepted alias tags name DIFFERENT commits." 1)"
if [ "$(status_of "$raw")" = "1" ] && case "$(text_of "$raw")" in *"[FAIL]"*) true ;; *) false ;; esac; then
  pass_case "4 TAG-SPLIT + check-only: still FAIL"
else
  fail_case "4 TAG-SPLIT + check-only: still FAIL" "$raw"
fi

# --- 5. TAG-DRIFT, --check-only: still FAIL ---------------------------------
raw="$(run_tagparity_decide 1 "FAIL: TAG-DRIFT — the tag names a commit other than HEAD." 1)"
if [ "$(status_of "$raw")" = "1" ] && case "$(text_of "$raw")" in *"[FAIL]"*) true ;; *) false ;; esac; then
  pass_case "5 TAG-DRIFT + check-only: still FAIL"
else
  fail_case "5 TAG-DRIFT + check-only: still FAIL" "$raw"
fi

echo "full_mode_requires_tag:"

# ---------------------------------------------------------------------------
# run_full_mode_requires_tag <crate-dir> <pkg-name> <version> <check-only> \
#   <make-tag: 0|1> -> "<exit>|<stderr>"
#
# Builds a real scratch git repo (the function shells out to `git
# rev-parse`), optionally creates the candidate tag, then drives the
# extracted function inside it.
# ---------------------------------------------------------------------------
# shellcheck disable=SC2034  # CRATE_DIR/PKG_NAME/VERSION/CRATE_INPUT/CHECK_ONLY
# are read by full_mode_requires_tag, eval'd below, which shellcheck cannot see into.
run_full_mode_requires_tag() {
  local crate_dir="$1" pkg_name="$2" version="$3" check_only="$4" make_tag="$5"
  local repo out xrc
  repo="$(mktemp -d "${TMPDIR:-/tmp}/preflight-check6-gate-repo.XXXXXX")"
  (
    cd "$repo" || exit 1
    git init -q .
    git config user.email "selftest@example.com"
    git config user.name "selftest"
    printf 'x\n' > file.txt
    git add file.txt
    git commit -q -m init
    if [ "$make_tag" -eq 1 ]; then
      if [ "$pkg_name" != "$crate_dir" ]; then
        git tag "${pkg_name}-v${version}"
      else
        git tag "${crate_dir}-v${version}"
      fi
    fi
  ) >/dev/null 2>&1
  out="$(
    cd "$repo" || exit 1
    set +e
    CRATE_DIR="$crate_dir"
    PKG_NAME="$pkg_name"
    VERSION="$version"
    CRATE_INPUT="$crate_dir"
    CHECK_ONLY="$check_only"
    eval "$(awk '/^full_mode_requires_tag\(\) \{/,/^\}/' "$UNDER_TEST")"
    full_mode_requires_tag 2>&1 >/dev/null
    printf '\nEXIT=%s' "$?"
  )"
  rm -rf "$repo"
  xrc="$(printf '%s' "$out" | sed -n 's/^EXIT=//p' | tail -n1)"
  out="$(printf '%s' "$out" | grep -v '^EXIT=' | tr '\n' ' ' | tr -s ' ')"
  printf '%s|%s' "${xrc:-?}" "$out"
}

# --- 6. tag exists locally: passes -------------------------------------------
raw="$(run_full_mode_requires_tag trusty-selftest trusty-selftest 9.9.9 0 1)"
if [ "$(status_of "$raw")" = "0" ]; then
  pass_case "6 tag exists: passes"
else
  fail_case "6 tag exists: passes" "$raw"
fi

# --- 7. tag missing locally: fails, names the --check-only remedy ------------
raw="$(run_full_mode_requires_tag trusty-selftest trusty-selftest 9.9.9 0 0)"
if [ "$(status_of "$raw")" = "1" ] && case "$(text_of "$raw")" in *"--check-only trusty-selftest"*) true ;; *) false ;; esac; then
  pass_case "7 tag missing: fails, names remedy"
else
  fail_case "7 tag missing: fails, names remedy" "$raw"
fi

# --- 8. --check-only is always a no-op, tag or no tag ------------------------
raw="$(run_full_mode_requires_tag trusty-selftest trusty-selftest 9.9.9 1 0)"
if [ "$(status_of "$raw")" = "0" ]; then
  pass_case "8 check-only, no tag: no-op, passes"
else
  fail_case "8 check-only, no tag: no-op, passes" "$raw"
fi

# --- 9. package-name alias (tga-style): passes on the pkg-name candidate -----
raw="$(run_full_mode_requires_tag trusty-git-analytics tga 2.19.0 0 1)"
if [ "$(status_of "$raw")" = "0" ]; then
  pass_case "9 alias tag (tga-v...): passes"
else
  fail_case "9 alias tag (tga-v...): passes" "$raw"
fi

echo
if [ "$FAILED" -eq 0 ]; then
  echo "preflight-check6-tag-gate-selftest: ${PASSED} case(s) passed."
  exit 0
fi
echo "preflight-check6-tag-gate-selftest: ${FAILED} of $((PASSED + FAILED)) case(s) FAILED." >&2
exit 1

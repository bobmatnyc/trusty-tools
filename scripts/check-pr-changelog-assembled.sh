#!/usr/bin/env bash
#
# check-pr-changelog-assembled.sh — merge-time half of the changelog-assembler
# gate (#6406). Companion to scripts/check-changelog-assembled.sh, which is the
# publish-time half wired into scripts/preflight-publish.sh CHECK 9.
#
# Why: the publish-time gate stops a bypass at the last possible moment, right
#   before `cargo publish`. This one catches the SAME shape earlier — while the
#   PR that wrote a hand-authored `## [<version>]` section is still open — so a
#   bypass never reaches main at all. #5919's six trusty-audit releases each
#   landed a PR that (by hand-editing Cargo.toml and never running
#   `scripts/assemble-changelog.sh`) either wrote no section at all, or could
#   have written one by hand while leaving the fragments it should have
#   consumed sitting untouched. This gate targets the second shape specifically
#   — the first is unreachable at PR time, because a version bump alone is not
#   evidence a release section exists yet (see the scope note below).
#
# What: for every crate whose `crates/<crate>/CHANGELOG.md` gains a NEW
#   `## [<version>]` heading in this PR (present in the diff's added lines,
#   absent from the merge base entirely), runs
#   `scripts/check-changelog-assembled.sh <crate> <version>` and fails if it
#   reports STRANDED-FRAGMENTS. A NEW heading is precisely what
#   `scripts/assemble-changelog.sh` writes when it runs for real — so a PR that
#   adds one is asserting "I cut a release", and this gate holds that PR to the
#   one guarantee a real assemble run always keeps: every fragment it folded in
#   is gone in the same commit.
#
# SCOPE, DELIBERATELY NARROW. This does NOT fire merely because a crate's
#   `Cargo.toml` version changed. `scripts/bump-version.sh --no-changelog`
#   bumps the manifest and deliberately leaves `changelog.d/` fragments pending
#   for the release cut to consume LATER — see that script's own header note,
#   "FRAGMENT CONSUMPTION IS THE RELEASE CUT'S JOB, AND ONLY ITS JOB (#5674)".
#   A gate that flagged every version bump without a same-PR CHANGELOG.md
#   section would break that supported workflow and teach people to bypass
#   this gate instead of the one it exists to catch. Requiring a NEW version
#   heading as the trigger keeps this gate silent on every in-flight bump and
#   loud only when a PR claims, in its own diff, to have assembled a release.
#
#   This narrowness means a hand-edited `Cargo.toml` version with NO CHANGELOG
#   section at all (the actual #5919 shape at the PR that introduced it) is NOT
#   caught here — nothing distinguishes that PR from a legitimate
#   `--no-changelog` bump at merge time. `scripts/check-changelog-assembled.sh`
#   run as CHECK 9 of `scripts/preflight-publish.sh` is what catches that shape,
#   at the point a version is actually tagged or published, when the ambiguity
#   is gone.
#
# Usage:
#   bash scripts/check-pr-changelog-assembled.sh                  # base: origin/main
#   bash scripts/check-pr-changelog-assembled.sh --base <ref>     # explicit base
#   PR_CHANGELOG_ASSEMBLED_BASE=<ref> bash scripts/check-pr-changelog-assembled.sh
#
# Exit: 0 when no crate has a new version section with stranded fragments (or
#   no crate's CHANGELOG.md changed at all); 1 when at least one does; 2 usage.
#
# Test: scripts/check-pr-changelog-assembled-selftest.sh builds synthetic
#   repos with a real assemble commit (clean) and a hand-authored section that
#   left a fragment behind (stranded), and asserts both outcomes.
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

BASE="${PR_CHANGELOG_ASSEMBLED_BASE:-origin/main}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --base needs a ref" >&2
        exit 2
      }
      BASE="$2"
      shift 2
      ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

if ! MERGE_BASE="$(git merge-base "${BASE}" HEAD 2>/dev/null)"; then
  echo "ERROR: cannot find a merge base between '${BASE}' and HEAD." >&2
  echo "       Fetch the base ref first (CI must check out with fetch-depth: 0):" >&2
  echo "         git fetch origin main" >&2
  exit 1
fi

CHANGED="$(git diff --name-only --no-renames "${MERGE_BASE}" HEAD -- 'crates/*/CHANGELOG.md')"

if [[ -z "${CHANGED//[$'\n' ]/}" ]]; then
  echo "check-pr-changelog-assembled: no crates/*/CHANGELOG.md changed — OK."
  exit 0
fi

FAIL=0
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/CHANGELOG\.md$#\1#')"
  [[ "$crate" == "$path" ]] && continue

  # New headings only: lines the diff ADDS that are absent from the WHOLE
  # merge-base file. A heading that already existed at the merge base (a
  # --merge run folding fragments into a stale section, #5298) is not "new"
  # even if the diff happens to touch nearby lines.
  new_headings="$(
    git diff --unified=0 --no-renames "${MERGE_BASE}" HEAD -- "$path" \
      | grep -E '^\+## \[' \
      | sed -E 's/^\+## \[([^]]+)\].*/\1/' \
      | LC_ALL=C sort -u
  )"
  [[ -z "$new_headings" ]] && continue

  base_headings="$(git show "${MERGE_BASE}:${path}" 2>/dev/null \
    | grep -oE '^## \[[^]]+\]' | sed -E 's/^## \[([^]]+)\].*/\1/' || true)"

  while IFS= read -r version; do
    [[ -z "$version" ]] && continue
    if printf '%s\n' "$base_headings" | grep -qxF "$version"; then
      continue # already existed at the merge base — not a new section
    fi

    echo "check-pr-changelog-assembled: ${crate} gained a new '## [${version}]'" \
      "section in this PR — verifying it was assembled, not hand-written."
    out=""
    rc=0
    out="$(bash "${REPO_ROOT}/scripts/check-changelog-assembled.sh" "${crate}" "${version}" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]] && printf '%s' "$out" | grep -q 'STRANDED-FRAGMENTS'; then
      echo "FAIL ${crate} ${version}: new CHANGELOG.md section, but changelog.d/ still" >&2
      echo "     holds fragment(s) it should have consumed:" >&2
      printf '%s\n' "$out" | sed 's/^/       /' >&2
      FAIL=1
    elif [[ "$rc" -ne 0 ]]; then
      # NO-SECTION cannot fire here (the section is exactly what triggered this
      # scan), so any other nonzero exit is unexpected tool trouble, not a
      # finding this gate is designed to make. Report it rather than swallow it.
      echo "FAIL ${crate} ${version}: check-changelog-assembled.sh reported an" >&2
      echo "     unexpected failure:" >&2
      printf '%s\n' "$out" | sed 's/^/       /' >&2
      FAIL=1
    else
      echo "OK   ${crate} ${version}: assembled cleanly, no stranded fragments."
    fi
  done <<<"$new_headings"
done <<<"$CHANGED"

if [[ "$FAIL" -ne 0 ]]; then
  cat >&2 <<'EOF'

A new '## [<version>]' section in CHANGELOG.md must come from the real
assembler, never a hand-written edit:

  scripts/assemble-changelog.sh <crate-dir> <version>

which deletes every fragment it folds in as part of the SAME operation. If
fragments still exist after adding the section, either run the assembler for
real or delete the section and let the actual release cut write it (#6406).
EOF
  exit 1
fi

echo "check-pr-changelog-assembled: OK — every new CHANGELOG.md section in this PR was assembled cleanly."
exit 0

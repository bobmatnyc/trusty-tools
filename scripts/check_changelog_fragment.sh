#!/usr/bin/env bash
#
# check_changelog_fragment.sh — per-PR changelog fragment gate (issue #4476).
#
# Why: "every PR that changes a crate's src/** records its user-visible change"
#   was a review-gate rule with ZERO mechanical enforcement — the same shape as
#   the 500-SLOC cap before scripts/check_line_cap.sh (#610). Advice without a
#   gate loses: entries got skipped, then backfilled at release time from commit
#   subjects that had already lost the nuance. #4476 also moved the entry from a
#   shared `## [Unreleased]` section (which made every concurrent PR conflict)
#   to a per-PR fragment file, so there is now a check worth automating: a
#   fragment is a NEW file, and its presence or absence is unambiguous.
#
# What: diffs the working branch against a base ref and, for every crate whose
#   `crates/<crate>/src/**` changed, requires evidence that the change was
#   recorded. Accepted evidence, per crate:
#     1. an ADDED-or-MODIFIED `crates/<crate>/changelog.d/<name>.md` fragment
#        that the release assembler ACCEPTS                        (the rule)
#     2. a `crates/<crate>/CHANGELOG.md` edit that adds a bullet   (TRANSITIONAL)
#
#   (1) is validated, not merely detected. Presence alone is worthless: a 0-byte
#   file, a body with no bullet, an unknown category, a fragment one directory
#   too deep, or several categories stacked into one file all look like evidence
#   and are all rejected, dropped, or MIS-RENDERED at release time. So the gate
#   runs the real assembler in preview mode
#   (`assemble-changelog.sh <crate> --stdout`) and fails if it would not accept
#   the crate's fragment set — the gate and the assembler cannot disagree,
#   because the gate asks the assembler.
#
#   Four narrower traps this closes, all of which passed a presence-only check:
#     - `git rm`-ing an existing fragment counted as evidence while DESTROYING
#       the record; deletions are excluded from evidence (`--diff-filter=d`).
#     - a fragment at `changelog.d/sub/12-x.md` counted, then vanished from the
#       release (the assembler now rejects nested fragments outright).
#     - `changelog.d/README.md` counted, and is skipped by the assembler
#       forever; it is the tracked directory placeholder, never evidence.
#     - a fragment stacking several categories into one file counted, and then
#       SILENTLY MIS-RENDERED: only line 1 is a category, so the rest became body
#       text under it. The 1.3.3 `4286-retire-trusty-mpm-override-files.md`
#       fragment put all four of its categories under `### Removed` and was
#       caught only by a human diffing the preview. The assembler now rejects it.
#
#   (2) exists only so PRs opened BEFORE #4476 landed — which wrote entries into
#   the shared `## [Unreleased]` section and are mid-review (#4463, #4464, #4465,
#   #4466, #4475) — do not go red and get churned. It SELF-EXPIRES: it applies
#   only when `scripts/assemble-changelog.sh` did not yet exist at the merge
#   base, which is true exactly for branches cut before this mechanism landed.
#   Every branch cut afterwards gets the strict rule with no dated deadline to
#   maintain and no cleanup PR to remember. It also requires the diff to add a
#   real bullet line, so a whitespace-only CHANGELOG.md touch is not evidence.
#
# Exemptions (a crate is NOT required to have evidence when):
#   - no `crates/<crate>/src/**` path changed at all — this is what makes
#     docs-only and CI-only PRs pass, per the documented rule;
#   - the only changed paths under that crate's src/** are test files, using
#     check_line_cap.sh's own classification (basename `tests.rs`, or ending
#     `_test.rs`/`_tests.rs`, or a `/tests/` or `/benches/` path segment). A
#     test-only edit has no user-visible change to describe.
#   - the crate no longer EXISTS at HEAD — i.e. the PR deleted it outright
#     (`crates/<crate>/Cargo.toml` is gone). Deleting a crate deletes every
#     `crates/<crate>/src/**` file, which reads as a source change and demanded
#     a fragment; but the fragment would have to live in the very directory
#     being removed, and `assemble-changelog.sh <crate>` cannot run for a crate
#     that is not there. The requirement was unsatisfiable, so a crate deletion
#     could never go green (found by #3732, which dissolved `cto-assistant`).
#     This exempts the DELETED crate only. Every crate that survives the PR is
#     still checked exactly as before, so the removal is still recorded — in
#     the surviving crate whose users are the ones who notice it (for #3732,
#     `crates/trusty-agents/changelog.d/`). A crate whose src/** merely shrank
#     is NOT exempt: the Cargo.toml probe is what distinguishes a dissolution
#     from a large deletion inside a crate that still exists.
#   - the path is under a `testdata/` directory. Golden fixtures live in
#     `src/**/testdata/` in this workspace, so regenerating a snapshot counted as
#     a source change and demanded a changelog fragment for a file that is, by
#     construction, test input. This PR's own regeneration of
#     crates/trusty-mpm/src/core/testdata/pm-prompt-*.md tripped exactly that.
#
#   There is deliberately NO "trivial change" escape hatch: adding a fragment is
#   one new file, and the rule it enforces has never had one.
#
# CI shape (issue #4468 caution): this job has NO `paths:` filter, so it always
#   runs and always reports on every PR — including an exempt docs-only PR,
#   where it reports SUCCESS. A `paths:`-filtered required check never reports on
#   the PRs it skips and leaves them permanently pending.
#
# Usage:
#   bash scripts/check_changelog_fragment.sh                  # base: origin/main
#   bash scripts/check_changelog_fragment.sh --base <ref>     # explicit base
#   CHANGELOG_GATE_BASE=<ref> bash scripts/check_changelog_fragment.sh
#
# Exit: 0 when every crate with source changes has evidence (or is exempt);
#   non-zero with a per-crate summary on stderr when one does not.
#
# Test: exercised in both directions in the PR that introduced it (#4476) — a
#   tree with fragments exits 0; the same tree with the fragments deleted fails
#   and names the crates. Pure path/diff logic; no unit-test harness.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only — `git`, `grep`, `sed`, `sort`.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

BASE="${CHANGELOG_GATE_BASE:-origin/main}"
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
    -h | --help)
      sed -n '2,80p' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

if ! MERGE_BASE="$(git merge-base "$BASE" HEAD 2>/dev/null)"; then
  echo "ERROR: cannot find a merge base between '$BASE' and HEAD." >&2
  echo "       Fetch the base ref first (CI must check out with fetch-depth: 0):" >&2
  echo "         git fetch origin main" >&2
  exit 1
fi

# Two views of the same diff. `CHANGED` includes deletions, because deleting a
# source file IS a change worth recording. `PRESENT` excludes them, because a
# deleted fragment or changelog is the opposite of evidence — the presence-only
# check counted `git rm crates/X/changelog.d/*.md` as a record while destroying
# the one that existed.
CHANGED="$(git diff --name-only --no-renames "$MERGE_BASE" HEAD)"
PRESENT="$(git diff --name-only --no-renames --diff-filter=d "$MERGE_BASE" HEAD)"

# #4618: the scan floor. "No changes at all against the base" used to exit 0 as
# "nothing to check" — indistinguishable from a gate that examined the whole PR
# and found it recorded. A real PR always changes at least one path, so an empty
# diff means the base ref is wrong or the checkout is shallow, not that the PR is
# clean. Report the number examined so a future regression is visible in the log.
CHANGED_COUNT="$(printf '%s\n' "$CHANGED" | grep -c '[^[:space:]]' || true)"
if [[ "${CHANGED_COUNT:-0}" -lt 1 ]]; then
  echo "FAIL: SCAN FLOOR — the diff ${MERGE_BASE}..HEAD lists 0 changed path(s)." >&2
  echo "      Nothing was examined, so this gate could not have failed. Check that" >&2
  echo "      '${BASE}' is the right base and that CI checked out with fetch-depth: 0." >&2
  echo "      A gate that scans nothing is not a passing gate (issue #4618)." >&2
  exit 1
fi

# Why: `git cat-file -e <rev>:<path>` exits 128 BOTH when the path is legitimately
#   absent from that tree AND when git itself failed, so the previous
#   `git cat-file -e ... || continue` read every git error as "the PR deleted this
#   crate" — the exemption — and the gate printed OK on a PR with a real
#   unrecorded source change (#4618 item 3).
# What: returns 0 when <path> exists at <rev>, 1 when it is definitively absent.
#   `git ls-tree` separates the two cases that `cat-file -e` conflates: it exits 0
#   with EMPTY output for an absent path, and non-zero only on a genuine git
#   failure — which is escalated to a hard gate failure here, never swallowed.
#   Stderr is captured SEPARATELY, never folded into `out` with `2>&1`: `out`
#   is the existence signal, so a git warning printed on an otherwise
#   successful run would make an absent path read as present — turning the
#   #3732 crate-deletion exemption into a false red.
path_exists_at_rev() {
  local rev="$1" path="$2" out err rc=0
  err="$(mktemp "${TMPDIR:-/tmp}/changelog.lstree.XXXXXX")"
  out="$(git ls-tree -r --name-only "$rev" -- "$path" 2>"$err")" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: TOOL ERROR — 'git ls-tree ${rev} -- ${path}' failed:" >&2
    sed 's/^/       /' "$err" >&2
    echo "      Whether the path exists is unknown, so no exemption may be granted." >&2
    echo "      This is NOT a pass (issue #4618)." >&2
    rm -f "$err"
    exit 1
  fi
  rm -f "$err"
  [[ -n "$out" ]]
}

# The TRANSITIONAL CHANGELOG.md branch applies only to branches cut before the
# fragment mechanism existed. Probing for the assembler at the merge base is a
# self-expiring test: absent => this branch predates #4476 => be lenient;
# present => the branch had fragments available => be strict. No date, no
# follow-up cleanup PR.
if path_exists_at_rev "$MERGE_BASE" "scripts/assemble-changelog.sh"; then
  TRANSITIONAL=0
else
  TRANSITIONAL=1
fi

# Why: a test-only edit under src/** has no user-visible change to describe.
# Classification is copied from check_line_cap.sh so the two gates agree on what
# "a test file" means.
# What: returns 0 when $1 is a test/benchmark path.
is_test_path() {
  local path="$1" base
  base="$(basename "$path")"
  [[ "$base" == "tests.rs" ]] && return 0
  case "$base" in
    *_test.rs | *_tests.rs) return 0 ;;
  esac
  case "$path" in
    */tests/* | */benches/* | */testdata/*) return 0 ;;
  esac
  return 1
}

# Why: `case` globs let `*` span `/`, so the pattern `crates/*/changelog.d/*.md`
# also matches `crates/a/b/changelog.d/x.md` and a naive sed extraction then
# emits a whole path where a crate name belongs. Extract first, then verify the
# reconstructed path — which only holds when there is exactly one directory level
# under crates/ and the fragment sits directly in changelog.d/.
# What: prints the crate name for a depth-1 fragment path, or fails.
fragment_crate() {
  local path="$1" crate
  crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/changelog\.d/[^/]+\.md$#\1#')"
  [[ "$crate" == "$path" ]] && return 1
  [[ "$crate" == */* ]] && return 1
  echo "$crate"
}

# Crates with a NON-test source change (these need evidence).
needs=""
# Crates with fragment evidence.
has_fragment=""
# Crates with transitional CHANGELOG.md evidence.
has_changelog=""

# Source changes come from the deletion-inclusive view.
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  case "$path" in
    crates/*/src/*)
      crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/src/.*#\1#')"
      [[ "$crate" == */* ]] && continue
      is_test_path "$path" && continue
      # The crate was dissolved by this PR — there is no changelog.d/ left to
      # put a fragment in, and the assembler cannot run for it. See the
      # "crate no longer EXISTS at HEAD" exemption above. A git FAILURE here is
      # not the exemption; path_exists_at_rev hard-fails on one (#4618).
      path_exists_at_rev HEAD "crates/${crate}/Cargo.toml" || continue
      needs="${needs}${crate}"$'\n'
      ;;
  esac
done <<<"$CHANGED"

# Evidence comes from the view that excludes deletions.
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  case "$path" in
    crates/*/changelog.d/*)
      # README.md is the tracked placeholder that keeps changelog.d/ alive
      # between releases. The assembler skips it permanently, so counting it as
      # evidence would record a bullet that can never be released.
      [[ "$(basename "$path")" == "README.md" ]] && continue
      if crate="$(fragment_crate "$path")"; then
        has_fragment="${has_fragment}${crate}"$'\n'
      fi
      ;;
    crates/*/CHANGELOG.md)
      crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/CHANGELOG.md$#\1#')"
      [[ "$crate" == */* ]] && continue
      # A whitespace-only touch is not a changelog entry.
      if git diff --unified=0 --no-renames "$MERGE_BASE" HEAD -- "$path" |
        grep -qE '^\+[[:space:]]*-[[:space:]]'; then
        has_changelog="${has_changelog}${crate}"$'\n'
      fi
      ;;
  esac
done <<<"$PRESENT"

needs="$(printf '%s' "$needs" | grep -v '^$' | LC_ALL=C sort -u || true)"

if [[ -z "$needs" ]]; then
  echo "changelog-fragment gate: scanned ${CHANGED_COUNT} changed path(s); no crate source changed (docs-only / CI-only / test-only) — OK."
  exit 0
fi

fail=0
while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue
  if printf '%s' "$has_fragment" | grep -qx "$crate"; then
    # Ask the assembler, rather than trusting the filename. This is what turns a
    # presence check into a validation: an empty, bodyless, mis-categorised or
    # nested fragment fails HERE, in the PR that wrote it, instead of at release.
    if assemble_err="$(bash "${REPO_ROOT}/scripts/assemble-changelog.sh" "$crate" --stdout 2>&1 >/dev/null)"; then
      echo "OK   ${crate}: changelog.d fragment present and valid"
    else
      echo "FAIL ${crate}: changelog.d fragment present but the release assembler rejects it" >&2
      printf '%s\n' "$assemble_err" | sed 's/^/       /' >&2
      fail=1
    fi
  elif [[ "$TRANSITIONAL" -eq 1 ]] && printf '%s' "$has_changelog" | grep -qx "$crate"; then
    echo "OK   ${crate}: CHANGELOG.md entry (TRANSITIONAL — branch predates #4476)"
  else
    echo "FAIL ${crate}: crates/${crate}/src/** changed with no changelog record" >&2
    fail=1
  fi
done <<<"$needs"

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<'EOF'

Add one fragment per crate you changed:

  crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md

  Breaking | Added | Fixed | Performance | Changed | Removed | Security |
  Documentation                                                    <- line 1

  - one bullet per user-visible change, in the crate CHANGELOG's existing style

The number keeps the filename collision-free across concurrent PRs; that is the
whole point (issue #4476). The file must sit DIRECTLY in changelog.d/ — a nested
one is rejected — and carries exactly ONE category: everything after line 1 is
copied through verbatim, so a second category stacked into the same file renders
as body text under the first. Split it, reusing the number with a different slug.
Never edit the crate CHANGELOG.md by hand: release time assembles the fragments
(scripts/assemble-changelog.sh).

Preview what will be released:  bash scripts/assemble-changelog.sh <crate> --stdout

Exempt: docs-only, CI-only, test-only, and testdata/ changes, plus a crate this
PR deleted outright (record the removal in a surviving crate's fragment).
EOF
  exit 1
fi

crate_count="$(printf '%s\n' "$needs" | grep -c '[^[:space:]]' || true)"
echo "changelog-fragment gate: scanned ${CHANGED_COUNT} changed path(s); all ${crate_count} crate(s) with source changes are recorded."

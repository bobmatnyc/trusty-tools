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
#   file, a body with no bullet, an unknown category, or a fragment one directory
#   too deep all look like evidence and are all rejected or dropped at release
#   time. So the gate runs the real assembler in preview mode
#   (`assemble-changelog.sh <crate> --stdout`) and fails if it would not accept
#   the crate's fragment set — the gate and the assembler cannot disagree,
#   because the gate asks the assembler.
#
#   Three narrower traps this closes, all of which passed a presence-only check:
#     - `git rm`-ing an existing fragment counted as evidence while DESTROYING
#       the record; deletions are excluded from evidence (`--diff-filter=d`).
#     - a fragment at `changelog.d/sub/12-x.md` counted, then vanished from the
#       release (the assembler now rejects nested fragments outright).
#     - `changelog.d/README.md` counted, and is skipped by the assembler
#       forever; it is the tracked directory placeholder, never evidence.
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

if [[ -z "$CHANGED" ]]; then
  echo "changelog-fragment gate: no changes against ${BASE} — nothing to check."
  exit 0
fi

# The TRANSITIONAL CHANGELOG.md branch applies only to branches cut before the
# fragment mechanism existed. Probing for the assembler at the merge base is a
# self-expiring test: absent => this branch predates #4476 => be lenient;
# present => the branch had fragments available => be strict. No date, no
# follow-up cleanup PR.
if git cat-file -e "${MERGE_BASE}:scripts/assemble-changelog.sh" 2>/dev/null; then
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
  echo "changelog-fragment gate: no crate source changed (docs-only / CI-only / test-only) — OK."
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
one is rejected. Never edit the crate CHANGELOG.md by hand: release time
assembles the fragments (scripts/assemble-changelog.sh).

Preview what will be released:  bash scripts/assemble-changelog.sh <crate> --stdout

Exempt: docs-only, CI-only, test-only, and testdata/ changes.
EOF
  exit 1
fi

echo "changelog-fragment gate: all crates with source changes are recorded."

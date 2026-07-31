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
#     1. a `crates/<crate>/changelog.d/*.md` fragment in the diff  (the rule)
#     2. a `crates/<crate>/CHANGELOG.md` edit in the diff          (TRANSITIONAL)
#
#   (2) exists only so PRs opened BEFORE #4476 landed — which wrote entries into
#   the shared `## [Unreleased]` section and are mid-review — do not go red and
#   get churned. New work must use (1); once the pre-#4476 PRs have landed this
#   branch can be dropped, and `scripts/assemble-changelog.sh` already fails
#   loudly at release time if a leftover `## [Unreleased]` section survives.
#
# Exemptions (a crate is NOT required to have evidence when):
#   - no `crates/<crate>/src/**` path changed at all — this is what makes
#     docs-only and CI-only PRs pass, per the documented rule;
#   - the only changed paths under that crate's src/** are test files, using
#     check_line_cap.sh's own classification (basename `tests.rs`, or ending
#     `_test.rs`/`_tests.rs`, or a `/tests/` or `/benches/` path segment). A
#     test-only edit has no user-visible change to describe.
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
      sed -n '2,50p' "$0" >&2
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

CHANGED="$(git diff --name-only "$MERGE_BASE" HEAD)"

if [[ -z "$CHANGED" ]]; then
  echo "changelog-fragment gate: no changes against ${BASE} — nothing to check."
  exit 0
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
    */tests/* | */benches/*) return 0 ;;
  esac
  return 1
}

# Crates with a NON-test source change (these need evidence).
needs=""
# Crates with fragment evidence.
has_fragment=""
# Crates with transitional CHANGELOG.md evidence.
has_changelog=""

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  case "$path" in
    crates/*/src/*)
      crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/src/.*#\1#')"
      is_test_path "$path" && continue
      needs="${needs}${crate}"$'\n'
      ;;
    crates/*/changelog.d/*.md)
      crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/changelog.d/.*#\1#')"
      has_fragment="${has_fragment}${crate}"$'\n'
      ;;
    crates/*/CHANGELOG.md)
      crate="$(printf '%s' "$path" | sed -E 's#^crates/([^/]+)/CHANGELOG.md$#\1#')"
      has_changelog="${has_changelog}${crate}"$'\n'
      ;;
  esac
done <<<"$CHANGED"

needs="$(printf '%s' "$needs" | grep -v '^$' | LC_ALL=C sort -u || true)"

if [[ -z "$needs" ]]; then
  echo "changelog-fragment gate: no crate source changed (docs-only / CI-only / test-only) — OK."
  exit 0
fi

fail=0
while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue
  if printf '%s' "$has_fragment" | grep -qx "$crate"; then
    echo "OK   ${crate}: changelog.d fragment present"
  elif printf '%s' "$has_changelog" | grep -qx "$crate"; then
    echo "OK   ${crate}: CHANGELOG.md edit (TRANSITIONAL — new PRs must use changelog.d/)"
  else
    echo "FAIL ${crate}: crates/${crate}/src/** changed with no changelog record" >&2
    fail=1
  fi
done <<<"$needs"

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<'EOF'

Add one fragment per crate you changed:

  crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md

  Breaking | Added | Fixed | Performance | Changed | Documentation   <- line 1

  - one bullet per user-visible change, in the crate CHANGELOG's existing style

The number keeps the filename collision-free across concurrent PRs; that is the
whole point (issue #4476). Never edit the crate CHANGELOG.md by hand — release
time assembles the fragments (scripts/assemble-changelog.sh).

Exempt: docs-only, CI-only, and test-only changes.
EOF
  exit 1
fi

echo "changelog-fragment gate: all crates with source changes are recorded."

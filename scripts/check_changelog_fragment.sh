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
#   source changed, requires evidence that the change was recorded. A path is
#   crate source when it lies under `crates/` and carries a `/src/` directory
#   segment — the selector is unchanged since #4476; what changed in #4576 is
#   only that a NESTED one (`crates/trusty-audit/ui/src-tauri/src/main.rs`,
#   `crates/trusty-agents/ui/src/App.svelte`) is now attributed to the crate that
#   ships it instead of being silently dropped. See ATTRIBUTION BY STRUCTURE.
#   Test files under src/** are EXEMPT, and what counts as one is
#   `scripts/lib/source_class.sh` — shared with
#   `scripts/check-pr-version-bump.sh`, which reads the same definition and
#   deliberately does not apply the exemption (#5765; the ruling is in that
#   file). Accepted evidence, per crate:
#     1. an ADDED-or-MODIFIED `crates/<crate>/changelog.d/<name>.md` fragment
#        that the release assembler ACCEPTS                        (the rule)
#     2. a `crates/<crate>/CHANGELOG.md` edit that adds a bullet   (TRANSITIONAL)
#     3. a `crates/<crate>/CHANGELOG.md` bullet the assembler folded into the
#        crate's already-cut `## [<version>]` section    (RELEASE WINDOW, #6695)
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
#   (3) is the RELEASE WINDOW, and it exists because this gate and
#   `scripts/check-changelog-assembled.sh` demanded opposite states of the same
#   PR (#6695). Once a release cut writes `## [<version>]`, a source fix landing
#   before the tag must run `assemble-changelog.sh <crate> <version> --merge`,
#   which folds the pending fragments into that section and DELETES them in the
#   same operation — a survivor is exactly what the assembled gate fails on. The
#   fragment is then gone from the diff, so this gate read the fold as an
#   omission and failed the PR. Live instance: branch
#   fix/prepublish-doc-links-20260902 (ffe03c23c) passed the assembled gate for
#   trusty-common and trusty-mpm and was failed here for both.
#
#   Four facts must all hold, and each rules out a way of faking the shape:
#     - the crate's CHANGELOG.md gained a bullet, and that exact line sits
#       INSIDE the `## [<version>]` section at HEAD — a bullet added anywhere
#       else is not a record of the release being cut;
#     - <version> is what `crates/<crate>/Cargo.toml` ships RIGHT NOW, so the
#       section is the one about to be published;
#     - no `<package>-v<version>` tag exists yet. After the tag that section is
#       released history, and writing into it back-dates a change into a
#       version that never carried it. A checkout with NO tags at all cannot
#       establish this, so it is refused rather than assumed (fail closed);
#     - `scripts/check-changelog-assembled.sh <crate> <version>` exits 0 — no
#       fragment survived and the section is really there. Asking the other
#       gate is what makes the two agree BY CONSTRUCTION, the same reason this
#       script asks the assembler instead of validating fragments itself.
#
#   NOT accepted as the signature: a deleted `changelog.d/*.md` in the diff,
#   which #6695 proposed. `--merge` consumes the fragment the same commit added,
#   so the net diff carries no changelog.d path at all — ffe03c23c changed five
#   files and none of them was a fragment. Requiring the deletion would have
#   left the reported case red.
#
# ATTRIBUTION BY STRUCTURE (#4576). The crate that owns a changed path is found
#   by walking UP to the nearest ancestor directory holding a Cargo.toml, then
#   rolling that owner up to the `crates/<crate>/` directory that owns
#   changelog.d/. No path depth is hardcoded, so a crate nested three levels
#   deep needs no second fix. A path this gate SELECTS as source but cannot
#   attribute is a hard failure naming the path (UNATTRIBUTED SOURCE), never a
#   silent drop — see the fail-open it replaces in `resolve_source_crate`.
#
# Exemptions (a crate is NOT required to have evidence when):
#   - no crate source path changed at all — this is what makes docs-only and
#     CI-only PRs pass, per the documented rule;
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
# ATTRIBUTION (#5018). The base must name the base BRANCH as it exists NOW
#   (`origin/main`), never a frozen commit SHA. The base ref decides which
#   commits count as "this PR's", so a wrong base blames the wrong author.
#
#   `actions/checkout` resolves a `pull_request` event to `refs/pull/N/merge`, a
#   GitHub-built merge commit whose FIRST parent is the base branch's CURRENT
#   tip, refreshed continuously. `github.event.pull_request.base.sha` — what
#   this gate's workflow originally passed — is a snapshot taken when the event
#   fired, and goes stale the moment anything else merges. The stale SHA is an
#   ANCESTOR of the merge ref, so `git merge-base` hands it straight back
#   instead of finding a fork point, and the diff sweeps in every commit main
#   took in between.
#
#   That is what turns an unrelated crate red. A release assembles a crate's
#   fragments into CHANGELOG.md and DELETES `changelog.d/*.md`; deletions are
#   excluded from evidence (see PRESENT below). So any crate that both changed
#   and shipped a release inside that window reads as "source changed, no
#   fragment" — release-time fragment CONSUMPTION misread as this PR's
#   OMISSION. Live instance: PR #5018 changed one crate (trusty-installer) and
#   was failed for trusty-analyze, trusty-git-analytics and trusty-review, none
#   of which it touches (run 31431273762). Against the live branch tip the
#   merge base IS the merge ref's first parent, so the diff is exactly what the
#   PR contributes — 9 paths and 1 crate, not 1279 and 16.
#
#   Same failure and same fix as #4688 (check-pr-version-bump.sh) and #4960
#   (detect-docs-only.sh); this gate was the last one still pinned to base.sha.
#   `main()` refuses the broken shape rather than guessing — see STALE BASE.
#
# Usage:
#   bash scripts/check_changelog_fragment.sh                  # base: origin/main
#   bash scripts/check_changelog_fragment.sh --base <ref>     # explicit base
#   CHANGELOG_GATE_BASE=<ref> bash scripts/check_changelog_fragment.sh
#
# Exit: 0 when every crate with source changes has evidence (or is exempt);
#   non-zero with a per-crate summary on stderr when one does not.
#
# Test: scripts/check_changelog_base_selftest.sh replays the #5018 shape on a
#   synthetic repo — a branch touching crate A only, with crate B's src/**
#   changed and B's fragments consumed by a release on main since the fork
#   point — and asserts the stale base is refused, the live branch base passes
#   naming only A, and a genuinely missing fragment still fails.
#   scripts/check_changelog_release_window_selftest.sh replays the #6695 shape:
#   a crate whose pending fragments a --merge folded into its cut section
#   passes this gate AND check-changelog-assembled.sh, while the same bullet
#   under an already-tagged section still fails.
#   scripts/check_changelog_attribution_selftest.sh replays the #4576 shape:
#   a nested crate source path must be attributed, not dropped, and an
#   unattributable one must fail the gate. Fragment validation is covered by
#   scripts/assemble_changelog_selftest.sh, the scan floor by
#   scripts/check_scan_floor_selftest.sh, and the shared test-file
#   classification by scripts/check_source_class_selftest.sh.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only — `git`, `grep`, `sed`, `sort`.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
# The shared source/test path classification (#5765). Sourced from this script's
# OWN directory, not from $REPO_ROOT, so a copy of the gate run out of tree
# picks up the library beside it rather than a different checkout's.
# shellcheck source=lib/source_class.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/source_class.sh"
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
      # Through the exemption list — keep this range in step with the header.
      sed -n '2,130p' "$0" >&2
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

# #5018 STALE BASE. A ref resolves to a symbolic full name; a bare SHA resolves
# to empty output. That distinction is the whole test: `origin/main` tracks the
# base branch and self-corrects as main moves, a pinned SHA cannot.
if [[ -n "$(git rev-parse --symbolic-full-name "$BASE" 2>/dev/null)" ]]; then
  BASE_IS_REF=1
else
  BASE_IS_REF=0
fi

# Refuse a bare-SHA base that lags the merge ref's own base parent. Comparing
# (BASE, HEAD) cannot distinguish a stale base from a correct one — both are
# ancestors of HEAD — so the gate does not try to guess which commits are the
# PR's. It rejects the one configuration where the mis-attribution is provable
# and names the fix, instead of silently blaming crates the PR never touched.
#
# All four conditions are required, and each one rules out a legitimate caller:
#   - bare SHA: `--base origin/main` is a ref and is always allowed;
#   - HEAD has a second parent: only the merge-ref checkout has one, so a run
#     against a plain branch head is untouched (there the same stale SHA still
#     resolves to the true fork point, which is correct);
#   - BASE is an ancestor of HEAD^1: a base that is not behind the base parent
#     is not stale;
#   - BASE differs from HEAD^1: passing the merge ref's own base parent by SHA
#     is exact, not stale, and stays allowed.
if [[ "$BASE_IS_REF" -eq 0 ]] &&
  git rev-parse --verify --quiet 'HEAD^2' >/dev/null &&
  git merge-base --is-ancestor "$BASE" 'HEAD^1' &&
  [[ "$(git rev-parse "$BASE")" != "$(git rev-parse 'HEAD^1')" ]]; then
  BASE_PARENT="$(git rev-parse 'HEAD^1')"
  BEHIND="$(git rev-list --count "${BASE}..${BASE_PARENT}")"
  echo "FAIL: STALE BASE — '${BASE}' is a bare commit SHA ${BEHIND} commit(s) behind" >&2
  echo "      HEAD's base parent ${BASE_PARENT:0:10}, and HEAD is a merge ref." >&2
  echo "      The diff against it would span every commit merged in between and" >&2
  echo "      report other authors' crates as this PR's missing fragments — a" >&2
  echo "      release in that window CONSUMES changelog.d/*.md, which reads as an" >&2
  echo "      omission. That is the #5018 false failure; refusing to repeat it." >&2
  echo "      Pass the base BRANCH instead, so the merge base tracks it:" >&2
  echo "        CHANGELOG_GATE_BASE=origin/main bash scripts/check_changelog_fragment.sh" >&2
  echo "      In a workflow, use origin/\${{ github.event.pull_request.base.ref }}" >&2
  echo "      after re-fetching that branch (the #4688 pattern)." >&2
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

# #5765: `is_test_path` used to be a private copy here, and
# scripts/check-pr-version-bump.sh carried a different match with no test arm at
# all — so the two gates disagreed about the same path and nothing recorded
# which was meant to. Both now read the definition from
# scripts/lib/source_class.sh, whose THE RULING section states why this gate
# EXEMPTS a test file (a test-only edit has no user-visible change to describe)
# while the version-bump gate counts one as source (a crates.io tarball ships
# it). Sourced above with the rest of this gate's setup.

# #4576: attribute a source path to its crate by STRUCTURE, not by path depth.
#
# Why: the source arm used to extract the crate with
# `sed -E 's#^crates/([^/]+)/src/.*#\1#'`, which spans exactly one directory
# level. The `case` glob that selects the path does NOT — bash `case` lets `*`
# span `/` — so every NESTED crate source path was selected and then silently
# discarded by the `[[ "$crate" == */* ]] && continue` guard that followed. It
# raised nothing: the gate concluded "no crate source changed" and reported
# success. PR #5796 added crates/trusty-audit/ui/src-tauri/src/** and the gate
# said exactly that while passing. A green meaning "examined nothing" is
# indistinguishable from one meaning "checked and clean" — the #5620 shape.
# Widening the sed by one more hardcoded level would only move the wrong depth.
#
# What: `nearest_manifest_dir` walks UP from a path's directory to the nearest
# ancestor holding a Cargo.toml at <rev>, bounded to directories under crates/.
# `resolve_source_crate` then rolls that owner up to the top-level `crates/<name>`
# directory, which is the unit changelog.d/ and `assemble-changelog.sh <crate-dir>`
# are keyed to — a nested member such as crates/trusty-audit/ui/src-tauri owns no
# changelog.d/ of its own, so its changes are recorded by the crate that ships it.
# HEAD is tried first, then the merge base, so a path whose crate this PR DELETED
# still attributes and reaches the #3732 dissolution exemption below.
#
# Neither uses command substitution: `path_exists_at_rev` hard-exits the gate on
# a genuine git failure, and a `$(...)` would trap that exit in a subshell and
# hand back a garbage crate name — reintroducing a fail-open inside the fix for
# one.
#
# Test: scripts/check_changelog_attribution_selftest.sh.
OWNER_DIR=""
nearest_manifest_dir() {
  local rev="$1" dir="$2"
  OWNER_DIR=""
  while [[ "$dir" == crates/?* ]]; do
    if path_exists_at_rev "$rev" "${dir}/Cargo.toml"; then
      OWNER_DIR="$dir"
      return 0
    fi
    dir="${dir%/*}"
  done
  return 1
}

# `git diff --name-only` emits sorted paths, so files in one directory arrive
# adjacent; caching the last directory's answer collapses the repeated walk on a
# large diff without a bash-3.2-hostile associative array.
ATTRIB_DIR=""
ATTRIB_CRATE=""

# Sets SOURCE_CRATE to the owning top-level crate name; returns 1 when the path
# is under crates/ and looks like source but belongs to no crate at either rev.
SOURCE_CRATE=""
resolve_source_crate() {
  local path="$1" dir
  dir="${path%/*}"
  if [[ "$dir" == "$ATTRIB_DIR" ]]; then
    SOURCE_CRATE="$ATTRIB_CRATE"
    [[ -n "$SOURCE_CRATE" ]]
    return
  fi
  SOURCE_CRATE=""
  if nearest_manifest_dir HEAD "$dir" || nearest_manifest_dir "$MERGE_BASE" "$dir"; then
    SOURCE_CRATE="${OWNER_DIR#crates/}"
    SOURCE_CRATE="${SOURCE_CRATE%%/*}"
  fi
  ATTRIB_DIR="$dir"
  ATTRIB_CRATE="$SOURCE_CRATE"
  [[ -n "$SOURCE_CRATE" ]]
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

# #6695 RELEASE WINDOW. Accepts the state an
# `assemble-changelog.sh <crate> <version> --merge` run leaves behind, and only
# that state. The header's "(3) is the RELEASE WINDOW" section states why each
# of the four facts below is required; this is the mechanism.
#
# Sets ASSEMBLED_VERSION on acceptance and ASSEMBLED_WHY on refusal, so the
# failure line can name the missing fact instead of leaving an author who DID
# run the assembler with nothing to act on.
#
# Test: scripts/check_changelog_release_window_selftest.sh.
ASSEMBLED_WHY=""
ASSEMBLED_VERSION=""
assembled_into_cut_section() {
  local crate="$1"
  local manifest="crates/${crate}/Cargo.toml"
  local changelog="crates/${crate}/CHANGELOG.md"
  local version pkg section added line assembled_err
  ASSEMBLED_WHY=""
  ASSEMBLED_VERSION=""

  if [[ ! -f "$manifest" || ! -f "$changelog" ]]; then
    ASSEMBLED_WHY="${manifest} or ${changelog} is not in the working tree"
    return 1
  fi

  version="$(grep -m1 -E '^version[[:space:]]*=[[:space:]]*"[0-9]+\.[0-9]+\.[0-9]+"' "$manifest" |
    sed -E 's/^version[[:space:]]*=[[:space:]]*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/' || true)"
  pkg="$(grep -m1 -E '^name[[:space:]]*=[[:space:]]*"' "$manifest" |
    sed -E 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/' || true)"
  if [[ -z "$version" || -z "$pkg" ]]; then
    ASSEMBLED_WHY="could not read name/version from ${manifest}"
    return 1
  fi

  # The release window is bounded by the tag. A checkout carrying no tags at
  # all cannot answer "has this version shipped", and a fact this gate cannot
  # establish is never an exemption (the #4618 rule).
  if [[ -z "$(git for-each-ref --count=1 --format='%(refname)' refs/tags 2>/dev/null || true)" ]]; then
    ASSEMBLED_WHY="this checkout has no tags, so whether ${pkg} ${version} already shipped cannot be established (fetch them: git fetch --tags origin)"
    return 1
  fi
  if [[ -n "$(git tag --list "${pkg}-v${version}" 2>/dev/null || true)" ]]; then
    ASSEMBLED_WHY="${pkg} ${version} is already tagged — '## [${version}]' is released history, not a pending cut"
    return 1
  fi

  # Literal prefix match, not a regex: the closing bracket terminates the
  # version, so '## [0.47.1]' cannot also match '## [0.47.10] — …', and no
  # escaped dots have to survive an awk -v assignment.
  section="$(awk -v hdr="## [${version}]" '
      index($0, hdr) == 1 { inside = 1; next }
      inside && index($0, "## [") == 1 { exit }
      inside { print }
    ' "$changelog" || true)"
  if [[ -z "$section" ]]; then
    ASSEMBLED_WHY="${changelog} has no '## [${version}]' section to fold into"
    return 1
  fi

  added="$(git diff --unified=0 --no-renames "$MERGE_BASE" HEAD -- "$changelog" |
    grep -E '^\+[[:space:]]*-[[:space:]]' | sed 's/^+//' || true)"
  if [[ -z "$added" ]]; then
    ASSEMBLED_WHY="this branch adds no bullet to ${changelog}"
    return 1
  fi

  ASSEMBLED_WHY="this branch's ${changelog} bullets land outside the '## [${version}]' section"
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    # `-e` is required: a bullet begins with `-`, which grep reads as an
    # option otherwise and then reports a usage error INSTEAD of a mismatch.
    if printf '%s\n' "$section" | grep -qxF -e "$line"; then
      ASSEMBLED_WHY=""
      break
    fi
  done <<<"$added"
  [[ -n "$ASSEMBLED_WHY" ]] && return 1

  # Ask the other gate rather than re-deriving what it means by "assembled".
  # This is what makes the two verdicts agree by construction (#6695), the same
  # reason the fragment arm below asks the assembler.
  if ! assembled_err="$(bash "${REPO_ROOT}/scripts/check-changelog-assembled.sh" \
    "$crate" "$version" 2>&1 >/dev/null)"; then
    ASSEMBLED_WHY="check-changelog-assembled.sh still rejects ${crate} ${version}: $(printf '%s' "$assembled_err" | grep -m1 '^FAIL' || true)"
    return 1
  fi

  ASSEMBLED_VERSION="$version"
  return 0
}

# Crates with a NON-test source change (these need evidence).
needs=""
# Crates with fragment evidence.
has_fragment=""
# Crates with transitional CHANGELOG.md evidence.
has_changelog=""
# #4576: source paths this gate could not attribute to any crate. An
# unattributable path is a gap in the gate's own knowledge, never an exemption.
unattributed=""
# Source paths actually attributed, reported so a future silent drop is visible
# in the log the way the scan floor's path count already is (#4618).
attributed_count=0
# "<crate>\t<path>" for paths attributed from OUTSIDE crates/<crate>/src/. The
# failure line names `crates/<crate>/src/**`, which is where a reader would look
# and not find a nested change; one sample path saves that trip.
nested_samples=""

# Source changes come from the deletion-inclusive view.
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  case "$path" in
    crates/*/src/*)
      is_test_path "$path" && continue
      # #4576: attribute by structure. A path this arm SELECTED but cannot
      # attribute is reported, never dropped — the silent drop is what made the
      # gate report success over a whole nested source tree.
      if ! resolve_source_crate "$path"; then
        unattributed="${unattributed}${path}"$'\n'
        continue
      fi
      crate="$SOURCE_CRATE"
      attributed_count=$((attributed_count + 1))
      case "$path" in
        "crates/${crate}/src/"*) ;;
        *) nested_samples="${nested_samples}${crate}"$'\t'"${path}"$'\n' ;;
      esac
      # The crate was dissolved by this PR — there is no changelog.d/ left to
      # put a fragment in, and the assembler cannot run for it. See the
      # "crate no longer EXISTS at HEAD" exemption above. A git FAILURE here is
      # not the exemption; path_exists_at_rev hard-fails on one (#4618).
      if path_exists_at_rev HEAD "crates/${crate}/Cargo.toml"; then
        needs="${needs}${crate}"$'\n'
      elif ! path_exists_at_rev "$MERGE_BASE" "crates/${crate}/Cargo.toml"; then
        # #4576: not a dissolution — the top-level crate directory holds no
        # manifest at EITHER rev, so there is no changelog.d/ this path could
        # ever be recorded in and nothing said so. Report it.
        unattributed="${unattributed}${path}"$'\n'
      fi
      ;;
  esac
done <<<"$CHANGED"

# #4576: fail CLOSED. Pre-fix these paths vanished from the changed set with no
# message, so the gate printed "no crate source changed … — OK" over them.
if [[ -n "$unattributed" ]]; then
  echo "FAIL: UNATTRIBUTED SOURCE — path(s) under crates/ that look like crate" >&2
  echo "      source belong to no crate this gate can name, at HEAD or at the" >&2
  echo "      merge base ${MERGE_BASE:0:10}:" >&2
  printf '%s' "$unattributed" | grep -v '^$' | sed 's/^/         /' >&2
  echo "      Attribution walks up from each path to the nearest ancestor" >&2
  echo "      directory holding a Cargo.toml, then to the crates/<crate>/ that" >&2
  echo "      owns changelog.d/. None was found, so the gate cannot say which" >&2
  echo "      crate must record the change — and will not call that an" >&2
  echo "      exemption. Add the crate's Cargo.toml, or move the file out of" >&2
  echo "      crates/ if it is not crate source (issue #4576)." >&2
  exit 1
fi

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
  echo "changelog-fragment gate: scanned ${CHANGED_COUNT} changed path(s); attributed ${attributed_count} crate-source path(s); no crate source changed (docs-only / CI-only / test-only) — OK."
  exit 0
fi

fail=0
while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue
  ASSEMBLED_WHY=""
  has_bullet=0
  if printf '%s' "$has_changelog" | grep -qx "$crate"; then
    has_bullet=1
  fi
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
  elif [[ "$has_bullet" -eq 1 ]] && assembled_into_cut_section "$crate"; then
    echo "OK   ${crate}: bullet folded into the cut '## [${ASSEMBLED_VERSION}]' section (RELEASE WINDOW, #6695)"
  elif [[ "$TRANSITIONAL" -eq 1 ]] && [[ "$has_bullet" -eq 1 ]]; then
    echo "OK   ${crate}: CHANGELOG.md entry (TRANSITIONAL — branch predates #4476)"
  else
    echo "FAIL ${crate}: crates/${crate}/src/** changed with no changelog record" >&2
    # #6695: the author edited CHANGELOG.md and it was not accepted. Say which
    # of the release-window facts was missing, so the next step is obvious.
    if [[ -n "$ASSEMBLED_WHY" ]]; then
      echo "       a CHANGELOG.md bullet is only a record inside a pending release cut: ${ASSEMBLED_WHY}" >&2
    fi
    # #4576: name a nested path when that is what changed, so the reader is not
    # sent to crates/<crate>/src/ to find nothing there.
    sample="$(printf '%s' "$nested_samples" | grep -m1 "^${crate}	" || true)"
    if [[ -n "$sample" ]]; then
      echo "       nested source, e.g. ${sample#*	}" >&2
    fi
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

In the RELEASE WINDOW — the crate's Cargo.toml version already has a
`## [<version>]` section and no `<package>-v<version>` tag exists yet — write the
fragment as usual, then fold it in so both changelog gates agree (issue #6695):

  bash scripts/assemble-changelog.sh <crate> <version> --merge

Preview what will be released:  bash scripts/assemble-changelog.sh <crate> --stdout

Exempt: docs-only, CI-only, test-only, and testdata/ changes, plus a crate this
PR deleted outright (record the removal in a surviving crate's fragment).
EOF
  exit 1
fi

crate_count="$(printf '%s\n' "$needs" | grep -c '[^[:space:]]' || true)"
echo "changelog-fragment gate: scanned ${CHANGED_COUNT} changed path(s); attributed ${attributed_count} crate-source path(s); all ${crate_count} crate(s) with source changes are recorded."

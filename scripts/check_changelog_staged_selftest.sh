#!/usr/bin/env bash
#
# check_changelog_staged_selftest.sh — the author modes of
# scripts/check_changelog_fragment.sh (issue #6947).
#
# Why: the gate diffs `<base>..HEAD`, so before a commit exists it reports
#   `SCAN FLOOR — 0 changed path(s)` and an author cannot check a fragment at
#   all; after the commit it rejects shape errors — `carries a second category
#   inside its bullet body` is the live one — that were knowable the moment the
#   file was written. Two engineers lost a cycle to that on 2026-09-07, one to
#   each half. `--staged` and `--file` answer both questions earlier.
#
#   Neither mode may weaken the gate. `--staged` must still fail a staged source
#   change with no fragment, still reject a malformed one, and still leave the
#   default `<base>..HEAD` verdict untouched — a pre-flight that passes what the
#   gate would fail is worse than no pre-flight, because it is the one an author
#   runs first.
#
# What: builds a throwaway git repo and drives both modes over it.
#
#   --staged cases:
#     staged-src-no-fragment-fails    a staged crates/demo/src/** change with no
#                                     fragment FAILS naming `demo`. This is the
#                                     case that reports SCAN FLOOR against the
#                                     default mode, which is the whole defect.
#     staged-untracked-fragment-ok    the same staged change with the fragment
#                                     written but NOT `git add`-ed passes. An
#                                     untracked-not-ignored path is evidence:
#                                     an author writes the file before staging
#                                     it, and that is when they want the answer.
#     staged-fragment-ok              the same with the fragment staged too.
#     staged-invalid-fragment-fails   a staged two-category fragment FAILS with
#                                     the assembler's own rejection, in the same
#                                     words the post-commit path prints.
#     staged-docs-only-exempt         a staged docs-only change passes, so the
#                                     mode inherits the exemptions rather than
#                                     re-deriving them.
#     staged-empty-index-scan-floor   a clean tree with nothing staged and
#                                     nothing untracked FAILS with SCAN FLOOR.
#                                     The floor is relaxed to "at least one
#                                     staged or untracked path", never removed —
#                                     a run that scans nothing is still not a
#                                     pass (#4618).
#     staged-ignored-file-not-scanned an ignored untracked file is not a scanned
#                                     path, so a tree holding only one still
#                                     hits the floor.
#     default-mode-unlabelled         the default run over the same repo reports
#                                     its ordinary summary with no mode label —
#                                     the CI verdict is byte-for-byte what it
#                                     was.
#
#   --file cases:
#     file-valid-fragment             exit 0.
#     file-two-headings-fails         exit 1 naming the second category. This is
#                                     the error that cost a commit-amend cycle.
#     file-missing-category-fails     exit 1 naming the unknown category.
#     file-empty-fails                exit 1 — an empty file is not a fragment.
#     file-nested-path-fails          a fragment one directory too deep is
#                                     rejected on PLACEMENT, before its content
#                                     is read; the assembler drops one silently
#                                     from the release.
#     file-readme-placeholder-fails   changelog.d/README.md is the tracked
#                                     directory placeholder, never evidence.
#     file-missing-file-fails         a path that does not exist.
#     file-needs-no-git-history       --file works with the base ref absent and
#                                     the fragment uncommitted — the mode's
#                                     entire point is answering before there is
#                                     anything to diff.
#
#   Usage cases: two author modes at once, and an author mode with --base, are
#   both exit 2. A mode conflict that silently ran one of the two is how a
#   verdict gets attributed to flags nobody passed.
#
# Usage:
#   bash scripts/check_changelog_staged_selftest.sh
#   bash scripts/check_changelog_staged_selftest.sh --gate /path/to/gate.sh
#
#   `--gate` runs the cases against an ALTERNATE copy of the gate. Pointing it
#   at the pre-#6947 script is how the mutation is demonstrated: that run must
#   FAIL here, proving these cases are not passing by construction.
#
# Exit: 0 when every case holds; 1 (naming the case) when one does not.
#
# Test: this IS the test. It is wired into
#   .github/workflows/changelog-fragment.yml ahead of the real gate run.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only. Same constraints as the script under test.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

GATE_SOURCE="$SCRIPT_DIR/check_changelog_fragment.sh"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --gate)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --gate needs a path" >&2
        exit 2
      }
      GATE_SOURCE="$2"
      shift 2
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

[[ -f "$GATE_SOURCE" ]] || {
  echo "ERROR: no gate script at '$GATE_SOURCE'" >&2
  exit 2
}

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/changelog-staged-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

REPO="$TMP_ROOT/repo"
GATE="scripts/check_changelog_fragment.sh"
fail=0

g() { git -C "$REPO" "$@"; }

# ---------------------------------------------------------------------------
# Fixture: one crate, plus the docs path used by the exemption case.
# ---------------------------------------------------------------------------
mkdir -p "$REPO/scripts/lib" "$REPO/crates/demo/src" \
  "$REPO/crates/demo/changelog.d" "$REPO/docs"
cp "$GATE_SOURCE" "$REPO/scripts/check_changelog_fragment.sh"
cp "$SCRIPT_DIR/assemble-changelog.sh" "$REPO/scripts/"
# #5765: the gate sources its path classification from `scripts/lib/` beside
# itself, so the library travels with it into the synthetic repo.
cp "$SCRIPT_DIR/lib/source_class.sh" "$REPO/scripts/lib/"

printf 'pub fn v() -> u32 { 1 }\n' >"$REPO/crates/demo/src/lib.rs"
printf '[package]\nname = "demo"\nversion = "0.1.0"\n' >"$REPO/crates/demo/Cargo.toml"
printf '# Changelog\n\n---\n' >"$REPO/crates/demo/CHANGELOG.md"
printf 'Placeholder keeping changelog.d/ tracked between releases.\n' \
  >"$REPO/crates/demo/changelog.d/README.md"
printf '# Docs\n' >"$REPO/docs/notes.md"
printf 'target/\n*.log\n' >"$REPO/.gitignore"

g init -q -b main
g config user.email selftest@example.invalid
g config user.name "changelog staged self-test"
g add -A
g commit -qm "M0: demo crate"
# `main` stays parked at the fixture commit and is the base every case diffs
# against; the work happens on `work`. A case that COMMITS (the default-mode
# one) needs a base that is not its own branch tip — with both on `main` the
# merge base is HEAD and the default run reports SCAN FLOOR, which is the
# defect under test rather than a verdict.
BASE_SHA="$(g rev-parse HEAD)"

# ---------------------------------------------------------------------------
# Fragment bodies. Written with printf, not a heredoc, so every byte is visible
# at the call site and the file ends exactly where it is meant to.
# ---------------------------------------------------------------------------
write_valid_fragment() { printf 'Fixed\n\n- a user-visible change\n' >"$1"; }
write_two_heading_fragment() {
  printf 'Fixed\n\n- a user-visible change\n\nChanged\n\n- a second one\n' >"$1"
}
write_no_category_fragment() { printf -- '- a bullet with no category above it\n' >"$1"; }

# ---------------------------------------------------------------------------
# assert_gate: name, expected exit status, ERE the output must match, ERE it
# must NOT match ("" to skip either), then the gate's arguments.
#
# Capture, then match. NOT `... | grep -q`: under `set -o pipefail` the gate's
# own (expected) non-zero exit becomes the pipeline's status even when grep
# matched, so every rejection assertion would read as a miss.
# ---------------------------------------------------------------------------
assert_gate() {
  local name="$1" want_rc="$2" want_re="$3" deny_re="$4" out rc=0
  shift 4
  out="$(cd "$REPO" && CHANGELOG_GATE_BASE=main bash "$GATE" "$@" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_rc" ]; then
    echo "FAIL: $name -> exit $rc (expected $want_rc)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  if [ -n "$want_re" ] && ! grep -qE "$want_re" <<<"$out"; then
    echo "FAIL: $name -> exit $rc as expected, but output does not match /$want_re/" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  if [ -n "$deny_re" ] && grep -qE "$deny_re" <<<"$out"; then
    echo "FAIL: $name -> exit $rc as expected, but output MATCHES forbidden /$deny_re/" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  echo "PASS: $name -> exit $rc${want_re:+, matched /$want_re/}"
}

# reset_tree: a fresh `work` branch at the fixture commit with no staged,
# unstaged or untracked change, so each case starts from the same place.
reset_tree() {
  g checkout -q -B work "$BASE_SHA"
  g reset -q --hard
  g clean -qfd
}

# ===========================================================================
# --staged
# ===========================================================================

# 1. THE #6947 DEFECT, first half. Staged source, no fragment anywhere. The
#    default mode reports SCAN FLOOR here because nothing is committed; this
#    mode must report the real verdict.
reset_tree
printf 'pub fn v() -> u32 { 2 }\n' >"$REPO/crates/demo/src/lib.rs"
g add -A
assert_gate staged-src-no-fragment-fails 1 \
  'FAIL demo: crates/demo/src/\*\* changed with no changelog record' \
  'SCAN FLOOR' \
  --staged

# 2. An untracked fragment is evidence. This is the state an author is in
#    between writing the file and staging it.
reset_tree
printf 'pub fn v() -> u32 { 3 }\n' >"$REPO/crates/demo/src/lib.rs"
g add crates/demo/src/lib.rs
write_valid_fragment "$REPO/crates/demo/changelog.d/6947-staged-mode.md"
assert_gate staged-untracked-fragment-ok 0 \
  'OK   demo: changelog.d fragment present and valid' \
  'FAIL' \
  --staged

# 3. The fully staged shape, and the mode label that keeps a --staged verdict
#    from being pasted as the gate's.
reset_tree
printf 'pub fn v() -> u32 { 4 }\n' >"$REPO/crates/demo/src/lib.rs"
write_valid_fragment "$REPO/crates/demo/changelog.d/6947-staged-mode.md"
g add -A
assert_gate staged-fragment-ok 0 \
  'changelog-fragment gate: --staged \(index \+ untracked\):' \
  'FAIL' \
  --staged

# 4. THE #6947 DEFECT, second half. The shape error is caught while the change
#    is still staged, in the assembler's own words.
reset_tree
printf 'pub fn v() -> u32 { 5 }\n' >"$REPO/crates/demo/src/lib.rs"
write_two_heading_fragment "$REPO/crates/demo/changelog.d/6947-two-categories.md"
g add -A
assert_gate staged-invalid-fragment-fails 1 \
  'carries a second category inside its bullet body' \
  '' \
  --staged

# 5. The exemptions are inherited, not re-derived.
reset_tree
printf '# Docs\n\nchanged\n' >"$REPO/docs/notes.md"
g add -A
assert_gate staged-docs-only-exempt 0 \
  'no crate source changed \(docs-only / CI-only / test-only\) — OK' \
  'FAIL' \
  --staged

# 6. The floor is relaxed, not removed (#4618). Nothing staged, nothing
#    untracked: a run that examined nothing is not a passing run.
reset_tree
assert_gate staged-empty-index-scan-floor 1 \
  'SCAN FLOOR — the index against HEAD plus untracked files lists 0' \
  '' \
  --staged

# 7. An ignored file is not a scanned path. `--exclude-standard` is what makes
#    a build directory full of artefacts fail to satisfy the floor.
reset_tree
mkdir -p "$REPO/target"
printf 'artefact\n' >"$REPO/target/out.bin"
printf 'noise\n' >"$REPO/debug.log"
assert_gate staged-ignored-file-not-scanned 1 \
  'SCAN FLOOR' \
  '' \
  --staged

# 8. THE DEFAULT VERDICT IS UNTOUCHED. Same repo, committed, default mode: the
#    summary carries no mode label, so what CI reports is what it always was.
reset_tree
printf 'pub fn v() -> u32 { 6 }\n' >"$REPO/crates/demo/src/lib.rs"
write_valid_fragment "$REPO/crates/demo/changelog.d/6947-staged-mode.md"
g add -A
g commit -qm "source change with a fragment"
assert_gate default-mode-unlabelled 0 \
  'changelog-fragment gate: scanned [0-9]+ changed path\(s\)' \
  '\-\-staged'

# ===========================================================================
# --file
# ===========================================================================
reset_tree

FRAG_DIR="$TMP_ROOT/frags"
mkdir -p "$FRAG_DIR"
write_valid_fragment "$FRAG_DIR/valid.md"
write_two_heading_fragment "$FRAG_DIR/two-headings.md"
write_no_category_fragment "$FRAG_DIR/no-category.md"
: >"$FRAG_DIR/empty.md"

assert_gate file-valid-fragment 0 \
  'OK   .*valid\.md: valid changelog fragment' \
  'FAIL' \
  --file "$FRAG_DIR/valid.md"

assert_gate file-two-headings-fails 1 \
  'carries a second category inside its bullet body' \
  '' \
  --file "$FRAG_DIR/two-headings.md"

assert_gate file-missing-category-fails 1 \
  "has an unknown category" \
  '' \
  --file "$FRAG_DIR/no-category.md"

assert_gate file-empty-fails 1 \
  'is empty — the first non-blank line must be a category' \
  '' \
  --file "$FRAG_DIR/empty.md"

# Placement, checked before the content is read: a nested fragment is dropped
# from the release by the assembler, so a green here would be a lie.
mkdir -p "$REPO/crates/demo/changelog.d/sub"
write_valid_fragment "$REPO/crates/demo/changelog.d/sub/6947-nested.md"
assert_gate file-nested-path-fails 1 \
  'not a fragment path' \
  'OK ' \
  --file crates/demo/changelog.d/sub/6947-nested.md

assert_gate file-readme-placeholder-fails 1 \
  'tracked directory' \
  'OK ' \
  --file crates/demo/changelog.d/README.md

assert_gate file-missing-file-fails 1 \
  'no such file' \
  '' \
  --file crates/demo/changelog.d/6947-not-written-yet.md

# The mode reads no git history: the base ref does not exist, the fragment is
# untracked, and the answer is still correct.
reset_tree
write_valid_fragment "$REPO/crates/demo/changelog.d/6947-uncommitted.md"
file_out=""
file_rc=0
file_out="$(cd "$REPO" && CHANGELOG_GATE_BASE=refs/heads/no-such-base \
  bash "$GATE" --file crates/demo/changelog.d/6947-uncommitted.md 2>&1)" || file_rc=$?
if [ "$file_rc" -eq 0 ] && grep -q 'valid changelog fragment' <<<"$file_out"; then
  echo "PASS: file-needs-no-git-history -> exit 0 with an absent base ref"
else
  echo "FAIL: file-needs-no-git-history -> exit $file_rc" >&2
  printf '%s\n' "$file_out" | sed 's/^/       /' >&2
  fail=1
fi

# ===========================================================================
# Usage. A mode conflict must refuse, never pick one silently.
# ===========================================================================
reset_tree
assert_gate usage-two-modes-refused 2 \
  'are different questions' \
  '' \
  --staged --file "$FRAG_DIR/valid.md"

assert_gate usage-staged-with-base-refused 2 \
  'neither --staged nor --file reads' \
  '' \
  --staged --base main

assert_gate usage-file-with-base-refused 2 \
  'neither --staged nor --file reads' \
  '' \
  --file "$FRAG_DIR/valid.md" --base main

echo
if [ "$fail" -ne 0 ]; then
  echo "check_changelog_staged_selftest: FAILED — the author modes do not hold" >&2
  echo "  (issue #6947). Gate under test: ${GATE_SOURCE}" >&2
  exit 1
fi
echo "check_changelog_staged_selftest: all author-mode cases passed."

#!/usr/bin/env bash
#
# check_changelog_release_window_selftest.sh — the two changelog gates must
# agree on a same-commit `--merge` assembly (issue #6695).
#
# Why: `scripts/check_changelog_fragment.sh` demanded a fragment file and
#   `scripts/check-changelog-assembled.sh` demanded that no fragment survive, so
#   a source fix landing between a release cut and its publish could not satisfy
#   both. Running `assemble-changelog.sh <crate> <version> --merge` — which is
#   what clears the assembled gate — consumes the fragment the same commit
#   added, leaving a diff with a CHANGELOG.md bullet and no changelog.d path at
#   all. The fragment gate read that as an omission. Observed 2026-09-02 on
#   branch fix/prepublish-doc-links-20260902 (ffe03c23c): assembled gate PASS
#   for trusty-common and trusty-mpm, fragment gate EXIT=1 for both.
#
#   Neither the contradiction nor its fix had a test. This is it.
#
# What: builds a throwaway git repo carrying the exact shape — one crate whose
#   0.1.1 section is cut but not yet tagged — and asserts both gates' verdicts
#   on each branch shape.
#
#   Cases:
#     merge-window-passes-both   the branch changes src, writes a fragment and
#                                folds it in with `--merge`. BOTH gates pass.
#                                This is the case that goes red against the
#                                pre-#6695 fragment gate.
#     tagged-section-rejected    the same tree once crate-a-v0.1.1 exists. That
#                                section is released history, so a bullet
#                                written into it is a back-dated record, not
#                                evidence, and the gate still fails.
#     stranded-fragment-fails    a hand-written bullet while another fragment
#                                sits unconsumed in changelog.d/. The assembled
#                                gate rejects that state, so the fragment gate
#                                must too — the two agree in BOTH directions.
#     bullet-outside-cut-fails   a bullet added under the older `## [0.1.0]`
#                                section is not a record of the pending cut.
#     no-record-still-fails      a source change with no changelog record at all
#                                still fails. The fix must not weaken the gate.
#
# Test: this IS the test. Run directly:
#   bash scripts/check_changelog_release_window_selftest.sh
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only. Same constraints as the scripts under test.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/changelog-window-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

REPO="$TMP_ROOT/repo"
GATE="scripts/check_changelog_fragment.sh"
ASSEMBLED="scripts/check-changelog-assembled.sh"
fail=0

g() { git -C "$REPO" "$@"; }

# ---------------------------------------------------------------------------
# Fixture: one crate, a release cut that wrote `## [0.1.1]` and consumed the
# fragments behind it, and an earlier tag so the "has this shipped" probe has
# tags to read.
# ---------------------------------------------------------------------------
mkdir -p "$REPO/scripts/lib"
cp "$SCRIPT_DIR/check_changelog_fragment.sh" "$REPO/scripts/"
cp "$SCRIPT_DIR/assemble-changelog.sh" "$REPO/scripts/"
cp "$SCRIPT_DIR/check-changelog-assembled.sh" "$REPO/scripts/"
cp "$SCRIPT_DIR/lib/source_class.sh" "$REPO/scripts/lib/"

CRATE="$REPO/crates/crate-a"
mkdir -p "$CRATE/src" "$CRATE/changelog.d"
printf 'pub fn v() -> u32 { 1 }\n' >"$CRATE/src/lib.rs"
printf '[package]\nname = "crate-a"\nversion = "0.1.0"\n' >"$CRATE/Cargo.toml"
printf 'Placeholder keeping changelog.d/ tracked between releases.\n' \
  >"$CRATE/changelog.d/README.md"
cat >"$CRATE/CHANGELOG.md" <<'EOF'
# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.1.0] — 2026-08-01

### Added

- initial release
EOF

g init -q -b main
g config user.email selftest@example.invalid
g config user.name "changelog release-window self-test"
g add -A
g commit -qm "M0: crate-a 0.1.0"
g tag crate-a-v0.1.0

# The release cut (#6693's shape): version bumped, section written, fragments
# consumed. Nothing is tagged yet — this is the window the fix is about.
printf '[package]\nname = "crate-a"\nversion = "0.1.1"\n' >"$CRATE/Cargo.toml"
python3 - "$CRATE/CHANGELOG.md" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
cut = """## [0.1.1] — 2026-09-01

### Fixed

- the bullet the release cut assembled

"""
s = s.replace("## [0.1.0]", cut + "## [0.1.0]")
open(p, "w").write(s)
PY
g add -A
g commit -qm "M1: cut crate-a 0.1.1 (section written, not yet tagged)"

# ---------------------------------------------------------------------------
# assert_gate: name, expected exit status, ERE the output must match.
# ---------------------------------------------------------------------------
assert_gate() {
  local name="$1" want_rc="$2" want_re="$3" out rc=0
  # Capture, then match. NOT `... | grep -q`: under `set -o pipefail` the gate's
  # own (expected) non-zero exit becomes the pipeline's status even when grep
  # matched, so every rejection assertion would read as a miss.
  out="$(cd "$REPO" && CHANGELOG_GATE_BASE=main bash "$GATE" 2>&1)" || rc=$?
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
  echo "PASS: $name -> exit $rc${want_re:+, matched /$want_re/}"
}

# ---------------------------------------------------------------------------
# assert_assembled: name, expected exit status. Runs the OTHER gate on the same
# tree — the point of #6695 is that the two verdicts agree.
# ---------------------------------------------------------------------------
assert_assembled() {
  local name="$1" want_rc="$2" out rc=0
  out="$(cd "$REPO" && bash "$ASSEMBLED" crate-a 0.1.1 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_rc" ]; then
    echo "FAIL: $name -> exit $rc (expected $want_rc)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  echo "PASS: $name -> exit $rc"
}

reset_branch() {
  g checkout -q main
  g branch -q -D pr 2>/dev/null || true
  g checkout -q -b pr
}

# ---------------------------------------------------------------------------
# 1. THE #6695 DEFECT. A source fix in the release window writes its fragment
#    and folds it in with --merge, which is the ONLY way to clear the assembled
#    gate. Both gates must pass. Against the pre-fix script this case reports
#    "FAIL crate-a: crates/crate-a/src/** changed with no changelog record".
# ---------------------------------------------------------------------------
reset_branch
printf 'pub fn v() -> u32 { 2 }\n' >"$CRATE/src/lib.rs"
printf 'Fixed\n\n- a user-visible fix landing inside the release window\n' \
  >"$CRATE/changelog.d/6695-window-fix.md"
(cd "$REPO" && bash scripts/assemble-changelog.sh crate-a 0.1.1 --merge >/dev/null)
g add -A
g commit -qm "PR: src fix + fragment, folded into [0.1.1] by --merge"

# The diff shape this turns on: a CHANGELOG.md bullet and NO changelog.d path,
# because the fragment was added and consumed in the same commit. Assert it, so
# a future fixture drift cannot quietly test a different (easier) shape.
if [ -n "$(g diff --name-only main HEAD -- crates/crate-a/changelog.d/)" ]; then
  echo "FAIL: fixture drift — the branch diff still carries a changelog.d path," >&2
  echo "      so it is not the ffe03c23c shape this test exists for." >&2
  fail=1
fi

assert_gate merge-window-passes-both 0 \
  "OK   crate-a: bullet folded into the cut '## \[0\.1\.1\]' section"
assert_assembled merge-window-assembled-agrees 0

# ---------------------------------------------------------------------------
# 2. NOT A BLANK CHEQUE. Once 0.1.1 is tagged, that section is released
#    history and a bullet written into it back-dates the change.
# ---------------------------------------------------------------------------
g tag crate-a-v0.1.1
assert_gate tagged-section-rejected 1 'is already tagged'
g tag -d crate-a-v0.1.1 >/dev/null

# ---------------------------------------------------------------------------
# 3. THE OTHER DIRECTION. A hand-written bullet while a fragment still sits
#    unconsumed is the state the assembled gate fails on, so this gate must
#    fail it too.
# ---------------------------------------------------------------------------
reset_branch
printf 'pub fn v() -> u32 { 3 }\n' >"$CRATE/src/lib.rs"
printf 'Fixed\n\n- somebody else pending work\n' >"$CRATE/changelog.d/9000-other.md"
g add -A
g commit -qm "PR: another crate-a fragment left pending"
g checkout -q main
g merge -q --ff-only pr
reset_branch
printf 'pub fn v() -> u32 { 4 }\n' >"$CRATE/src/lib.rs"
python3 - "$CRATE/CHANGELOG.md" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("- the bullet the release cut assembled",
              "- the bullet the release cut assembled\n- a bullet typed in by hand")
open(p, "w").write(s)
PY
g add -A
g commit -qm "PR: src fix + hand-written bullet, fragment left stranded"
assert_gate stranded-fragment-fails 1 'check-changelog-assembled.sh still rejects'
assert_assembled stranded-fragment-assembled-agrees 1

# Put the fixture back on a clean cut for the remaining cases.
g checkout -q main
g rm -q "crates/crate-a/changelog.d/9000-other.md"
g commit -qm "M2: drop the stranded fragment"

# ---------------------------------------------------------------------------
# 4. THE SECTION MATTERS. A bullet under the previous, already-shipped section
#    records nothing about the cut about to be published.
# ---------------------------------------------------------------------------
reset_branch
printf 'pub fn v() -> u32 { 5 }\n' >"$CRATE/src/lib.rs"
python3 - "$CRATE/CHANGELOG.md" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("- initial release", "- initial release\n- a bullet in the wrong section")
open(p, "w").write(s)
PY
g add -A
g commit -qm "PR: src fix + bullet under the old [0.1.0] section"
assert_gate bullet-outside-cut-fails 1 'land outside'

# ---------------------------------------------------------------------------
# 5. STILL A GATE. No record at all must still fail.
# ---------------------------------------------------------------------------
reset_branch
printf 'pub fn v() -> u32 { 6 }\n' >"$CRATE/src/lib.rs"
g add -A
g commit -qm "PR: src fix with no changelog record"
assert_gate no-record-still-fails 1 \
  'FAIL crate-a: crates/crate-a/src/\*\* changed with no changelog record'

echo
if [ "$fail" -ne 0 ]; then
  echo "check_changelog_release_window_selftest: FAILED — the two changelog gates" >&2
  echo "  do not agree on a same-commit --merge assembly (issue #6695)." >&2
  exit 1
fi
echo "check_changelog_release_window_selftest: all release-window cases passed."

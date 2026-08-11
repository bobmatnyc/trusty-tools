#!/usr/bin/env bash
#
# assemble_changelog_selftest.sh — fixture regressions for the changelog
# fragment validator in scripts/assemble-changelog.sh.
#
# Why: a malformed fragment used to be silently MIS-RENDERED rather than
#   rejected. The real case, during the trusty-mpm 1.3.3 release:
#   `crates/trusty-mpm/changelog.d/4286-retire-trusty-mpm-override-files.md`
#   packed four categories into one file. Line 1 is the only category line and
#   everything after it is copied through verbatim, so the bare `Changed`,
#   `Added` and `Fixed` lines became body text and all four categories' bullets
#   rendered under `### Removed`. Line 1 was valid and bullets were present, so
#   every check the assembler had passed it. A human diffing the `--stdout`
#   preview caught it; nothing in the tooling would have, and the mis-rendered
#   section would have shipped in CHANGELOG.md permanently. The exact file is
#   pinned verbatim as scripts/test-data/changelog-fragment-four-categories.md
#   and replayed below.
#
# What: copies the REAL scripts/assemble-changelog.sh into a throwaway workspace
#   (WORKSPACE_ROOT derives from the script's own location, so a temp
#   `<tmp>/scripts/` + `<tmp>/crates/` tree exercises the whole script including
#   main(), with zero risk to the checkout), synthesizes one crate per case, and
#   asserts the exit status and diagnostic of `<crate> --stdout`.
#
#   Cases: the four-category replay, a valid single-category fragment, an
#   invalid line-1 category, a nested fragment, an empty fragment, a category
#   with no bullet, and the false-positive guard — a bullet that legitimately
#   BEGINS with a category word, plus an indented continuation line that is one,
#   must both pass.
#
#   Three more came out of the adversarial review of PR #4686:
#     - fence guard: a bare category inside ```, ~~~, a longer run, or a fence
#       with an info string is CONTENT and must pass;
#     - heading form: `## Changed` / `### Fixed ###` is the same defect in
#       different markup and must be rejected;
#     - wrapped-continuation: pinned as a deliberate KNOWN LIMITATION (still
#       rejected), including the assertion that the error states the remedy.
#
#   Issue #5298 adds the stale-section family, replayed from PR #4824 — a
#   `## [1.3.5]` section assembled for a cut that never published, with newer
#   fragments accumulating behind it. Against the pre-#5298 script the refusal
#   named neither the stranded fragments nor a way forward, and `--merge` did
#   not exist, so every case below FAILS on that script. Covered: the refusal
#   enumerates the pending fragments and points at `--merge`; `--merge` appends
#   to a category the section already has; `--merge` inserts a missing category
#   in CATEGORY_ORDER position without duplicating an existing heading; sections
#   above and below are untouched; fragments are consumed; and `--merge` against
#   a section that does not exist is refused rather than silently downgraded to
#   a fresh insert.
#
# Test: this IS the test. Run directly:
#   bash scripts/assemble_changelog_selftest.sh
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only. Same constraints as the script under test.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE_DIR="$SCRIPT_DIR/test-data"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/assemble-changelog-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

mkdir -p "$TMP_ROOT/scripts"
cp "$SCRIPT_DIR/assemble-changelog.sh" "$TMP_ROOT/scripts/assemble-changelog.sh"
ASSEMBLER="$TMP_ROOT/scripts/assemble-changelog.sh"

fail=0

# Creates crates/<name>/ with a minimal CHANGELOG.md and an empty changelog.d/,
# and prints the fragment directory.
new_crate() {
  local name="$1" dir="$TMP_ROOT/crates/$1"
  mkdir -p "$dir/changelog.d"
  cat >"$dir/CHANGELOG.md" <<'EOF'
# Changelog

---
EOF
  echo "$dir/changelog.d"
}

# case name, expected exit status (0 = accept, 1 = reject), then an ERE that the
# combined output must match ("" to skip the match).
assert_case() {
  local name="$1" want_rc="$2" want_re="$3" out rc=0
  out="$(bash "$ASSEMBLER" "$name" --stdout 2>&1)" || rc=$?
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
# 1. THE REAL DEFECT, replayed verbatim. Four categories in one fragment.
# ---------------------------------------------------------------------------
d="$(new_crate replay-four-categories)"
cp "$FIXTURE_DIR/changelog-fragment-four-categories.md" \
  "$d/4286-retire-trusty-mpm-override-files.md"
assert_case replay-four-categories 1 'carries a second category inside its bullet body'

# The diagnostic must name the file and the exact line of EACH smuggled
# category — an error that says only "something is wrong" is not actionable.
#
# Capture first, then match. NOT `bash "$ASSEMBLER" … | grep -q`: under
# `set -o pipefail` the assembler's own (expected) exit 1 becomes the pipeline's
# status even when grep matched, so every assertion here would read as a miss.
# The script under test carries a comment about the same trap.
replay_out="$(bash "$ASSEMBLER" replay-four-categories --stdout 2>&1 || true)"
for stray in '17: Changed' '30: Added' '40: Fixed'; do
  if grep -qF "4286-retire-trusty-mpm-override-files.md:$stray" <<<"$replay_out"; then
    echo "PASS: replay names 4286-retire-trusty-mpm-override-files.md:$stray"
  else
    echo "FAIL: replay does not name 4286-retire-trusty-mpm-override-files.md:$stray" >&2
    fail=1
  fi
done

# And nothing may be rendered: a rejected fragment set must produce no section.
if [ -z "$(bash "$ASSEMBLER" replay-four-categories --stdout 2>/dev/null || true)" ]; then
  echo "PASS: replay renders no section on stdout"
else
  echo "FAIL: replay rejected but still wrote a section to stdout" >&2
  fail=1
fi

# ---------------------------------------------------------------------------
# 2. A valid single-category fragment still assembles.
# ---------------------------------------------------------------------------
d="$(new_crate valid-single-category)"
cat >"$d/1234-valid.md" <<'EOF'
Fixed

- pm_guard no longer scans quoted content (#1234)
  - indented sub-bullets are preserved verbatim
EOF
assert_case valid-single-category 0 '^### Fixed$'
assert_case valid-single-category 0 '^  - indented sub-bullets are preserved verbatim$'

# ---------------------------------------------------------------------------
# 3. FALSE-POSITIVE GUARD. A bullet may legitimately begin with a category
#    word, and an indented continuation line may BE one. Neither is a smuggled
#    heading; both must pass.
# ---------------------------------------------------------------------------
d="$(new_crate false-positive-guard)"
cat >"$d/1235-guard.md" <<'EOF'
Changed

- Changed the default timeout from 30s to 10s (#1235)
- Removed
  Removed
  Security
- Added support for the `--stdout` preview
EOF
assert_case false-positive-guard 0 '^### Changed$'
assert_case false-positive-guard 0 '^- Changed the default timeout from 30s to 10s'

# ---------------------------------------------------------------------------
# 3b. FENCE GUARD (review finding 1). A bare category word inside a fenced code
#     block is CONTENT — a fragment may document the fragment format itself, or
#     show example assembler output. Covers ```, ~~~, a run longer than three,
#     and an info string. Whole-line matching that ignored fences is the
#     seeded-stub bug shape from #4286; this pins that it cannot come back.
# ---------------------------------------------------------------------------
d="$(new_crate fence-guard)"
cat >"$d/1240-fences.md" <<'EOF'
Documentation

- Documents the fragment format. Every fenced line below is content, not a
  heading, and none of it may be read as a second category.

```
Fixed
```

~~~
Changed
~~~

````text
Removed
## Security
````
EOF
assert_case fence-guard 0 '^### Documentation$'
assert_case fence-guard 0 '^Fixed$'
assert_case fence-guard 0 '^## Security$'

# ---------------------------------------------------------------------------
# 3c. HEADING FORM (review finding 2). A second category written as a markdown
#     heading is the same defect in different markup, and structurally worse: a
#     stray `## …` inside a release section splits it for anything parsing on
#     `^## ` boundaries, including the cliff.toml GitHub Release body step.
# ---------------------------------------------------------------------------
d="$(new_crate heading-form-category)"
cat >"$d/1241-heading-form.md" <<'EOF'
Removed

- the thing that was removed (#1241)

## Changed

- a bullet that would land under Removed, below a stray heading

### Fixed ###

- and another
EOF
assert_case heading-form-category 1 'carries a second category inside its bullet body'

heading_out="$(bash "$ASSEMBLER" heading-form-category --stdout 2>&1 || true)"
for stray in '5: ## Changed' '9: ### Fixed ###'; do
  if grep -qF "1241-heading-form.md:$stray" <<<"$heading_out"; then
    echo "PASS: heading form names 1241-heading-form.md:$stray"
  else
    echo "FAIL: heading form does not name 1241-heading-form.md:$stray" >&2
    fail=1
  fi
done

# A heading-form category INSIDE a fence is still content — the two rules must
# not fight. Covered by the `## Security` assertion in the fence-guard case.

# ---------------------------------------------------------------------------
# 3d. KNOWN LIMITATION (review finding 3), pinned deliberately. An unindented
#     hard-wrapped continuation line that is solely a category word is still
#     rejected. Suppressing it needs a "preceded by a blank line" rule, which
#     would let a fragment written without blank lines between its categories
#     sail through — reopening the defect this guard exists to close. A false
#     positive costs one indent; a false negative ships a mis-rendered
#     CHANGELOG.md permanently. The error must state that remedy.
# ---------------------------------------------------------------------------
d="$(new_crate wrapped-continuation-limitation)"
cat >"$d/1242-wrapped.md" <<'EOF'
Fixed

- Renamed the response status value from
Changed
to Resolved (#1242)
EOF
assert_case wrapped-continuation-limitation 1 'carries a second category inside its bullet body'
assert_case wrapped-continuation-limitation 1 'indent it'

# ---------------------------------------------------------------------------
# 4. An invalid line-1 category is rejected.
# ---------------------------------------------------------------------------
d="$(new_crate bad-line-one)"
cat >"$d/1236-bad-category.md" <<'EOF'
Improvements

- something user-visible (#1236)
EOF
assert_case bad-line-one 1 "has an unknown category 'Improvements'"

# ---------------------------------------------------------------------------
# 5. A nested fragment is rejected outright, never silently skipped.
# ---------------------------------------------------------------------------
d="$(new_crate nested-fragment)"
mkdir -p "$d/sub"
cat >"$d/sub/1237-nested.md" <<'EOF'
Added

- a bullet that would never be assembled (#1237)
EOF
assert_case nested-fragment 1 'must sit directly in'

# ---------------------------------------------------------------------------
# 6. An empty fragment is rejected.
# ---------------------------------------------------------------------------
d="$(new_crate empty-fragment)"
: >"$d/1238-empty.md"
assert_case empty-fragment 1 'is empty'

# ---------------------------------------------------------------------------
# 7. A category with no bullet is rejected.
# ---------------------------------------------------------------------------
d="$(new_crate bodyless-fragment)"
cat >"$d/1239-bodyless.md" <<'EOF'
Security

we tightened some things
EOF
assert_case bodyless-fragment 1 'has a category but no bullet'

# ---------------------------------------------------------------------------
# 8. README.md is the tracked directory placeholder, never a fragment. A crate
#    holding only it must not be read as holding a malformed fragment.
# ---------------------------------------------------------------------------
d="$(new_crate readme-placeholder-only)"
cat >"$d/README.md" <<'EOF'
This directory holds per-PR changelog fragments.

Removed
Changed
EOF
assert_case readme-placeholder-only 0 'no changelog fragments pending'

# ===========================================================================
# ISSUE #5298 — a stale `## [<version>]` section must never strand fragments.
#
# Replays PR #4824: `## [1.3.5]` was assembled for a cut that never published,
# consuming its fragments, and newer fragments then accumulated behind it. The
# fixture below carries all three placement shapes in one section — a category
# the section already has (Fixed, appended to), one that sorts BEFORE an
# existing heading (Breaking), and one that sorts after every existing heading
# (Security, appended at the section end).
# ===========================================================================

# Creates crates/<name>/ carrying the stale [1.3.5] section plus an already
# released [1.3.4] section below it, and prints the crate directory.
new_stale_crate() {
  local name="$1" dir="$TMP_ROOT/crates/$1"
  mkdir -p "$dir/changelog.d"
  cat >"$dir/CHANGELOG.md" <<'EOF'
# Changelog

---

## [1.3.5] — 2026-08-04

### Added

- an entry consumed by the cut that never published (#4824)

### Fixed

- another entry consumed by that cut (#4824)

## [1.3.4] — 2026-08-03

### Breaking

- a heading that belongs to a DIFFERENT section; the scan for "which headings
  does the target section already have" must not see it, or `### Breaking`
  would be deferred to the end of [1.3.5] instead of leading it

### Fixed

- older, already released and tagged
EOF
  echo "$dir"
}

# Same stale [1.3.5] section, but it is the LAST thing in the file — the shape a
# crate's first release has. The merger reaches it through END rather than
# through the next `## ` heading.
new_stale_eof_crate() {
  local name="$1" dir="$TMP_ROOT/crates/$1"
  mkdir -p "$dir/changelog.d"
  cat >"$dir/CHANGELOG.md" <<'EOF'
# Changelog

---

## [1.3.5] — 2026-08-04

### Added

- an entry consumed by the cut that never published (#4824)
EOF
  echo "$dir"
}

seed_stale_fragments() {
  local d="$1/changelog.d"
  cat >"$d/4900-later-fixed.md" <<'EOF'
Fixed

- a bullet that accumulated AFTER the stale section was written (#4900)
EOF
  cat >"$d/4901-later-security.md" <<'EOF'
Security

- a category the stale section does not carry at all (#4901)
EOF
  cat >"$d/4902-later-breaking.md" <<'EOF'
Breaking

- a category that sorts ahead of every heading the section has (#4902)
EOF
}

# ---------------------------------------------------------------------------
# 9. The default refusal must ENUMERATE the stranded fragments and name the way
#    forward. A bare "refusing to insert a second one" is the dead end that made
#    #4824 cost a manual reverse-apply of a merged commit.
# ---------------------------------------------------------------------------
stale_dir="$(new_stale_crate stale-refusal)"
seed_stale_fragments "$stale_dir"

stale_rc=0
stale_out="$(bash "$ASSEMBLER" stale-refusal 1.3.5 2>&1)" || stale_rc=$?
if [ "$stale_rc" -ne 1 ]; then
  echo "FAIL: stale-refusal -> exit $stale_rc (expected 1)" >&2
  fail=1
else
  echo "PASS: stale-refusal -> exit 1"
fi
for want in \
  'changelog.d/4900-later-fixed.md' \
  'changelog.d/4901-later-security.md' \
  'changelog.d/4902-later-breaking.md' \
  '--merge'; do
  if grep -qF -- "$want" <<<"$stale_out"; then
    echo "PASS: stale-refusal diagnostic names $want"
  else
    echo "FAIL: stale-refusal diagnostic does not name $want" >&2
    printf '%s\n' "$stale_out" | sed 's/^/       /' >&2
    fail=1
  fi
done

# The refusal must also change nothing — neither the file nor the fragments.
if [ "$(find "$stale_dir/changelog.d" -maxdepth 1 -name '*.md' | wc -l | tr -d ' ')" = "3" ]; then
  echo "PASS: stale-refusal leaves all 3 fragments in place"
else
  echo "FAIL: stale-refusal consumed or lost fragments" >&2
  fail=1
fi

# `--check` is what bump-version.sh runs as its pre-flight, so it owes the same
# diagnostic rather than a bare refusal.
check_rc=0
check_out="$(bash "$ASSEMBLER" stale-refusal 1.3.5 --check 2>&1)" || check_rc=$?
if [ "$check_rc" -eq 1 ] && grep -qF -- '--merge' <<<"$check_out"; then
  echo "PASS: stale-refusal --check -> exit 1 and points at --merge"
else
  echo "FAIL: stale-refusal --check -> exit $check_rc without the --merge remedy" >&2
  printf '%s\n' "$check_out" | sed 's/^/       /' >&2
  fail=1
fi

# ---------------------------------------------------------------------------
# 10. `--merge` folds the pending fragments INTO the existing section. The whole
#     file is diffed against the expected result: appending to `### Fixed` must
#     not duplicate that heading, `### Breaking` must land ahead of `### Added`,
#     `### Security` at the section end, and the already-released [1.3.4]
#     section below must come through byte-identical.
# ---------------------------------------------------------------------------
stale_dir="$(new_stale_crate stale-merge)"
seed_stale_fragments "$stale_dir"

merge_rc=0
merge_out="$(bash "$ASSEMBLER" stale-merge 1.3.5 --merge 2>&1)" || merge_rc=$?
if [ "$merge_rc" -ne 0 ]; then
  echo "FAIL: stale-merge -> exit $merge_rc (expected 0)" >&2
  printf '%s\n' "$merge_out" | sed 's/^/       /' >&2
  fail=1
else
  echo "PASS: stale-merge -> exit 0"
fi

cat >"$TMP_ROOT/expected-merge.md" <<'EOF'
# Changelog

---

## [1.3.5] — 2026-08-04

### Breaking

- a category that sorts ahead of every heading the section has (#4902)

### Added

- an entry consumed by the cut that never published (#4824)

### Fixed

- another entry consumed by that cut (#4824)
- a bullet that accumulated AFTER the stale section was written (#4900)

### Security

- a category the stale section does not carry at all (#4901)

## [1.3.4] — 2026-08-03

### Breaking

- a heading that belongs to a DIFFERENT section; the scan for "which headings
  does the target section already have" must not see it, or `### Breaking`
  would be deferred to the end of [1.3.5] instead of leading it

### Fixed

- older, already released and tagged
EOF

if diff -u "$TMP_ROOT/expected-merge.md" "$stale_dir/CHANGELOG.md" >"$TMP_ROOT/merge.diff" 2>&1; then
  echo "PASS: stale-merge produces the expected CHANGELOG.md byte-for-byte"
else
  echo "FAIL: stale-merge CHANGELOG.md differs from expected" >&2
  sed 's/^/       /' "$TMP_ROOT/merge.diff" >&2
  fail=1
fi

# The fragments must be consumed in the SAME operation, exactly as the fresh
# insert path does — a merge that leaves them pending would strand them again.
if [ "$(find "$stale_dir/changelog.d" -maxdepth 1 -name '*.md' | wc -l | tr -d ' ')" = "0" ]; then
  echo "PASS: stale-merge consumed all 3 fragments"
else
  echo "FAIL: stale-merge left fragments in changelog.d/" >&2
  find "$stale_dir/changelog.d" -maxdepth 1 -name '*.md' | sed 's/^/       /' >&2
  fail=1
fi

# And a second --merge is a clean no-op rather than a duplicate write.
rerun_rc=0
rerun_out="$(bash "$ASSEMBLER" stale-merge 1.3.5 --merge 2>&1)" || rerun_rc=$?
if [ "$rerun_rc" -eq 0 ] && grep -qF 'no changelog fragments pending' <<<"$rerun_out" \
  && diff -q "$TMP_ROOT/expected-merge.md" "$stale_dir/CHANGELOG.md" >/dev/null; then
  echo "PASS: a second --merge is a no-op"
else
  echo "FAIL: a second --merge was not a no-op (exit $rerun_rc)" >&2
  printf '%s\n' "$rerun_out" | sed 's/^/       /' >&2
  fail=1
fi

# ---------------------------------------------------------------------------
# 10b. The target section as the LAST section in the file — a crate's first
#      release. The merger reaches the end of the section through END, not
#      through a following `## ` heading.
# ---------------------------------------------------------------------------
eof_dir="$(new_stale_eof_crate stale-merge-eof)"
cat >"$eof_dir/changelog.d/4903-eof-fixed.md" <<'EOF'
Fixed

- a bullet merged into the last section in the file (#4903)
EOF

eof_rc=0
eof_out="$(bash "$ASSEMBLER" stale-merge-eof 1.3.5 --merge 2>&1)" || eof_rc=$?
cat >"$TMP_ROOT/expected-eof.md" <<'EOF'
# Changelog

---

## [1.3.5] — 2026-08-04

### Added

- an entry consumed by the cut that never published (#4824)

### Fixed

- a bullet merged into the last section in the file (#4903)
EOF
# The diff runs unconditionally so the failure branch always has a file to
# print — short-circuiting it behind $eof_rc left `sed` reading a missing file
# and, under `set -e`, aborted the whole selftest before its summary line.
diff -u "$TMP_ROOT/expected-eof.md" "$eof_dir/CHANGELOG.md" >"$TMP_ROOT/eof.diff" 2>&1 || true
if [ "$eof_rc" -eq 0 ] && [ ! -s "$TMP_ROOT/eof.diff" ]; then
  echo "PASS: --merge into the last section in the file"
else
  echo "FAIL: --merge into the last section in the file (exit $eof_rc)" >&2
  printf '%s\n' "$eof_out" | sed 's/^/       /' >&2
  sed 's/^/       /' "$TMP_ROOT/eof.diff" >&2
  fail=1
fi

# ---------------------------------------------------------------------------
# 11. `--merge` against a version that has NO section is refused, never
#     downgraded to a fresh insert. Silently doing something other than what was
#     asked is the same fail-open shape this issue is about.
# ---------------------------------------------------------------------------
d="$(new_crate merge-without-section)"
cat >"$d/1250-nothing-to-merge.md" <<'EOF'
Added

- a bullet with no existing section to merge into (#1250)
EOF
noseg_rc=0
noseg_out="$(bash "$ASSEMBLER" merge-without-section 9.9.9 --merge 2>&1)" || noseg_rc=$?
if [ "$noseg_rc" -eq 1 ] && grep -qF "has no '## [9.9.9]' section to merge into" <<<"$noseg_out"; then
  echo "PASS: --merge without a target section -> exit 1"
else
  echo "FAIL: --merge without a target section -> exit $noseg_rc" >&2
  printf '%s\n' "$noseg_out" | sed 's/^/       /' >&2
  fail=1
fi
if [ -f "$d/1250-nothing-to-merge.md" ]; then
  echo "PASS: --merge without a target section left the fragment in place"
else
  echo "FAIL: --merge without a target section deleted the fragment" >&2
  fail=1
fi

# ---------------------------------------------------------------------------
# 12. The fresh-insert path is unchanged by the #5298 refactor: a crate with no
#     matching section still gets one written below the `---` separator, and its
#     fragments consumed.
# ---------------------------------------------------------------------------
d="$(new_crate fresh-insert)"
cat >"$d/1251-fresh.md" <<'EOF'
Added

- a bullet for a version that has no section yet (#1251)
EOF
fresh_rc=0
fresh_out="$(bash "$ASSEMBLER" fresh-insert 2.0.0 2>&1)" || fresh_rc=$?
fresh_log="$TMP_ROOT/crates/fresh-insert/CHANGELOG.md"
if [ "$fresh_rc" -eq 0 ] \
  && grep -qE '^## \[2\.0\.0\] — [0-9]{4}-[0-9]{2}-[0-9]{2}$' "$fresh_log" \
  && grep -qF -- '- a bullet for a version that has no section yet (#1251)' "$fresh_log" \
  && [ ! -f "$d/1251-fresh.md" ]; then
  echo "PASS: fresh insert still writes the section and consumes the fragment"
else
  echo "FAIL: fresh insert regressed (exit $fresh_rc)" >&2
  printf '%s\n' "$fresh_out" | sed 's/^/       /' >&2
  sed 's/^/       /' "$fresh_log" >&2
  fail=1
fi

# ===========================================================================
# 13. PLAN-FILE MARKER COLLISION (#5298 review, LOW/Promote).
#
# `--merge` hands fragment bodies to awk in a plan file whose category
# boundaries are `@@CATEGORY` + a tab. A body line beginning with that sequence
# is read as a boundary, so the bullets below it are filed under the smuggled
# category — and --merge then deletes the fragment, the only other copy.
#
# The placed-vs-expected set check in merge_fragments() cannot see this when the
# smuggled name is a category that is ALSO independently pending: the two sets
# match either way. Case 13c replays exactly that shape, which on the pre-guard
# branch head merged, misfiled a bullet, and exited 0 having deleted both
# fragments.
# ===========================================================================

# Literal tabs via printf, not a heredoc — an invisible tab in a fixture is the
# kind of thing an editor silently converts and nobody notices.
d="$(new_crate plan-marker-collision)"
printf 'Fixed\n\n- a real Fixed bullet (#1243)\n@@CATEGORY\tRemoved\n- authored as Fixed prose, below that line\n' \
  >"$d/1243-plan-marker.md"
assert_case plan-marker-collision 1 'collides with the --merge plan-file'
# The diagnostic must name the line AND the smuggled category, with the tab
# rendered visibly — tab-versus-space is the entire difference between the
# marker and ordinary prose.
assert_case plan-marker-collision 1 '1243-plan-marker.md:4: @@CATEGORY<TAB>Removed'

# A fenced marker collides exactly as hard — the plan reader does not track
# fence state — so unlike stray_category_lines() this guard does NOT exempt
# fences. Pinned because the two guards sit next to each other and read alike.
d="$(new_crate plan-marker-in-fence)"
# `~~~` rather than a backtick fence: both are fences to the validator, and this
# one does not read as a command substitution to shellcheck (SC2016).
printf 'Documentation\n\n- documents the plan format (#1244)\n\n~~~\n@@CATEGORY\tRemoved\n~~~\n' \
  >"$d/1244-fenced-marker.md"
assert_case plan-marker-in-fence 1 'collides with the --merge plan-file'

# FALSE-POSITIVE GUARD. Only the token at column 0 followed by a TAB is the
# marker. Prose mentioning it, an indented occurrence, and the token followed by
# a space are all legitimate content.
d="$(new_crate plan-marker-false-positive)"
printf 'Fixed\n\n- prose mentioning @@CATEGORY in passing (#1245)\n  @@CATEGORY\tindented, so not at column 0\n@@CATEGORY followed by a space is not the marker\n' \
  >"$d/1245-not-the-marker.md"
assert_case plan-marker-false-positive 0 '^### Fixed$'
assert_case plan-marker-false-positive 0 'followed by a space is not the marker'

# 13c. END-TO-END: the shape that slips past the set-equality guard. Two
#      fragments, and the smuggled category is the other one's, so
#      placed == expected. Must be refused before any file is touched.
marker_dir="$(new_stale_crate plan-marker-merge)"
printf 'Fixed\n\n- a real Fixed bullet (#4910)\n@@CATEGORY\tRemoved\n- authored as Fixed prose, below that line\n' \
  >"$marker_dir/changelog.d/4910-marker.md"
printf 'Removed\n\n- a genuinely Removed bullet (#4911)\n' \
  >"$marker_dir/changelog.d/4911-removed.md"

cp "$marker_dir/CHANGELOG.md" "$TMP_ROOT/plan-marker-before.md"
mk_rc=0
mk_out="$(bash "$ASSEMBLER" plan-marker-merge 1.3.5 --merge 2>&1)" || mk_rc=$?
if [ "$mk_rc" -eq 1 ] && grep -qF 'collides with the --merge plan-file' <<<"$mk_out"; then
  echo "PASS: plan-marker --merge -> exit 1 before any write"
else
  echo "FAIL: plan-marker --merge -> exit $mk_rc (expected 1 with the marker diagnostic)" >&2
  printf '%s\n' "$mk_out" | sed 's/^/       /' >&2
  fail=1
fi
# Fail CLOSED: the file untouched and BOTH fragments still on disk. On the
# pre-guard head this merged, misfiled the bullet under `### Removed`, and
# deleted both fragments.
if diff -q "$TMP_ROOT/plan-marker-before.md" "$marker_dir/CHANGELOG.md" >/dev/null \
  && [ "$(find "$marker_dir/changelog.d" -maxdepth 1 -name '*.md' | wc -l | tr -d ' ')" = "2" ]; then
  echo "PASS: plan-marker --merge left CHANGELOG.md and both fragments untouched"
else
  echo "FAIL: plan-marker --merge mutated CHANGELOG.md or consumed a fragment" >&2
  diff -u "$TMP_ROOT/plan-marker-before.md" "$marker_dir/CHANGELOG.md" | sed 's/^/       /' >&2
  find "$marker_dir/changelog.d" -maxdepth 1 -name '*.md' | sed 's/^/       /' >&2
  fail=1
fi

# ===========================================================================
# 14. UNDELETABLE FRAGMENT — the silent-partial arm, both write paths.
#
# Both paths delete the consumed fragments AFTER CHANGELOG.md is replaced. A
# bare `rm` there aborts under `set -e` with no message: section written,
# fragments still pending, nothing said so, and the next run re-adds the
# bullets. --merge is the worse of the two — its re-run appends the survivor to
# a section that already contains it, where the write path's re-run at least
# hits the already-has-a-section refusal.
#
# Lever: chmod a-w on changelog.d/, since unlink needs write permission on the
# DIRECTORY. Root defeats that, so the cases are skipped there rather than
# reported as passing — a spurious pass is worse than an honest skip.
# ===========================================================================

if [ "$(id -u)" = "0" ]; then
  echo "SKIP: undeletable-fragment cases — running as root, chmod a-w does not"
  echo "      stop unlink, so the failure cannot be provoked deterministically."
else
  # 14a. --merge path.
  undel_dir="$(new_stale_crate undeletable-fragment-merge)"
  cat >"$undel_dir/changelog.d/4920-undeletable.md" <<'EOF'
Fixed

- a bullet whose fragment cannot be removed (#4920)
EOF
  chmod a-w "$undel_dir/changelog.d"
  undel_rc=0
  undel_out="$(bash "$ASSEMBLER" undeletable-fragment-merge 1.3.5 --merge 2>&1)" || undel_rc=$?
  chmod u+w "$undel_dir/changelog.d"

  if [ "$undel_rc" -eq 1 ]; then
    echo "PASS: undeletable-fragment-merge -> exit 1"
  else
    echo "FAIL: undeletable-fragment-merge -> exit $undel_rc (expected 1)" >&2
    printf '%s\n' "$undel_out" | sed 's/^/       /' >&2
    fail=1
  fi
  # The diagnostic owes three things: that CHANGELOG.md is already updated, the
  # name of each survivor, and "do not re-run".
  for want in 'WAS ALREADY UPDATED' 'changelog.d/4920-undeletable.md' 'Do NOT re-run'; do
    if grep -qF -- "$want" <<<"$undel_out"; then
      echo "PASS: undeletable-fragment-merge diagnostic states $want"
    else
      echo "FAIL: undeletable-fragment-merge diagnostic omits $want" >&2
      printf '%s\n' "$undel_out" | sed 's/^/       /' >&2
      fail=1
    fi
  done
  # The success message is keyed to the DELETION, not to the mv — the whole
  # point is that a half-applied release never reads as a completed one.
  if grep -qF 'Merged 1 fragment' <<<"$undel_out"; then
    echo "FAIL: undeletable-fragment-merge printed the success message anyway" >&2
    fail=1
  else
    echo "PASS: undeletable-fragment-merge suppressed the success message"
  fi
  # CHANGELOG.md really was written — this is a partial, not a rollback, and the
  # message has to be true when it says so.
  if grep -qF -- '- a bullet whose fragment cannot be removed (#4920)' "$undel_dir/CHANGELOG.md"; then
    echo "PASS: undeletable-fragment-merge left CHANGELOG.md updated, as reported"
  else
    echo "FAIL: undeletable-fragment-merge reported an update that did not happen" >&2
    fail=1
  fi

  # 14b. Default-write path — the same shape, inherited from origin/main.
  d="$(new_crate undeletable-fragment-write)"
  cat >"$d/4921-undeletable.md" <<'EOF'
Added

- a bullet whose fragment cannot be removed (#4921)
EOF
  chmod a-w "$d"
  wr_rc=0
  wr_out="$(bash "$ASSEMBLER" undeletable-fragment-write 3.0.0 2>&1)" || wr_rc=$?
  chmod u+w "$d"

  if [ "$wr_rc" -eq 1 ] \
    && grep -qF -- 'changelog.d/4921-undeletable.md' <<<"$wr_out" \
    && ! grep -qF 'Assembled 1 fragment' <<<"$wr_out"; then
    echo "PASS: undeletable-fragment-write -> exit 1, names the survivor, no success line"
  else
    echo "FAIL: undeletable-fragment-write -> exit $wr_rc" >&2
    printf '%s\n' "$wr_out" | sed 's/^/       /' >&2
    fail=1
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "assemble_changelog_selftest: one or more fragment-validation cases FAILED." >&2
  exit 1
fi

echo "assemble_changelog_selftest: all fragment-validation cases passed."
exit 0

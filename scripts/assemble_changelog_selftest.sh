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

if [ "$fail" -ne 0 ]; then
  echo "assemble_changelog_selftest: one or more fragment-validation cases FAILED." >&2
  exit 1
fi

echo "assemble_changelog_selftest: all fragment-validation cases passed."
exit 0

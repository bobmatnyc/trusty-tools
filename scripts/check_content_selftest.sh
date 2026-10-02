#!/usr/bin/env bash
#
# check_content_selftest.sh — the content/ gates fail closed (#8388).
#
# Why: ADR-0064's content version is only as good as the checks that refuse a
#   wrong one. Each case below is a way the manifest, the member versions, the
#   bump or the content changelog can be wrong; a gate that passes any of them
#   is not a gate.
#
# What: builds throwaway git repos holding copies of the scripts under test and
#   asserts the exit status and a message for each case.
#
#   tree (scripts/check_content.py tree):
#     tree-valid                 block and flow `metadata:` forms, an opt-in
#                                instruction, an unversioned skill reference
#     tree-sync-is-stable        `sync` rewrites the same member list
#     tree-missing-manifest      FAIL
#     tree-unparsable-manifest   FAIL
#     tree-bad-bundle-version    FAIL, version "1.0"
#     tree-unknown-manifest-key  FAIL, a misspelt [bundle] table
#     tree-bundle-below-member   FAIL — #8388's "lower the bundle version once"
#     tree-agent-unversioned     FAIL, an agent with no metadata.version
#     tree-top-level-version     FAIL, `version:` collides with agent_schema.rs
#     tree-member-unlisted       FAIL
#     tree-member-version-drift  FAIL
#     tree-schema-major-drift    FAIL, manifest vs package_content.sh
#     tree-outside-ia            FAIL, a content/output-styles/ directory
#     tree-listed-not-on-disk    FAIL, a [[member]] with no file
#     tree-symlink-in-class      FAIL, content/agents/alias.md -> engineer.md
#     tree-symlinked-class-dir   FAIL, content/agents -> elsewhere
#     tree-unterminated-frontmatter FAIL, no closing `---`
#     tree-quoted-top-level-version FAIL, `"version":` is still `version:`
#     tree-non-ascii-path        FAIL, content/agents/café.md, listed and valid
#     tree-expect-version-*      the release workflow's version must match
#   bump (scripts/check_content.py bump):
#     bump-released-unchanged    FAIL, member changed under a released version
#     bump-raised                OK
#     bump-unreleased-unchanged  OK, the version has no tag yet
#     bump-decreased             FAIL even with no member change
#     bump-no-member-change      OK
#     bump-manifest-deleted      FAIL
#     bump-no-tags               FAIL, release state cannot be established
#     bump-scan-floor            FAIL, empty diff
#     bump-missing-base          FAIL, --base names no ref
#     bump-non-ascii-path        FAIL, content/agents/café.md under a released version
#     bump-pairing-accepts-adtm  OK, pair_name_status on A/D/M/T records
#     bump-pairing-rejects-rename-status FAIL, two R records (even field count)
#   changelog (scripts/check_changelog_fragment.sh, both trees in one run):
#     changelog-content-no-fragment     FAIL
#     changelog-content-fragment-ok     OK
#     changelog-content-bad-category    FAIL, same as a crate fragment
#     changelog-crate-bad-category      FAIL, the crate arm is unchanged
#     changelog-both-ok                 OK, crate and content recorded
#     changelog-crate-ok-content-missing FAIL content, the crate arm passing
#     changelog-content-non-ascii-path  FAIL, content/agents/café.md
#     changelog-content-quoted-path     FAIL, a path git quotes even so
#     changelog-crate-non-ascii-fragment OK, crates/demo/changelog.d/8388-café.md
#     changelog-{staged,untracked}-non-ascii-path FAIL, --staged sees café.md
#     changelog-file-content-*          --file placement and validation
#   rollup (scripts/assemble-changelog.sh content <version>):
#     rollup-writes-content-changelog   section written, fragment deleted
#
# Usage:
#   bash scripts/check_content_selftest.sh
#   bash scripts/check_content_selftest.sh --scripts-dir <dir>
#   `--scripts-dir` copies the scripts under test from <dir> instead (same
#   layout as scripts/). Pointed at origin/main's copies (no check_content.py),
#   every tree, bump and rollup case FAILs, and so does every changelog case
#   except two that hold there too: changelog-crate-bad-category guards the
#   unchanged crate arm, and changelog-file-content-ok passes because main's
#   --file already accepts that path.
#
# Exit: 0 when every case holds; 1 naming each case that did not.
# Test: this IS the test; wired into .github/workflows/changelog-fragment.yml.
# Portability: bash 3.2 and bash 5; needs git and Python 3.11+.

# #7812: re-run under bash when invoked as `zsh <this script>`.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" == "--scripts-dir" ]]; then
  [[ $# -eq 2 ]] || {
    echo "usage: $0 [--scripts-dir DIR]" >&2
    exit 2
  }
  SRC="$(cd "$2" && pwd)"
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/content-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT
fail=0
REPO=""

# new_repo <name> — a git repo with the scripts under test and a valid tree.
new_repo() {
  REPO="$TMP_ROOT/$1"
  mkdir -p "$REPO/scripts/lib"
  git -C "$REPO" init -q -b main
  git -C "$REPO" config user.email selftest@example.invalid
  git -C "$REPO" config user.name "content selftest"
  local f
  for f in check_content.py package_content.sh check_changelog_fragment.sh assemble-changelog.sh; do
    [[ -f "$SRC/$f" ]] && cp "$SRC/$f" "$REPO/scripts/"
  done
  cp "$SRC/lib/source_class.sh" "$REPO/scripts/lib/"
  w content/agents/engineer.md '---' 'name: engineer' 'metadata:' '  version: "1.0.0"' '---' 'body'
  w content/agents/BASE-AGENT.md '---' 'name: base-agent' 'metadata: {version: "1.1.0"}' '---' 'base'
  w content/skills/tdd.md '---' 'name: tdd' 'metadata:' '  author: x' '  version: "0.5.0"' '---' 'skill'
  w content/skills/tdd/references/a.md '# reference, no frontmatter'
  w content/skills/plan/SKILL.md '---' 'name: plan' 'metadata: {version: "1.0.0"}' '---'
  w content/instructions/sections/identity.md '# section, unversioned'
  w content/instructions/output-style.md '---' 'name: tm' 'metadata:' '  version: "1.1.0"' '---'
  w content/changelog.d/README.md 'placeholder'
  w content/CONTENT-CHANGELOG.md '# Content Changelog' '' '---'
  manifest 1.1.0 1
  w crates/demo/Cargo.toml '[package]' 'name = "demo"'
  w crates/demo/src/lib.rs 'pub fn a() {}'
  w crates/demo/CHANGELOG.md '# Changelog' '' '---'
  w crates/demo/changelog.d/README.md 'placeholder'
  w docs/a.md 'doc'
  git -C "$REPO" add -A
  git -C "$REPO" commit -q -m base
}

# w <path> <line>... — write a file under $REPO, one argument per line.
w() {
  local path="$REPO/$1"
  shift
  mkdir -p "$(dirname "$path")"
  printf '%s\n' "$@" >"$path"
}

# manifest <bundle-version> <schema-major> — the valid member list.
manifest() {
  w content/manifest.toml '[bundle]' "version = \"$1\"" "schema_major = $2" '' \
    '[[member]]' 'path = "agents/BASE-AGENT.md"' 'version = "1.1.0"' '' \
    '[[member]]' 'path = "agents/engineer.md"' 'version = "1.0.0"' '' \
    '[[member]]' 'path = "instructions/output-style.md"' 'version = "1.1.0"' '' \
    '[[member]]' 'path = "skills/plan/SKILL.md"' 'version = "1.0.0"' '' \
    '[[member]]' 'path = "skills/tdd.md"' 'version = "0.5.0"'
}

# expect <case> <want-rc> <want-substrings> <command...> — run in $REPO.
# <want-substrings> holds one required substring per line.
expect() {
  local name="$1" want_rc="$2" want="$3" out rc=0 line miss=0
  shift 3
  out="$(cd "$REPO" && "$@" 2>&1)" || rc=$?
  while IFS= read -r line; do
    [[ "$out" == *"$line"* ]] || miss=1
  done <<<"$want"
  if [[ "$rc" -ne "$want_rc" ]] || [[ "$miss" -ne 0 ]]; then
    echo "FAIL $name: want exit $want_rc and '$want', got exit $rc:" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  else
    echo "ok   $name"
  fi
}

tree() { expect "$1" "$2" "$3" python3 scripts/check_content.py tree "${@:4}"; }

# --- tree -------------------------------------------------------------------
new_repo tree
tree tree-valid 0 "5 member(s) — OK"
expect tree-sync-is-stable 0 "wrote 5 member(s)" python3 scripts/check_content.py sync
tree tree-after-sync 0 "5 member(s) — OK"
tree tree-expect-version-match 0 "OK" --expect-version 1.1.0
tree tree-expect-version-mismatch 1 "but the release is 1.2.0" --expect-version 1.2.0

new_repo t-missing && rm "$REPO/content/manifest.toml"
tree tree-missing-manifest 1 "content/manifest.toml is missing"
new_repo t-unparsable && w content/manifest.toml '[bundle' 'version = "1.1.0"'
tree tree-unparsable-manifest 1 "not valid TOML"
new_repo t-badver && manifest 1.0 1
tree tree-bad-bundle-version 1 "must be SemVer"
new_repo t-key && w content/manifest.toml '[bundel]' 'version = "1.1.0"'
tree tree-unknown-manifest-key 1 "unknown top-level key(s): bundel"
new_repo t-below && manifest 1.0.9 1
tree tree-bundle-below-member 1 "bundle version 1.0.9 is lower than member agents/BASE-AGENT.md at 1.1.0"
new_repo t-unversioned && w content/agents/qa.md '---' 'name: qa' '---'
tree tree-agent-unversioned 1 "content/agents/qa.md: missing \`metadata"
new_repo t-top && w content/agents/engineer.md '---' 'name: engineer' 'version: "1.0.0"' 'metadata: {version: "1.0.0"}' '---'
tree tree-top-level-version 1 "top-level \`version:\` is not used"
new_repo t-unlisted && w content/skills/new.md '---' 'metadata: {version: "0.1.0"}' '---'
tree tree-member-unlisted 1 "content/skills/new.md: version 0.1.0 is not listed"
new_repo t-drift && w content/agents/engineer.md '---' 'metadata: {version: "1.0.1"}' '---'
tree tree-member-version-drift 1 "lists agents/engineer.md at 1.0.0, the file declares 1.0.1"
new_repo t-schema && manifest 1.1.0 2
tree tree-schema-major-drift 1 "schema_major 2 differs from scripts/package_content.sh"
new_repo t-ia && w content/output-styles/tm.md 'x'
tree tree-outside-ia 1 "content/output-styles: outside the content IA"
new_repo t-listed-absent && rm "$REPO/content/skills/tdd.md"
tree tree-listed-not-on-disk 1 "lists skills/tdd.md, which is not a versioned file under content/"
new_repo t-symlink && ln -s engineer.md "$REPO/content/agents/alias.md"
tree tree-symlink-in-class 1 "content/agents/alias.md: symlink or non-regular file"
new_repo t-symdir && mv "$REPO/content/agents" "$REPO/agents-real" && ln -s ../agents-real "$REPO/content/agents"
tree tree-symlinked-class-dir 1 "content/agents: symlink or non-directory"
new_repo t-unterminated && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' 'body'
tree tree-unterminated-frontmatter 1 "content/agents/engineer.md: unterminated frontmatter"
new_repo t-quoted-key && w content/agents/engineer.md '---' '"version": "1.0.0"' 'metadata: {version: "1.0.0"}' '---'
tree tree-quoted-top-level-version 1 "content/agents/engineer.md: top-level \`version:\` is not used"
# Listed in the manifest with a valid version, so the name is the only fault.
new_repo t-cafe && w content/agents/café.md '---' 'metadata: {version: "1.0.0"}' '---' &&
  w content/manifest.toml "$(cat "$REPO/content/manifest.toml")" '' '[[member]]' 'path = "agents/café.md"' 'version = "1.0.0"'
tree tree-non-ascii-path 1 "path must match [A-Za-z0-9._/-]+"

# --- bump -------------------------------------------------------------------
# bump_repo <name> — main at bundle 1.1.0 tagged content-v1.1.0, then a branch.
bump_repo() {
  new_repo "$1"
  git -C "$REPO" tag content-v1.1.0
  git -C "$REPO" checkout -q -b pr
}
commit() { git -C "$REPO" add -A && git -C "$REPO" commit -q -m "$1"; }
bump() { expect "$1" "$2" "$3" python3 scripts/check_content.py bump --base main; }

bump_repo b-released && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && commit edit
bump bump-released-unchanged 1 "already released as content-v1.1.0"
bump_repo b-raised && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && manifest 1.2.0 1 && commit edit
bump bump-raised 0 "bundle 1.2.0 (was 1.1.0) is unreleased — OK"
bump_repo b-unreleased && git -C "$REPO" checkout -q main && manifest 1.2.0 1 && commit next &&
  git -C "$REPO" checkout -q pr && git -C "$REPO" reset -q --hard main &&
  w content/skills/tdd/references/a.md 'edited' && commit edit
bump bump-unreleased-unchanged 0 "bundle 1.2.0 (was 1.2.0) is unreleased — OK"
bump_repo b-down && manifest 1.0.0 1 && commit down
bump bump-decreased 1 "bundle version went down, 1.1.0 -> 1.0.0"
bump_repo b-docs && w docs/a.md 'edit' && commit docs
bump bump-no-member-change 0 "no content member changed"
bump_repo b-deleted && git -C "$REPO" rm -q content/manifest.toml && commit rm
bump bump-manifest-deleted 1 "missing at HEAD"
bump_repo b-notags && git -C "$REPO" tag -d content-v1.1.0 >/dev/null && manifest 1.2.0 1 &&
  w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && commit edit
bump bump-no-tags 1 "no content-v* tag in this checkout"
bump_repo b-floor
bump bump-scan-floor 1 "SCAN FLOOR"
bump_repo b-nobase && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && commit edit
expect bump-missing-base 1 "cannot find a merge base between 'nope' and HEAD" \
  python3 scripts/check_content.py bump --base nope
# A path git would quote, added under the released version with no bump.
bump_repo b-cafe && w content/agents/café.md '---' 'metadata: {version: "1.0.0"}' '---' && commit cafe
bump bump-non-ascii-path 1 "e.g. content/agents/café.md, but bundle version 1.1.0"

# --no-renames never emits R or C, so the pairing function gets crafted -z output.
pairing() {
  expect "$1" "$2" "$3" python3 -c '
import sys
sys.path.insert(0, "scripts")
import check_content as c
try:
    print(c.pair_name_status(sys.argv[1].replace("@", chr(0))))
except c.Fail as err:
    print(f"FAIL {err}")
    sys.exit(1)
' "$4"
}
pairing bump-pairing-accepts-adtm 0 "[('A', 'a.md'), ('D', 'b.md'), ('M', 'c.md'), ('T', 'd.md')]" 'A@a.md@D@b.md@M@c.md@T@d.md@'
pairing bump-pairing-rejects-rename-status 1 "unexpected status 'R100' for path 'a.md'" 'R100@a.md@b.md@R100@c.md@d.md@'

# --- changelog --------------------------------------------------------------
gate() { expect "$1" "$2" "$3" bash scripts/check_changelog_fragment.sh --base main "${@:4}"; }

bump_repo c-none && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && commit edit
gate changelog-content-no-fragment 1 "FAIL content: 1 path(s) under content/"
w content/changelog.d/8388-edit.md 'Changed' '' '- engineer agent edited' && commit frag
gate changelog-content-fragment-ok 0 "OK   content: content/changelog.d fragment present and valid"
bump_repo c-badcat && w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' &&
  w content/changelog.d/8388-edit.md 'Fixd' '' '- engineer agent edited' && commit edit
gate changelog-content-bad-category 1 "content/changelog.d fragment present but the release assembler rejects it"
bump_repo c-crate && w crates/demo/src/lib.rs 'pub fn b() {}' &&
  w crates/demo/changelog.d/8388-x.md 'Fixd' '' '- demo edited' && commit edit
gate changelog-crate-bad-category 1 "FAIL demo: changelog.d fragment present but the release assembler rejects it"
bump_repo c-both && w crates/demo/src/lib.rs 'pub fn b() {}' && w crates/demo/changelog.d/8388-x.md 'Fixed' '' '- demo' &&
  w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' &&
  w content/changelog.d/8388-edit.md 'Changed' '' '- engineer agent edited' && commit edit
gate changelog-both-ok 0 "all 1 crate(s) with source changes are recorded
OK   content: content/changelog.d fragment present and valid"
expect changelog-file-content-ok 0 "valid changelog fragment" \
  bash scripts/check_changelog_fragment.sh --file content/changelog.d/8388-edit.md
w content/changelog.d/sub/8388-x.md 'Fixed' '' '- nested'
expect changelog-file-content-nested 1 "not a fragment path" \
  bash scripts/check_changelog_fragment.sh --file content/changelog.d/sub/8388-x.md
bump_repo c-crate-only && w crates/demo/src/lib.rs 'pub fn b() {}' && w crates/demo/changelog.d/8388-x.md 'Fixed' '' '- demo' &&
  w content/agents/engineer.md '---' 'metadata: {version: "1.0.0"}' '---' 'edit' && commit edit
gate changelog-crate-ok-content-missing 1 "OK   demo: changelog.d fragment present and valid
FAIL content: 1 path(s) under content/"
bump_repo c-cafe && w content/agents/café.md '---' 'metadata: {version: "1.0.0"}' '---' && commit cafe
gate changelog-content-non-ascii-path 1 "e.g. content/agents/café.md."
bump_repo c-quoted && w 'content/agents/a"b.md' '---' 'metadata: {version: "1.0.0"}' '---' && commit quoted
gate changelog-content-quoted-path 1 "FAIL content: 1 path(s) under content/"
# PRESENT reads unquoted paths too: a non-ASCII fragment name is still evidence.
bump_repo c-frag-cafe && w crates/demo/src/lib.rs 'pub fn b() {}' &&
  w crates/demo/changelog.d/8388-café.md 'Fixed' '' '- demo' && commit edit
gate changelog-crate-non-ascii-fragment 0 "OK   demo: changelog.d fragment present and valid"
bump_repo c-staged && w content/agents/café.md '---' 'metadata: {version: "1.0.0"}' '---' &&
  git -C "$REPO" add content/agents/café.md
gate changelog-staged-non-ascii-path 1 "e.g. content/agents/café.md." --staged
bump_repo c-untracked && w content/agents/café.md '---' 'metadata: {version: "1.0.0"}' '---'
gate changelog-untracked-non-ascii-path 1 "e.g. content/agents/café.md." --staged

# --- rollup -----------------------------------------------------------------
new_repo rollup && w content/changelog.d/8388-edit.md 'Changed' '' '- engineer agent edited'
expect rollup-writes-content-changelog 0 "into content/CONTENT-CHANGELOG.md as [1.2.0]" \
  bash scripts/assemble-changelog.sh content 1.2.0
if ! grep -q '^## \[1\.2\.0\]' "$REPO/content/CONTENT-CHANGELOG.md" ||
  ! grep -q '^- engineer agent edited$' "$REPO/content/CONTENT-CHANGELOG.md" ||
  [[ -e "$REPO/content/changelog.d/8388-edit.md" ]] || [[ ! -e "$REPO/content/changelog.d/README.md" ]]; then
  echo "FAIL rollup-content-changelog-state: section, bullet, consumed fragment or kept README wrong" >&2
  fail=1
else
  echo "ok   rollup-content-changelog-state"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "check_content_selftest: FAILED" >&2
  exit 1
fi
echo "check_content_selftest: all cases hold"

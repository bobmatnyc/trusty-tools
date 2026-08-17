#!/usr/bin/env bash
#
# check_changelog_attribution_selftest.sh — crate-attribution regressions for
# scripts/check_changelog_fragment.sh (issue #4576).
#
# Why: the gate selected source paths with the bash `case` glob
#   `crates/*/src/*`, whose `*` spans `/`, then extracted the crate with
#   `sed -E 's#^crates/([^/]+)/src/.*#\1#'`, which spans exactly ONE directory
#   level. A nested crate source path — `crates/trusty-audit/ui/src-tauri/src/`
#   is the live instance, `crates/trusty-agents/ui/src/` the original one — was
#   therefore selected and then discarded by the `[[ "$crate" == */* ]]` guard
#   that followed. Nothing was raised: the path never entered the changed set,
#   the gate concluded "no crate source changed" and reported SUCCESS. PR #5796
#   added a whole nested source tree and the gate said exactly that while
#   passing. That is the #5620 shape — a green that means "examined nothing" is
#   indistinguishable from one that means "checked and clean".
#
#   The attribution had no test, so nothing stopped the drop from being
#   reintroduced by the next edit. This is that test.
#
# What: builds a throwaway git repo carrying every path shape the gate has to
#   classify, runs the gate over one branch per shape, and asserts the verdict.
#
#   Cases:
#     nested-tauri-src-fails      crates/demo/ui/src-tauri/src/** changed with
#                                 no fragment FAILS naming crate `demo`. This is
#                                 the case that reports SUCCESS against the
#                                 pre-#4576 script.
#     nested-ui-src-fails         crates/demo/ui/src/** (Svelte/TS) likewise —
#                                 the shape #4576 was originally filed for.
#     nested-source-recorded      the same nested change WITH a valid fragment
#                                 in the shipping crate's changelog.d/ passes.
#                                 Attribution rolls the nested member up to
#                                 crates/demo/, the only directory that owns a
#                                 changelog.d/ the assembler can read.
#     depth-1-src-unchanged       the ordinary crates/demo/src/** case still
#                                 fails without a fragment. The fix must not
#                                 weaken the gate it is widening.
#     unattributed-source-fails   a source-shaped path under crates/ with no
#                                 Cargo.toml in any ancestor at either rev FAILS
#                                 naming UNATTRIBUTED SOURCE. Pre-fix it was
#                                 dropped and the gate printed OK.
#     non-source-shapes-exempt    build.rs, tests/, benches/, testdata/, a
#                                 generated ui/dist bundle, Cargo.toml and a
#                                 README all change at once and the gate still
#                                 passes with no fragment. This is the SCOPE
#                                 guard: the fix must not start demanding
#                                 fragments for path classes that never needed
#                                 one.
#     dissolved-crate-exempt      deleting a crate outright stays exempt (#3732)
#                                 and must NOT be reported as unattributable —
#                                 its manifest is gone at HEAD, so attribution
#                                 falls back to the merge base to find it.
#
# Usage:
#   bash scripts/check_changelog_attribution_selftest.sh
#   bash scripts/check_changelog_attribution_selftest.sh --gate /path/to/gate.sh
#
#   `--gate` runs the cases against an ALTERNATE copy of the gate. Pointing it
#   at the pre-#4576 script is how the mutation is demonstrated: that run must
#   FAIL here, proving these cases are not merely passing by construction.
#
# Exit: 0 when every case holds; 1 (naming the case) when one does not.
#
# Test: this IS the test. It is wired into .github/workflows/changelog-fragment.yml
#   ahead of the real gate run.
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

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/changelog-attribution-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

REPO="$TMP_ROOT/repo"
GATE="scripts/check_changelog_fragment.sh"
fail=0

g() { git -C "$REPO" "$@"; }

# ---------------------------------------------------------------------------
# Fixture. One shipping crate (`demo`) with:
#   - ordinary depth-1 source            crates/demo/src/lib.rs
#   - a NESTED workspace member          crates/demo/ui/src-tauri/{Cargo.toml,src/main.rs}
#   - nested non-Rust UI source          crates/demo/ui/src/App.svelte
#   - every non-source shape the gate must keep ignoring
# plus a second crate (`doomed`) that a later case dissolves.
# ---------------------------------------------------------------------------
mkdir -p "$REPO/scripts"
cp "$GATE_SOURCE" "$REPO/scripts/check_changelog_fragment.sh"
cp "$SCRIPT_DIR/assemble-changelog.sh" "$REPO/scripts/"

new_crate() {
  local name="$1" dir="$REPO/crates/$1"
  mkdir -p "$dir/src" "$dir/changelog.d"
  printf 'pub fn v() -> u32 { 1 }\n' >"$dir/src/lib.rs"
  printf '[package]\nname = "%s"\nversion = "0.1.0"\n' "$name" >"$dir/Cargo.toml"
  printf '# Changelog\n\n---\n' >"$dir/CHANGELOG.md"
  printf 'Placeholder keeping changelog.d/ tracked between releases.\n' \
    >"$dir/changelog.d/README.md"
}

fragment() {
  # crate, filename, category
  printf '%s\n\n- a user-visible change in %s\n' "$3" "$1" \
    >"$REPO/crates/$1/changelog.d/$2"
}

g init -q -b main
g config user.email selftest@example.invalid
g config user.name "changelog attribution self-test"

new_crate demo
new_crate doomed

# The nested workspace member, modelled on crates/trusty-audit/ui/src-tauri:
# its own Cargo.toml, its own src/, and NO changelog.d/ of its own.
mkdir -p "$REPO/crates/demo/ui/src-tauri/src" "$REPO/crates/demo/ui/src/lib"
printf '[package]\nname = "demo-ui"\nversion = "0.1.0"\n' \
  >"$REPO/crates/demo/ui/src-tauri/Cargo.toml"
printf 'fn main() {}\n' >"$REPO/crates/demo/ui/src-tauri/src/main.rs"
printf 'fn build() {}\n' >"$REPO/crates/demo/ui/src-tauri/build.rs"
printf '<script lang="ts">let n = 1;</script>\n' \
  >"$REPO/crates/demo/ui/src/App.svelte"

# Non-source shapes that have never required a fragment.
mkdir -p "$REPO/crates/demo/tests" "$REPO/crates/demo/benches" \
  "$REPO/crates/demo/src/testdata" "$REPO/crates/demo/ui/dist/assets" "$REPO/docs"
printf 'fn main() {}\n' >"$REPO/crates/demo/build.rs"
printf '#[test]\nfn t() {}\n' >"$REPO/crates/demo/tests/it.rs"
printf 'fn bench() {}\n' >"$REPO/crates/demo/benches/b.rs"
printf 'golden\n' >"$REPO/crates/demo/src/testdata/golden.txt"
printf 'console.log(1)\n' >"$REPO/crates/demo/ui/dist/assets/index-aaaa.js"
printf '# Docs\n' >"$REPO/docs/notes.md"

g add -A
g commit -qm "M0: demo (with a nested member) and doomed"
BASE="$(g rev-parse HEAD)"

# ---------------------------------------------------------------------------
# assert_case: name, expected exit status, ERE the output must match, ERE the
# output must NOT match ("" to skip either match).
# ---------------------------------------------------------------------------
assert_case() {
  local name="$1" want_rc="$2" want_re="$3" deny_re="$4" out rc=0
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
  if [ -n "$deny_re" ] && grep -qE "$deny_re" <<<"$out"; then
    echo "FAIL: $name -> exit $rc as expected, but output MATCHES forbidden /$deny_re/" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  echo "PASS: $name -> exit $rc${want_re:+, matched /$want_re/}"
}

# start_case: a fresh branch off the fixture base, so each case is independent.
start_case() { g checkout -q -B "$1" "$BASE"; }

# ---------------------------------------------------------------------------
# 1. THE #4576 DEFECT — the live instance. Only the nested member's Rust source
#    changes. Pre-fix the path is selected, then dropped, and the gate prints
#    "no crate source changed (docs-only / CI-only / test-only) — OK."
# ---------------------------------------------------------------------------
start_case nested-tauri
printf 'fn main() { println!("changed"); }\n' \
  >"$REPO/crates/demo/ui/src-tauri/src/main.rs"
g add -A
g commit -qm "change nested src-tauri source, no fragment"
assert_case nested-tauri-src-fails 1 \
  'FAIL demo: crates/demo/src/\*\* changed with no changelog record' \
  'no crate source changed'

# ---------------------------------------------------------------------------
# 2. THE #4576 DEFECT — the original instance. Nested Svelte/TS UI source, which
#    ships to users exactly as the Rust does.
# ---------------------------------------------------------------------------
start_case nested-ui
printf '<script lang="ts">let n = 2;</script>\n' \
  >"$REPO/crates/demo/ui/src/App.svelte"
g add -A
g commit -qm "change nested ui source, no fragment"
assert_case nested-ui-src-fails 1 \
  'FAIL demo: crates/demo/src/\*\* changed with no changelog record' \
  'no crate source changed'

# ---------------------------------------------------------------------------
# 3. THE FIX IS SATISFIABLE. The nested member owns no changelog.d/, so the
#    fragment belongs to the crate that SHIPS it — which is where attribution
#    rolls the path up to, and what `assemble-changelog.sh <crate-dir>` accepts.
# ---------------------------------------------------------------------------
start_case nested-recorded
printf 'fn main() { println!("changed"); }\n' \
  >"$REPO/crates/demo/ui/src-tauri/src/main.rs"
printf '<script lang="ts">let n = 3;</script>\n' \
  >"$REPO/crates/demo/ui/src/App.svelte"
fragment demo 4576-nested-change.md Fixed
g add -A
g commit -qm "change nested source and record it"
assert_case nested-source-recorded 0 \
  'OK   demo: changelog.d fragment present and valid' \
  'FAIL'

# ---------------------------------------------------------------------------
# 4. STILL A GATE. The ordinary depth-1 path the gate always handled.
# ---------------------------------------------------------------------------
start_case depth-1
printf 'pub fn v() -> u32 { 2 }\n' >"$REPO/crates/demo/src/lib.rs"
g add -A
g commit -qm "change depth-1 source, no fragment"
assert_case depth-1-src-unchanged 1 \
  'FAIL demo: crates/demo/src/\*\* changed with no changelog record' \
  ''

# ---------------------------------------------------------------------------
# 5. FAIL CLOSED. A source-shaped path under crates/ that belongs to no crate at
#    HEAD or at the merge base must be REPORTED. Pre-fix it was dropped in
#    silence and the gate printed OK — the fail-open this issue is about.
# ---------------------------------------------------------------------------
start_case orphan
mkdir -p "$REPO/crates/orphan/ui/src"
printf 'export const x = 1;\n' >"$REPO/crates/orphan/ui/src/x.ts"
g add -A
g commit -qm "add source under crates/ with no Cargo.toml anywhere"
assert_case unattributed-source-fails 1 \
  'UNATTRIBUTED SOURCE' \
  'no crate source changed'

# ---------------------------------------------------------------------------
# 6. SCOPE GUARD. Every path class that has never required a fragment changes at
#    once. If the attribution fix widened what counts as crate source, this goes
#    red — which is the point of asserting it.
# ---------------------------------------------------------------------------
start_case non-source
printf 'fn main() { /* changed */ }\n' >"$REPO/crates/demo/build.rs"
printf 'fn build() { /* changed */ }\n' >"$REPO/crates/demo/ui/src-tauri/build.rs"
printf '#[test]\nfn t() { assert!(true); }\n' >"$REPO/crates/demo/tests/it.rs"
printf 'fn bench() { /* changed */ }\n' >"$REPO/crates/demo/benches/b.rs"
printf 'golden v2\n' >"$REPO/crates/demo/src/testdata/golden.txt"
printf 'console.log(2)\n' >"$REPO/crates/demo/ui/dist/assets/index-aaaa.js"
printf '[package]\nname = "demo"\nversion = "0.1.1"\n' >"$REPO/crates/demo/Cargo.toml"
printf '# Docs\n\nchanged\n' >"$REPO/docs/notes.md"
g add -A
g commit -qm "change every non-source shape, no fragment"
assert_case non-source-shapes-exempt 0 \
  'no crate source changed \(docs-only / CI-only / test-only\) — OK' \
  'FAIL'

# ---------------------------------------------------------------------------
# 7. #3732 EXEMPTION INTACT. Dissolving a crate deletes every source file under
#    it AND its Cargo.toml, so attribution finds no manifest at HEAD. It must
#    fall back to the merge base and grant the exemption, not report the crate
#    as unattributable.
# ---------------------------------------------------------------------------
start_case dissolve
g rm -rq crates/doomed
g commit -qm "dissolve the doomed crate"
assert_case dissolved-crate-exempt 0 \
  'no crate source changed \(docs-only / CI-only / test-only\) — OK' \
  'UNATTRIBUTED SOURCE|FAIL'

echo
if [ "$fail" -ne 0 ]; then
  echo "check_changelog_attribution_selftest: FAILED — the gate mis-attributes" >&2
  echo "  crate source paths (issue #4576). Gate under test: ${GATE_SOURCE}" >&2
  exit 1
fi
echo "check_changelog_attribution_selftest: all crate-attribution cases passed."

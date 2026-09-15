#!/usr/bin/env bash
#
# check_gate_shells_selftest.sh — the repo-root gates give the same verdict
# under zsh as under bash (issue #7812).
#
# Why: the harness shell is zsh. `zsh scripts/check_line_cap.sh` died on
#   `BASH_SOURCE[0]: parameter not set` and `PATHS: assignment to invalid
#   subscript range` while bash ran it clean. Each gate now re-executes itself
#   under bash; this proves the re-exec reaches the real verdict, not only that
#   zsh stops erroring.
# What: builds fixture repos and runs each case as `bash <gate> <args>` and as
#   `zsh <gate> <args>`. A case passes only when both runs give the same exit
#   status and byte-identical output AND that result is the expected verdict,
#   so two identical crashes cannot pass:
#     line-cap           whole-tree run, over-cap Swift file        -> exit 1
#     line-cap-paths     path-list mode (the array zsh rejected)    -> exit 1
#     changelog-default  committed source change, no fragment      -> exit 1
#     changelog-staged   same branch plus a staged fragment         -> exit 0
#     test-pointers      --self-test, the gate's hermetic fixture   -> exit 0
#   zsh missing is a FAIL, never a skip; CI installs it (line-cap.yml).
# Test: this IS the test. Run directly: ./scripts/check_gate_shells_selftest.sh
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); needs zsh and git.

# #7812: this file is bash too, so it takes the same guard as the gates.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v zsh >/dev/null 2>&1; then
  echo "FAIL: zsh is not on PATH, so #7812 cannot be proven here. Install zsh." >&2
  exit 1
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/gate-shells.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT
fail=0

# new_repo <name>: print the path of a fresh repo holding copies of the gates
# and the libraries they source, so each gate resolves its root inside it.
new_repo() {
  local r="$TMP_ROOT/$1"
  mkdir -p "$r/scripts/lib"
  git -C "$r" init -q
  git -C "$r" config user.email selftest@example.invalid
  git -C "$r" config user.name "gate shells self-test"
  cp "$SCRIPT_DIR/check_line_cap.sh" "$SCRIPT_DIR/check_changelog_fragment.sh" \
    "$SCRIPT_DIR/assemble-changelog.sh" "$r/scripts/"
  cp "$SCRIPT_DIR/lib/sloc_awk.sh" "$SCRIPT_DIR/lib/source_class.sh" "$r/scripts/lib/"
  echo "$r"
}

# both <name> <want_rc> <want_re> <cwd> <gate> [args...]
both() {
  local name="$1" want_rc="$2" want_re="$3" cwd="$4" gate="$5" bout zout brc=0 zrc=0
  shift 5
  bout="$(cd "$cwd" && bash "$gate" "$@" 2>&1)" || brc=$?
  zout="$(cd "$cwd" && zsh "$gate" "$@" 2>&1)" || zrc=$?
  if [ "$brc" -ne "$zrc" ] || [ "$bout" != "$zout" ]; then
    echo "FAIL: $name — bash (exit $brc) and zsh (exit $zrc) disagree:" >&2
    printf -- '--- bash\n%s\n--- zsh\n%s\n' "$bout" "$zout" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  if [ "$brc" -ne "$want_rc" ] || ! grep -qE "$want_re" <<<"$bout"; then
    echo "FAIL: $name — same under both shells, but exit $brc (expected $want_rc) or no /$want_re/:" >&2
    printf '%s\n' "$bout" | sed 's/^/       /' >&2
    fail=1
    return
  fi
  echo "PASS: $name -> exit $brc under bash and zsh, identical output"
}

# ---- line cap: 520 small .rs files clear the scan floor; one Swift file is over.
lc="$(new_repo linecap)"
mkdir -p "$lc/pad" "$lc/app"
i=0
while [ "$i" -lt 520 ]; do
  printf 'pub fn pad%s() -> u32 {\n    %s\n}\n' "$i" "$i" >"$lc/pad/pad$i.rs"
  i=$((i + 1))
done
awk 'BEGIN { for (i = 0; i < 600; i++) printf "let v%d = %d\n", i, i }' >"$lc/app/Big.swift"
git -C "$lc" add -A && git -C "$lc" commit -qm fixture
both line-cap 1 'app/Big.swift is 600 SLOC' "$lc" scripts/check_line_cap.sh
both line-cap-paths 1 'app/Big.swift is 600 SLOC' "$lc" scripts/check_line_cap.sh pad/pad1.rs app/Big.swift

# ---- changelog fragment: a work branch with a committed source change.
cf="$(new_repo changelog)"
mkdir -p "$cf/crates/demo/src" "$cf/crates/demo/changelog.d"
printf 'pub fn v() -> u32 { 1 }\n' >"$cf/crates/demo/src/lib.rs"
printf '[package]\nname = "demo"\nversion = "0.1.0"\n' >"$cf/crates/demo/Cargo.toml"
printf '# Changelog\n\n---\n' >"$cf/crates/demo/CHANGELOG.md"
printf 'Placeholder.\n' >"$cf/crates/demo/changelog.d/README.md"
git -C "$cf" add -A && git -C "$cf" commit -qm base
git -C "$cf" branch -q base
git -C "$cf" checkout -q -b work
printf 'pub fn v() -> u32 { 2 }\n' >"$cf/crates/demo/src/lib.rs"
git -C "$cf" add -A && git -C "$cf" commit -qm "source change"
both changelog-default 1 'FAIL demo: crates/demo/src/\*\* changed with no changelog record' \
  "$cf" scripts/check_changelog_fragment.sh --base base
printf 'Fixed\n\n- a change\n' >"$cf/crates/demo/changelog.d/7812-shells.md"
git -C "$cf" add crates/demo/changelog.d/7812-shells.md
both changelog-staged 0 'OK   demo: changelog.d fragment present and valid' \
  "$cf" scripts/check_changelog_fragment.sh --staged --base base

# ---- test pointers: the gate's own fixture suite is hermetic.
both test-pointers 0 'check_test_pointers self-test: OK' "$TMP_ROOT" \
  "$SCRIPT_DIR/check_test_pointers.sh" --self-test

echo
if [ "$fail" -ne 0 ]; then
  echo "check_gate_shells_selftest: FAILED — a gate answers differently under zsh (#7812)." >&2
  exit 1
fi
echo "check_gate_shells_selftest: all gates give one verdict under bash and zsh."

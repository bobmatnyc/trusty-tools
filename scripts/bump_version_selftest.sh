#!/usr/bin/env bash
#
# bump_version_selftest.sh — fixtures for scripts/bump-version.sh's two
# changelog modes (issue #5674).
#
# Why: fragment consumption used to be unconditional and untested, and both
#   halves of that mattered. On PR #5673 a mid-PR bump ate four in-flight
#   `changelog.d/` fragments, the per-PR changelog gate then failed the PR for
#   lacking a fragment the script had just deleted, and the repair was manual
#   (commit 879b5fe2). The flag that fixes it is only worth having while
#   something proves BOTH directions still hold — a release cut that stops
#   assembling would ship a CHANGELOG with no section for the release, which
#   nothing else checks.
#
# What: copies the REAL bump-version.sh and assemble-changelog.sh into a
#   throwaway workspace (WORKSPACE_ROOT derives from the script's own location,
#   so a temp `<tmp>/scripts/` + `<tmp>/crates/` tree exercises main() end to
#   end with zero risk to the checkout), puts a stub `cargo` first on PATH so
#   the Cargo.lock sync is observed rather than performed, and asserts:
#
#     default            version bumped, fragments consumed, `## [0.2.0]`
#                        section written, tag commands printed
#     --no-changelog     version bumped, fragments SURVIVE, CHANGELOG.md
#                        byte-identical, no tag command printed
#     flag order         `--no-changelog` before the positionals behaves the same
#     unknown flag       refused with exit 2, manifest untouched
#     missing assembler  refused under the default, ACCEPTED under
#                        --no-changelog (that path never calls it)
#
#   Same throwaway-workspace pattern as scripts/assemble_changelog_selftest.sh.
#
# Test: this IS the test. Run directly:
#   bash scripts/bump_version_selftest.sh
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only. Same constraints as the script under test.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/bump-version-selftest.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

FAILURES=0
CASES=0

pass() {
  CASES=$((CASES + 1))
  printf '  ok   %s\n' "$1"
}

fail() {
  CASES=$((CASES + 1))
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: $1"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1: expected '$2', got '$3'"; fi
}

# assert_contains <label> <needle> <haystack>
assert_contains() {
  case "$3" in
    *"$2"*) pass "$1" ;;
    *) fail "$1: output does not contain '$2'" ;;
  esac
}

# assert_absent <label> <needle> <haystack>
assert_absent() {
  case "$3" in
    *"$2"*) fail "$1: output unexpectedly contains '$2'" ;;
    *) pass "$1" ;;
  esac
}

# A stub `cargo` so the lock sync is observable and costs nothing. The real one
# would need a workspace manifest and a registry; what this selftest is about is
# which files the script touches, not what cargo does with them.
mkdir -p "$TMP_ROOT/bin"
cat >"$TMP_ROOT/bin/cargo" <<'EOF'
#!/usr/bin/env bash
echo "stub cargo $*"
EOF
chmod +x "$TMP_ROOT/bin/cargo"

# new_workspace <name> [--no-assembler] — builds a throwaway workspace holding a
# copy of both scripts and one crate with a single fragment, and prints its root.
new_workspace() {
  local name="$1" with_assembler="${2:-with-assembler}"
  local root="$TMP_ROOT/$name"
  mkdir -p "$root/scripts" "$root/crates/demo-crate/changelog.d"
  cp "$SCRIPT_DIR/bump-version.sh" "$root/scripts/bump-version.sh"
  if [ "$with_assembler" = "with-assembler" ]; then
    cp "$SCRIPT_DIR/assemble-changelog.sh" "$root/scripts/assemble-changelog.sh"
    chmod +x "$root/scripts/assemble-changelog.sh"
  fi
  cat >"$root/crates/demo-crate/Cargo.toml" <<'EOF'
[package]
name = "demo-crate"
version = "0.1.0"
edition = "2021"
EOF
  cat >"$root/crates/demo-crate/CHANGELOG.md" <<'EOF'
# Changelog

---
EOF
  cat >"$root/crates/demo-crate/changelog.d/5674-in-flight.md" <<'EOF'
Fixed

- an in-flight bullet that a mid-PR bump must not eat
EOF
  echo "$root"
}

# run_bump <workspace-root> <args...> — runs the copied script with the stub
# cargo on PATH, then sets RC and OUT in THIS shell. Not a command substitution:
# a `$(...)` runs in a subshell, so an exit status assigned inside it is lost
# and every status assertion would read 0.
RC=0
OUT=""
run_bump() {
  local root="$1"
  shift
  RC=0
  PATH="$TMP_ROOT/bin:$PATH" bash "$root/scripts/bump-version.sh" "$@" \
    >"$TMP_ROOT/run.out" 2>&1 || RC=$?
  OUT="$(cat "$TMP_ROOT/run.out")"
}

manifest_version() {
  grep -m1 -E '^version[[:space:]]*=' "$1/crates/demo-crate/Cargo.toml" |
    sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/'
}

fragment_count() {
  find "$1/crates/demo-crate/changelog.d" -name '*.md' | grep -c . || true
}

# ---------------------------------------------------------------------------
# The release cut: unchanged behaviour, and the half that must never regress.
# ---------------------------------------------------------------------------
echo "default (release cut):"
ws="$(new_workspace default)"
run_bump "$ws" demo-crate minor
assert_eq "exits 0" "0" "$RC"
assert_eq "version bumped" "0.2.0" "$(manifest_version "$ws")"
assert_eq "fragment consumed" "0" "$(fragment_count "$ws")"
assert_contains "CHANGELOG gained the section" "## [0.2.0]" \
  "$(cat "$ws/crates/demo-crate/CHANGELOG.md")"
assert_contains "the bullet survived into the section" "an in-flight bullet" \
  "$(cat "$ws/crates/demo-crate/CHANGELOG.md")"
assert_contains "Cargo.lock sync ran" "stub cargo update -p demo-crate --precise 0.2.0" "$OUT"
assert_contains "prints the tag command" "git tag demo-crate-v0.2.0" "$OUT"

# ---------------------------------------------------------------------------
# The #5674 case: a bump riding along with its source change.
# ---------------------------------------------------------------------------
echo "--no-changelog (bump inside a source PR):"
ws="$(new_workspace noassemble)"
before="$(cat "$ws/crates/demo-crate/CHANGELOG.md")"
run_bump "$ws" demo-crate minor --no-changelog
assert_eq "exits 0" "0" "$RC"
assert_eq "version bumped" "0.2.0" "$(manifest_version "$ws")"
assert_eq "fragment survives" "1" "$(fragment_count "$ws")"
assert_eq "CHANGELOG.md byte-identical" "$before" \
  "$(cat "$ws/crates/demo-crate/CHANGELOG.md")"
assert_contains "Cargo.lock sync still ran" "stub cargo update -p demo-crate --precise 0.2.0" "$OUT"
assert_contains "says why nothing was assembled" "Skipping changelog assembly" "$OUT"
# A tag cut from an unmerged branch is the hazard this suppression exists for.
assert_absent "prints no tag command" "git tag" "$OUT"

echo "flag order:"
ws="$(new_workspace flagfirst)"
run_bump "$ws" --no-changelog demo-crate minor
assert_eq "exits 0 with the flag first" "0" "$RC"
assert_eq "fragment survives" "1" "$(fragment_count "$ws")"

echo "unknown flag:"
ws="$(new_workspace badflag)"
run_bump "$ws" demo-crate minor --no-changelogs
assert_eq "exits 2" "2" "$RC"
assert_eq "manifest untouched" "0.1.0" "$(manifest_version "$ws")"
assert_contains "names the offending option" "unknown option '--no-changelogs'" "$OUT"

# ---------------------------------------------------------------------------
# The assembler pre-flight is gated on the mode that uses it, not on nothing.
# ---------------------------------------------------------------------------
echo "missing assembler:"
ws="$(new_workspace noassembler no-assembler)"
run_bump "$ws" demo-crate minor
assert_eq "default refuses" "1" "$RC"
assert_eq "manifest untouched" "0.1.0" "$(manifest_version "$ws")"

ws="$(new_workspace noassembler2 no-assembler)"
run_bump "$ws" demo-crate minor --no-changelog
assert_eq "--no-changelog proceeds without it" "0" "$RC"
assert_eq "version bumped" "0.2.0" "$(manifest_version "$ws")"

echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo "bump_version_selftest: ${CASES} case(s), all pass"
  exit 0
fi
echo "bump_version_selftest: ${FAILURES} of ${CASES} case(s) FAILED" >&2
exit 1

#!/usr/bin/env bash
#
# check_scan_floor_selftest.sh — mutation self-test for every gate's scan floor
# (issue #4618).
#
# Why: a gate script that iterates a file set and exits 0 when that set is EMPTY
#   reports success identically to one that scanned everything and found nothing
#   wrong. A required check that cannot fail is not a check. #4618 found nine
#   gates in that state; the same shape had already been fixed one-at-a-time
#   three times before (#4097, #4307, #4614). The floors added in response are
#   themselves untested code — nothing stops a later refactor from restoring the
#   vacuous pass — so this script is the paired self-test the issue asks for,
#   generalizing the one gate that already did it right
#   (`scripts/check_token_drift.test.mjs`: "every ENFORCED crate compares >0 real
#   tokens against canonical").
#
# What: for each gate, builds a throwaway git repo whose scan set is EMPTY (or
#   whose git enumeration is shimmed to FAIL), copies the gate into it so its
#   `SCRIPT_DIR`-derived repo root resolves inside the fixture, runs it, and
#   asserts it exits NON-ZERO naming SCAN FLOOR or TOOL ERROR. Every case is a
#   mutation that used to exit 0.
#
#   Two cases are stricter than an empty set, because the tool actively FAILED
#   and the gate still passed:
#     - check_agent_assets.sh consumed `git ls-files` through a `< <(...)`
#       process substitution, which is exempt from both `set -e` and `pipefail`,
#       so a git exiting 128 arrived as an empty stream.
#     - check_changelog_fragment.sh probed with `git cat-file -e … || continue`,
#       which cannot tell "the crate was deleted" (the exemption) from "git
#       broke", so any git error granted the exemption.
#   Both are exercised here with a `git` shim that fails the relevant subcommand.
#
# Usage:
#   bash scripts/check_scan_floor_selftest.sh                     # every gate
#   bash scripts/check_scan_floor_selftest.sh line_cap            # one by name
#   bash scripts/check_scan_floor_selftest.sh a b                 # several
#   (names: line_cap generation_artifacts generation_artifacts_write_modes
#    test_pointers doc_numbers claude_md changelog_fragment
#    agent_assets_tool_error changelog_fragment_tool_error)
#
# Exit: 0 when every gate rejects its vacuous scan; 1 (naming the gate) when one
#   of them still reports success over nothing.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"

# Zero args = run every case. One or more names = run exactly those, so each
# gate's own workflow can pay only for its own case.
ONLY="$*"
PASSED=0
FAILED=0

KNOWN="line_cap generation_artifacts generation_artifacts_write_modes \
test_pointers doc_numbers claude_md changelog_fragment \
agent_assets_tool_error changelog_fragment_tool_error"

for requested in "$@"; do
  case " $KNOWN " in
    *" $requested "*) ;;
    *)
      echo "check_scan_floor_selftest: unknown gate '$requested'." >&2
      echo "  known: $KNOWN" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# new_fixture: print the path to a fresh throwaway git repo.
# ---------------------------------------------------------------------------
new_fixture() {
  local tmp
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/scanfloor.XXXXXX")"
  git -C "$tmp" init -q
  git -C "$tmp" config user.email selftest@example.invalid
  git -C "$tmp" config user.name "scan floor self-test"
  echo "$tmp"
}

# ---------------------------------------------------------------------------
# install_gate <fixture> <script-name> [extra-dir ...] — copy a gate (and any
# helper dirs it sources) into the fixture so its SCRIPT_DIR-derived REPO_ROOT
# resolves to the fixture rather than back to the real checkout.
# ---------------------------------------------------------------------------
install_gate() {
  local fixture="$1" name="$2"
  shift 2
  mkdir -p "$fixture/scripts"
  cp "$REPO_ROOT/scripts/$name" "$fixture/scripts/$name"
  local extra
  for extra in "$@"; do
    cp -R "$REPO_ROOT/scripts/$extra" "$fixture/scripts/"
  done
}

# ---------------------------------------------------------------------------
# install_failing_git_shim <fixture> <subcommand> [arg-substring] — put a `git`
# earlier on PATH that exits 128 for <subcommand> (optionally only when
# [arg-substring] appears in its arguments) and delegates everything else to the
# real git. Models a genuine tool failure, which must never read as "nothing to
# check". The substring filter lets a test target ONE probe inside a script that
# makes several, so the assertion proves the intended line failed closed.
# ---------------------------------------------------------------------------
install_failing_git_shim() {
  local fixture="$1" subcmd="$2" needle="${3:-}" real
  real="$(command -v git)"
  mkdir -p "$fixture/bin"
  cat > "$fixture/bin/git" <<EOF
#!/usr/bin/env bash
needle='$needle'
if [ "\${1:-}" = "$subcmd" ]; then
  if [ -z "\$needle" ] || case " \$* " in *"\$needle"*) true ;; *) false ;; esac; then
    echo "fatal: simulated git failure in '$subcmd' (scan-floor self-test)" >&2
    exit 128
  fi
fi
exec "$real" "\$@"
EOF
  chmod +x "$fixture/bin/git"
}

# ---------------------------------------------------------------------------
# expect_rejects <label> <fixture> <cmd...> — run the gate and assert it exits
# non-zero with a diagnosis naming the floor or the tool error.
# ---------------------------------------------------------------------------
expect_rejects() {
  local label="$1" fixture="$2"
  shift 2
  local out rc=0
  out="$(cd "$fixture" && "$@" 2>&1)" || rc=$?

  if [ "$rc" -eq 0 ]; then
    echo "SELF-TEST FAIL: ${label} exited 0 over a vacuous scan — the #4618 defect is back." >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    FAILED=$((FAILED + 1))
    rm -rf "$fixture"
    return
  fi

  case "$out" in
    *"SCAN FLOOR"* | *"TOOL ERROR"*) ;;
    *)
      echo "SELF-TEST FAIL: ${label} exited ${rc} but did not name SCAN FLOOR / TOOL ERROR;" >&2
      echo "       it may be failing for an unrelated reason, which proves nothing:" >&2
      printf '%s\n' "$out" | sed 's/^/       /' >&2
      FAILED=$((FAILED + 1))
      rm -rf "$fixture"
      return
      ;;
  esac

  echo "  ok  ${label} (exit ${rc}: $(printf '%s' "$out" | grep -m1 -E 'SCAN FLOOR|TOOL ERROR' | sed 's/^[[:space:]]*//'))"
  PASSED=$((PASSED + 1))
  rm -rf "$fixture"
}

want() {
  [ -z "$ONLY" ] && return 0
  case " $ONLY " in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

# ===========================================================================
# 1. check_line_cap.sh — zero tracked .rs files.
# ===========================================================================
if want line_cap; then
  f="$(new_fixture)"
  install_gate "$f" check_line_cap.sh lib
  echo "placeholder" > "$f/README.md"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_line_cap.sh (0 tracked .rs files)" "$f" bash scripts/check_line_cap.sh
fi

# ===========================================================================
# 2. check_generation_artifacts.sh — zero stylesheets and zero prose files.
# ===========================================================================
if want generation_artifacts; then
  f="$(new_fixture)"
  install_gate "$f" check_generation_artifacts.sh
  echo "fn main() {}" > "$f/main.rs"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_generation_artifacts.sh (0 scannable files)" "$f" \
    bash scripts/check_generation_artifacts.sh
fi

# ===========================================================================
# 2b. check_generation_artifacts.sh --seed / --update — the ALLOWLIST-WRITING
#     modes must be floored too. Seeding from an empty scan writes an empty
#     allowlist; pruning from one deletes every row as "no longer a finding".
#     Both are silent data loss dressed as a clean run, so the floor is
#     asserted before the mode dispatch.
# ===========================================================================
if want generation_artifacts_write_modes; then
  for mode in --seed --update; do
    f="$(new_fixture)"
    install_gate "$f" check_generation_artifacts.sh
    echo "fn main() {}" > "$f/main.rs"
    printf '# grandfathered\ndocs/legacy.md\n' > "$f/.generation-artifact-allowlist.tsv"
    git -C "$f" add -A && git -C "$f" commit -qm fixture
    expect_rejects "check_generation_artifacts.sh ${mode} (0 scannable files)" "$f" \
      bash scripts/check_generation_artifacts.sh "$mode"
  done
fi

# ===========================================================================
# 3. check_test_pointers.sh — zero tracked .rs files, so zero citations.
# ===========================================================================
if want test_pointers; then
  f="$(new_fixture)"
  install_gate "$f" check_test_pointers.sh
  echo "placeholder" > "$f/README.md"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_test_pointers.sh (0 citations resolved)" "$f" \
    bash scripts/check_test_pointers.sh
fi

# ===========================================================================
# 4. check_doc_numbers.sh — zero spec/ADR documents, so zero number claims.
# ===========================================================================
if want doc_numbers; then
  f="$(new_fixture)"
  install_gate "$f" check_doc_numbers.sh
  mkdir -p "$f/docs/specs"
  : > "$f/docs/specs/README.md"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_doc_numbers.sh (0 docs / 0 claims)" "$f" \
    bash scripts/check_doc_numbers.sh
fi

# ===========================================================================
# 5. check_claude_md_not_tracked.sh — an index too small for the probe to mean
#    anything (the "untracked" verdict is indistinguishable from a broken git).
# ===========================================================================
if want claude_md; then
  f="$(new_fixture)"
  install_gate "$f" check_claude_md_not_tracked.sh
  echo "placeholder" > "$f/README.md"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_claude_md_not_tracked.sh (index below floor)" "$f" \
    bash scripts/check_claude_md_not_tracked.sh
fi

# ===========================================================================
# 6. check_changelog_fragment.sh — an empty diff against the base.
# ===========================================================================
if want changelog_fragment; then
  f="$(new_fixture)"
  install_gate "$f" check_changelog_fragment.sh
  echo "placeholder" > "$f/README.md"
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  expect_rejects "check_changelog_fragment.sh (0 changed paths)" "$f" \
    bash scripts/check_changelog_fragment.sh --base HEAD
fi

# ===========================================================================
# 7. FAIL-OPEN CASE A — check_agent_assets.sh with `git ls-files` failing.
#    Pre-fix, the process substitution swallowed the 128 and the gate printed
#    "0 byte-parity file(s) match — OK".
# ===========================================================================
if want agent_assets_tool_error; then
  f="$(new_fixture)"
  install_gate "$f" check_agent_assets.sh
  mkdir -p "$f/crates/trusty-code/src/assets/agents" "$f/crates/trusty-mpm/src/assets/agents"
  for base in code-analyzer code-critic qa web-qa; do
    printf 'upstream %s\n' "$base" > "$f/crates/trusty-mpm/src/assets/agents/$base.md"
    printf 'deviated %s\n' "$base" > "$f/crates/trusty-code/src/assets/agents/$base.md"
  done
  # A parity file that genuinely DRIFTED: the gate must never report OK over it.
  printf 'source\n' > "$f/crates/trusty-mpm/src/assets/agents/research.md"
  printf 'DRIFTED\n' > "$f/crates/trusty-code/src/assets/agents/research.md"
  # The pins file must pre-exist: --force-add reads it before writing it.
  : > "$f/scripts/agent-asset-pins.tsv"
  ( cd "$f" && bash scripts/check_agent_assets.sh --force-add >/dev/null )
  git -C "$f" add -A && git -C "$f" commit -qm fixture
  install_failing_git_shim "$f" ls-files
  expect_rejects "check_agent_assets.sh (git ls-files exits 128)" "$f" \
    env "PATH=$f/bin:$PATH" bash scripts/check_agent_assets.sh
fi

# ===========================================================================
# 8. FAIL-OPEN CASE B — check_changelog_fragment.sh with the tree probe
#    failing. Pre-fix, `git cat-file -e … || continue` read the error as the
#    "this PR deleted the crate" exemption and printed OK over a real
#    unrecorded source change.
# ===========================================================================
if want changelog_fragment_tool_error; then
  f="$(new_fixture)"
  install_gate "$f" check_changelog_fragment.sh
  mkdir -p "$f/crates/demo/src" "$f/scripts"
  echo "placeholder" > "$f/README.md"
  printf '[package]\nname = "demo"\n' > "$f/crates/demo/Cargo.toml"
  echo "fn main() {}" > "$f/crates/demo/src/main.rs"
  git -C "$f" add -A && git -C "$f" commit -qm base
  # A real, unrecorded source change — the gate MUST NOT pass this.
  echo "fn changed() {}" >> "$f/crates/demo/src/main.rs"
  git -C "$f" add -A && git -C "$f" commit -qm "unrecorded source change"
  # Fail ONLY the `crates/<crate>/Cargo.toml` existence probe — the exact line
  # that used to grant the crate-deleted exemption on any git error (#4618 item
  # 3). Every other git call, including the merge-base assembler probe, works.
  install_failing_git_shim "$f" ls-tree "Cargo.toml"
  expect_rejects "check_changelog_fragment.sh (crate-existence probe exits 128)" "$f" \
    env "PATH=$f/bin:$PATH" bash scripts/check_changelog_fragment.sh --base HEAD~1
fi

# ===========================================================================
# Verdict
# ===========================================================================
if [ "$FAILED" -ne 0 ]; then
  echo "scan-floor self-test: ${PASSED} passed, ${FAILED} FAILED — see above (issue #4618)." >&2
  exit 1
fi

if [ "$PASSED" -eq 0 ]; then
  echo "scan-floor self-test: 0 cases ran — this self-test just proved nothing about itself." >&2
  echo "  (that is the very defect it exists to catch; check the gate name argument.)" >&2
  exit 1
fi

echo "scan-floor self-test: ${PASSED} gate(s) correctly refuse a vacuous scan — OK."
exit 0

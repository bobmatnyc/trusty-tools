#!/usr/bin/env bash
#
# check-changelog-assembled-selftest.sh — failing-case fixtures for
# scripts/check-changelog-assembled.sh.
#
# Why: a check whose FAILING branches were never exercised is exactly the gap
#   #6406 is about — eight `preflight-publish.sh` checks existed and none of
#   them read `changelog.d/` or `CHANGELOG.md`, so the six trusty-audit
#   releases that bypassed the assembler sailed through every one of them.
#   Asserting only "the gate ran and exited 0" against a clean fixture would
#   repeat that shape at one remove: a gate that always passes looks
#   identical to one that checks something.
#
# What: builds synthetic `crates/<dir>/` fixtures under a scratch directory and
#   runs the gate against each with `--repo`, asserting both the exit status
#   and the finding code on stdout/stderr. Case 5 runs the gate against the
#   REAL, currently-checked-out trusty-audit crate — the one #6400 repaired —
#   to prove the live repo passes with no fixture standing in for it.
#
# Test: this IS the test. Run directly:
#   bash scripts/check-changelog-assembled-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${SCRIPT_DIR}/check-changelog-assembled.sh"

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/changelog-assembled-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass_case() { echo "  ok  $1"; PASSED=$((PASSED + 1)); }
fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

# mkrepo <name> <crate-dir> <package-name> <version> -> prints the repo path.
# A minimal workspace shape: just enough for the gate's own resolve_crate_dir
# (a crates/<dir>/Cargo.toml with a name/version) to find it. The gate never
# invokes git when --repo is passed, so no git history is needed here.
mkrepo() {
  local name="$1" dir="$2" pkg="$3" version="$4"
  local repo="${WORK}/${name}"
  mkdir -p "${repo}/crates/${dir}"
  cat > "${repo}/crates/${dir}/Cargo.toml" <<TOML
[package]
name = "${pkg}"
version = "${version}"
edition = "2021"
TOML
  echo "$repo"
}

# run_case <label> <expect-exit> <expect-substring|-> <repo> [gate args...]
run_case() {
  local label="$1" want_exit="$2" want_sub="$3" repo="$4"
  shift 4
  local out rc=0
  out="$(bash "$GATE" --repo "$repo" "$@" 2>&1)" || rc=$?
  if [ "$rc" -ne "$want_exit" ]; then
    fail_case "${label}: expected exit ${want_exit}, got ${rc}" "$out"
    return
  fi
  if [ "$want_sub" != "-" ] && ! grep -qF -- "$want_sub" <<< "$out"; then
    fail_case "${label}: exit ${rc} but output never said '${want_sub}'" "$out"
    return
  fi
  if [ "$want_sub" = "-" ]; then
    pass_case "${label} -> exit ${rc} (clean)"
  else
    pass_case "${label} -> exit ${rc}, reported ${want_sub}"
  fi
}

# ===========================================================================
# 1. Correctly assembled — a section exists for the version, changelog.d/
#    holds only the README.md placeholder. This is what a real
#    scripts/assemble-changelog.sh run leaves behind. Exit 0.
# ===========================================================================
repo="$(mkrepo clean clean-crate clean-crate 1.0.0)"
mkdir -p "${repo}/crates/clean-crate/changelog.d"
echo "placeholder" > "${repo}/crates/clean-crate/changelog.d/README.md"
cat > "${repo}/crates/clean-crate/CHANGELOG.md" <<'EOF'
# Changelog — clean-crate

---

## [1.0.0] — 2026-01-01

### Fixed

- an assembled fix
EOF
run_case "clean assembled state" 0 "-" "$repo" clean-crate

# ===========================================================================
# 2. THE REGRESSION TEST — the #5919/#6406 shape. Cargo.toml was hand-bumped,
#    the assembler never ran: changelog.d/ still holds a real fragment and
#    CHANGELOG.md has no section for the new version. Both findings, exit 1.
# ===========================================================================
repo="$(mkrepo stranded stranded-crate stranded-crate 0.9.0)"
mkdir -p "${repo}/crates/stranded-crate/changelog.d"
echo "placeholder" > "${repo}/crates/stranded-crate/changelog.d/README.md"
cat > "${repo}/crates/stranded-crate/changelog.d/6406-stranded.md" <<'EOF'
Fixed

- a fix nobody ever folded into CHANGELOG.md
EOF
cat > "${repo}/crates/stranded-crate/CHANGELOG.md" <<'EOF'
# Changelog — stranded-crate

---

## [0.8.0] — 2026-01-01

### Fixed

- an older, already-released fix
EOF
run_case "hand-edited bump: stranded fragment" 1 "STRANDED-FRAGMENTS" "$repo" stranded-crate
run_case "hand-edited bump: no section (same fixture)" 1 "NO-SECTION" "$repo" stranded-crate

# ===========================================================================
# 3. Section exists, but a fragment survives anyway (a partial/aborted
#    assemble, or a fragment added by hand after the fact). One finding only.
# ===========================================================================
repo="$(mkrepo partial partial-crate partial-crate 2.0.0)"
mkdir -p "${repo}/crates/partial-crate/changelog.d"
echo "placeholder" > "${repo}/crates/partial-crate/changelog.d/README.md"
cat > "${repo}/crates/partial-crate/changelog.d/9999-leftover.md" <<'EOF'
Added

- something that should have been consumed
EOF
cat > "${repo}/crates/partial-crate/CHANGELOG.md" <<'EOF'
# Changelog — partial-crate

---

## [2.0.0] — 2026-01-01

### Added

- the real assembled entry
EOF
run_case "section present, fragment still stranded" 1 "STRANDED-FRAGMENTS" "$repo" partial-crate
out="$(bash "$GATE" --repo "$repo" partial-crate 2>&1)" || true
if grep -qF "NO-SECTION" <<< "$out"; then
  fail_case "section present, fragment still stranded: falsely also reported NO-SECTION" "$out"
else
  pass_case "section present, fragment still stranded -> only STRANDED-FRAGMENTS reported"
fi

# ===========================================================================
# 4. No changelog.d/ directory at all (never created, or already cleaned up)
#    but the section IS present — a legitimate clean state. Exit 0.
# ===========================================================================
repo="$(mkrepo nodir nodir-crate nodir-crate 3.1.0)"
cat > "${repo}/crates/nodir-crate/CHANGELOG.md" <<'EOF'
# Changelog — nodir-crate

---

## [3.1.0] — 2026-01-01

### Changed

- entry with no changelog.d/ directory left behind
EOF
run_case "no changelog.d/ directory, section present" 0 "-" "$repo" nodir-crate

# ===========================================================================
# 5. THE LIVE REPO. trusty-audit was the crate #5919/#6400 repaired — this
#    proves the check passes against the real, current checkout rather than
#    only a fixture built to make it pass.
# ===========================================================================
if [ -f "${REPO_ROOT}/crates/trusty-audit/Cargo.toml" ]; then
  run_case "live repo: trusty-audit (post-#6400 repair)" 0 "-" "$REPO_ROOT" trusty-audit
else
  echo "  skip live-repo case: crates/trusty-audit not found at ${REPO_ROOT}"
fi

echo
echo "check-changelog-assembled-selftest: ${PASSED} passed, ${FAILED} failed."
[ "$FAILED" -eq 0 ]

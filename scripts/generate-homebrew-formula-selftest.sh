#!/usr/bin/env bash
#
# generate-homebrew-formula-selftest.sh — fixtures for
# scripts/generate-homebrew-formula.sh.
#
# Why: two things need proving and neither is visible from a green release run.
#
#   1. BEHAVIOR PRESERVATION. The generator was extracted from an inline Python
#      heredoc in release.yml. The only acceptable evidence that the extraction
#      changed nothing is that it reproduces, byte for byte, the formulae the
#      retired code actually pushed. PASS A renders all nine live tap formulae
#      from their own recorded inputs (scripts/test-data/homebrew/tap-inputs.tsv)
#      and diffs against the real bytes (expected/<crate>.rb).
#
#   2. THE EXIT CONTRACT. A formula generator that returns 0 whether or not it
#      wrote anything reports success from the evidence that it ran. PASS B
#      pins every status the script can produce, and case `noop-exit-3` is the
#      one that matters: a run that updates zero formulae without being asked to
#      MUST exit 3, never 0. If that case ever goes green at 0, the summary line
#      has stopped meaning anything and the gate is decorative.
#
#   The two passes fail for different reasons on purpose. A renderer drift shows
#   up as a diff naming the line that moved; a contract regression shows up as an
#   exit-status mismatch. Neither can mask the other.
#
# What: builds a scratch tap under $TMPDIR, runs the generator against it, and
#   asserts exit status, summary text, and (pass A) exact bytes.
#
# Test: this IS the test. Run directly:
#   bash scripts/generate-homebrew-formula-selftest.sh
#
# Portability: POSIX tools only, bash 3.2 (macOS) and bash 5 (Linux CI).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$SCRIPT_DIR/generate-homebrew-formula.sh"
DATA_DIR="$SCRIPT_DIR/test-data/homebrew"
INPUTS="$DATA_DIR/tap-inputs.tsv"
EXPECTED_DIR="$DATA_DIR/expected"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/generate-homebrew-formula-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

TAB="$(printf '\t')"
fail=0

for required in "$GEN" "$INPUTS"; do
  if [ ! -f "$required" ]; then
    echo "FAIL: missing $required" >&2
    exit 1
  fi
done

# ---------------------------------------------------------------------------
# PASS A — byte identity against the formulae the live tap actually holds.
# ---------------------------------------------------------------------------
echo "--- pass A: byte identity against the live tap ---"

rendered=0
while IFS="$TAB" read -r crate version tag bins macos linux; do
  case "$crate" in ''|'#'*) continue ;; esac

  expected="$EXPECTED_DIR/$crate.rb"
  if [ ! -f "$expected" ]; then
    echo "FAIL: $crate -> no golden at $expected" >&2
    fail=1
    continue
  fi

  out_dir="$WORK/A/$crate"
  mkdir -p "$out_dir"
  rc=0
  bash "$GEN" --crate "$crate" --version "$version" --tag "$tag" \
    --binaries "$bins" --macos-sha256 "$macos" --linux-sha256 "$linux" \
    --formula-dir "$out_dir" >"$WORK/A-$crate.out" 2>"$WORK/A-$crate.err" || rc=$?

  if [ "$rc" -ne 0 ]; then
    echo "FAIL: $crate -> exit $rc (expected 0: a fresh render always writes)" >&2
    sed 's/^/       /' "$WORK/A-$crate.err" >&2
    fail=1
    continue
  fi

  if ! grep -qF "1 crate(s) considered, 1 formula(e) updated" "$WORK/A-$crate.out"; then
    echo "FAIL: $crate -> summary does not report one update:" >&2
    sed 's/^/       /' "$WORK/A-$crate.out" >&2
    fail=1
    continue
  fi

  if ! diff -u "$expected" "$out_dir/$crate.rb" >"$WORK/A-$crate.diff" 2>&1; then
    echo "FAIL: $crate -> rendered bytes differ from the live tap formula:" >&2
    sed 's/^/       /' "$WORK/A-$crate.diff" >&2
    fail=1
    continue
  fi

  echo "PASS: $crate -> byte-identical to the tap ($(wc -c <"$expected" | tr -d ' ') bytes)"
  rendered=$((rendered + 1))
done < "$INPUTS"

# A pass that rendered nothing is the same lie the gate itself refuses to tell.
if [ "$rendered" -eq 0 ]; then
  echo "FAIL: pass A rendered zero formulae — $INPUTS produced no usable rows." >&2
  fail=1
else
  echo "pass A: $rendered formula(e) reproduced byte-for-byte."
fi

# ---------------------------------------------------------------------------
# PASS B — the exit contract.
# ---------------------------------------------------------------------------
echo "--- pass B: exit contract ---"

# One crate's inputs, reused for every case. trusty-installer is the multi-binary
# shape, so a case that breaks binary handling fails here rather than silently.
B_CRATE="trusty-installer"
B_VERSION="0.8.0"
B_TAG="trusty-installer-v0.8.0"
B_BINS="trusty-installer tctl"
B_MAC="b7dcb4dbc8a1e1b38e31c99f024a7a32075bc41fbc2e62ff89e1e9fe568d7482"
B_LIN="c8c1754fa9f20d0868dc6fcea320d1cf9c180eda977107fdc81a4e180cd9fa0e"

B_DIR="$WORK/B"
mkdir -p "$B_DIR"

# run_case <label> <expected_exit> <expected_substring|-> [args...]
run_case() {
  local label="$1" expected_exit="$2" expected_sub="$3"
  shift 3
  local rc=0
  bash "$GEN" "$@" >"$WORK/B-$label.out" 2>"$WORK/B-$label.err" || rc=$?

  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: $label -> exit $rc (expected $expected_exit)" >&2
    sed 's/^/       /' "$WORK/B-$label.err" >&2
    sed 's/^/       /' "$WORK/B-$label.out" >&2
    fail=1
    return
  fi

  if [ "$expected_sub" != "-" ] \
     && ! grep -qF -- "$expected_sub" "$WORK/B-$label.out" \
     && ! grep -qF -- "$expected_sub" "$WORK/B-$label.err"; then
    echo "FAIL: $label -> exit $rc but output never mentions '$expected_sub'" >&2
    sed 's/^/       /' "$WORK/B-$label.err" >&2
    sed 's/^/       /' "$WORK/B-$label.out" >&2
    fail=1
    return
  fi

  echo "PASS: $label -> exit $rc"
}

# First write into an empty directory: a real update.
run_case "first-write" 0 "1 crate(s) considered, 1 formula(e) updated" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" --formula-dir "$B_DIR"

# THE CASE THIS FILE EXISTS FOR. Identical inputs, formula already on disk:
# zero formulae updated, so exit 3 (NO VERDICT) — never 0.
run_case "noop-exit-3" 3 "1 crate(s) considered, 0 formula(e) updated" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" --formula-dir "$B_DIR"

# ...and the summary must say zero, not merely exit 3.
if ! grep -qF "0 formula(e) updated" "$WORK/B-noop-exit-3.out"; then
  echo "FAIL: noop-exit-3 -> exit 3 but the summary does not report zero updates" >&2
  fail=1
fi

# The same no-op, asked for: a verdict, exit 0.
run_case "expect-unchanged-holds" 0 "unchanged, as asserted" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" --formula-dir "$B_DIR" \
  --expect-unchanged

# --expect-unchanged asserted against inputs that DO change the file: exit 1.
# The hatch states a different expectation; it never buys a pass.
run_case "expect-unchanged-violated" 1 "--expect-unchanged was asserted" \
  --crate "$B_CRATE" --version "9.9.9" --tag "trusty-installer-v9.9.9" \
  --binaries "$B_BINS" --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" \
  --formula-dir "$B_DIR" --expect-unchanged

# A tag that names a different version than --version. Renders URLs pointing at
# a release that does not exist, so it must not render at all.
run_case "tag-version-disagree" 1 "does not end in" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "trusty-installer-v0.7.0" \
  --binaries "$B_BINS" --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" \
  --formula-dir "$WORK/B-tagfail"

# No digest passed and no sidecar to find: the formula has exactly one sha256
# slot per platform, so this cannot degrade into a partial write.
mkdir -p "$WORK/empty-assets"
run_case "missing-sha256" 1 "no SHA-256 for" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --assets-dir "$WORK/empty-assets" --formula-dir "$WORK/B-shafail"

# Malformed digest: caught here rather than at `brew install`.
run_case "bad-digest" 1 "not lowercase hex" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --macos-sha256 "NOTAHASH" --linux-sha256 "$B_LIN" --formula-dir "$WORK/B-digestfail"

# Empty binary list: a formula that installs nothing.
run_case "no-binaries" 2 "-" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "" \
  --macos-sha256 "$B_MAC" --linux-sha256 "$B_LIN" --formula-dir "$WORK/B-binfail"

# Missing required argument, and an unknown one: usage, exit 2.
run_case "missing-required-arg" 2 "usage:" --crate "$B_CRATE"
run_case "unknown-arg" 2 "unknown argument" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" --wat

run_case "help" 0 "generate-homebrew-formula.sh" --help

# ---------------------------------------------------------------------------
# PASS C — the CI path: digests read from the .sha256 sidecars the build job
# uploads, rather than passed on the command line. These are two different code
# paths to the same bytes, and only the sidecar one ever runs in release.yml.
# ---------------------------------------------------------------------------
echo "--- pass C: digests read from .sha256 sidecars ---"

ASSETS="$WORK/C-assets/some/nested/dir"
mkdir -p "$ASSETS"
printf '%s  %s\n' "$B_MAC" "${B_CRATE}-${B_VERSION}-aarch64-apple-darwin.tar.gz" \
  > "$ASSETS/${B_CRATE}-${B_VERSION}-aarch64-apple-darwin.tar.gz.sha256"
printf '%s  %s\n' "$B_LIN" "${B_CRATE}-${B_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
  > "$ASSETS/${B_CRATE}-${B_VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256"

C_DIR="$WORK/C-formula"
run_case "sidecar-digests" 0 "1 formula(e) updated" \
  --crate "$B_CRATE" --version "$B_VERSION" --tag "$B_TAG" --binaries "$B_BINS" \
  --assets-dir "$WORK/C-assets" --formula-dir "$C_DIR"

if [ -f "$C_DIR/$B_CRATE.rb" ] && [ -f "$EXPECTED_DIR/$B_CRATE.rb" ]; then
  if diff -u "$EXPECTED_DIR/$B_CRATE.rb" "$C_DIR/$B_CRATE.rb" >"$WORK/C.diff" 2>&1; then
    echo "PASS: sidecar-digests -> identical to the explicit-digest render"
  else
    echo "FAIL: sidecar-digests -> bytes differ from the golden:" >&2
    sed 's/^/       /' "$WORK/C.diff" >&2
    fail=1
  fi
else
  echo "FAIL: sidecar-digests -> no formula written" >&2
  fail=1
fi

# ---------------------------------------------------------------------------
if [ "$fail" -ne 0 ]; then
  echo "generate-homebrew-formula-selftest: one or more cases FAILED." >&2
  exit 1
fi

echo "generate-homebrew-formula-selftest: all cases passed."
exit 0

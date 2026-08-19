#!/usr/bin/env bash
#
# build_accel_selftest.sh — self-test for scripts/lib/build_accel.sh.
#
# Why: the knob this library resolves sits on the release path, and the failure
#   mode that matters is not "the speedup did not happen" — it is a resolver that
#   changes what the SemVer gate does on a machine where the speedup is
#   unavailable. Every case below asks the same question in a different
#   environment: does an absent or opted-out accelerator resolve to EMPTY, so the
#   caller issues the command it issued before this library existed?
#
# What: four detection cases and four mode-line cases. Nothing here compiles,
#   and nothing reaches the network.
#   1. sccache absent from PATH                  -> empty
#   2. sccache on PATH                           -> its absolute path
#   3. PREFLIGHT_NO_SCCACHE=1 with sccache there -> empty (opt-out honoured)
#   4. PREFLIGHT_NO_SCCACHE=0                    -> NOT an opt-out
#
#   The integration half — that this resolution actually reaches the
#   `cargo semver-checks` subprocess, that the opt-out reaches it, that
#   CARGO_TARGET_DIR is not injected alongside it, and that a build failure under
#   the wrapper still exits 3 — lives in scripts/check_semver_selftest.sh cases
#   25-27, because that file already carries the stub cargo, stub registry and
#   stub crate those cases need.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
LIB="${REPO_ROOT}/scripts/lib/build_accel.sh"

PASSED=0
FAILED=0
WORK="$(mktemp -d "${TMPDIR:-/tmp}/build-accel-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# A stub sccache. Never executed — the resolver only asks PATH whether the name
# exists — but it must be executable, because `command -v` says nothing about a
# non-executable file.
STUB_BIN="${WORK}/bin"
mkdir -p "$STUB_BIN"
printf '#!/bin/sh\nexec "$@"\n' > "${STUB_BIN}/sccache"
chmod +x "${STUB_BIN}/sccache"

# A PATH with no sccache anywhere on it. Case 1 must not be quietly satisfied by
# the absence of a real sccache install on whoever's machine is running this, nor
# broken by its presence.
EMPTY_BIN="${WORK}/empty-bin"
mkdir -p "$EMPTY_BIN"

# resolve <fn> — run one resolver in a clean subshell, printing stdout only. Each
# case sets the variables it is about through the caller's environment; the
# subshell keeps one case from leaking into the next, which is the whole reason
# these functions print instead of exporting.
resolve() {
  # shellcheck source=lib/build_accel.sh
  ( . "$LIB" && "$1" )
}

# ===========================================================================
# 1-4. sccache detection and its opt-out.
# ===========================================================================

# --- 1. Absent from PATH. The common case on a CI runner, and the one that must
#        cost nothing: no error, no warning, no wrapper.
out="$(PATH="$EMPTY_BIN" resolve build_accel_sccache 2>&1)"
if [[ -n "$out" ]]; then
  fail_case "sccache/absent: resolved '${out}' with no sccache on PATH — the gate would run under a wrapper that is not there"
else
  pass_case "sccache absent from PATH resolves to empty"
fi

# --- 2. Present. `command -v` returns the path PATH resolved, so the assertion
#        is on the value the caller would actually put in RUSTC_WRAPPER.
out="$(PATH="${STUB_BIN}:${EMPTY_BIN}" resolve build_accel_sccache 2>&1)"
if [[ "$out" != "${STUB_BIN}/sccache" ]]; then
  fail_case "sccache/present: expected '${STUB_BIN}/sccache', got '${out}'"
else
  pass_case "sccache on PATH resolves to its absolute path"
fi

# --- 3. Opt-out. This is the escape hatch a release operator reaches for when
#        they suspect the wrapper, so it has to win over detection rather than
#        merely be consulted alongside it.
out="$(PATH="${STUB_BIN}:${EMPTY_BIN}" PREFLIGHT_NO_SCCACHE=1 resolve build_accel_sccache 2>&1)"
if [[ -n "$out" ]]; then
  fail_case "sccache/opt-out: PREFLIGHT_NO_SCCACHE=1 still resolved '${out}'"
else
  pass_case "PREFLIGHT_NO_SCCACHE=1 suppresses a present sccache"
fi

# --- 4. `0` is not an opt-out. A variable named NO_<thing> set to zero reads as
#        "do not disable" to whoever writes it; silently meaning the opposite
#        would turn the accelerator off for someone who thought they had turned
#        it on, and they would find out from a release timing, not an error.
out="$(PATH="${STUB_BIN}:${EMPTY_BIN}" PREFLIGHT_NO_SCCACHE=0 resolve build_accel_sccache 2>&1)"
if [[ "$out" != "${STUB_BIN}/sccache" ]]; then
  fail_case "sccache/zero: PREFLIGHT_NO_SCCACHE=0 was treated as an opt-out; got '${out}'"
else
  pass_case "PREFLIGHT_NO_SCCACHE=0 is not an opt-out"
fi

# --- 5. Nothing on stdout but the path. A caller reads stdout as the value for
#        RUSTC_WRAPPER, so any diagnostic that leaked there would be used as one.
out="$(PATH="$EMPTY_BIN" resolve build_accel_sccache 2> /dev/null)"
if [[ -n "$out" ]]; then
  fail_case "sccache/stdout: the absent case put '${out}' on stdout"
else
  pass_case "the absent case leaves stdout empty"
fi

# ===========================================================================
# 6-9. The mode line. One line in every mode, including the modes where nothing
# is accelerated — that line is the only report this library makes, so a mode it
# cannot describe is a mode nobody can diagnose.
# ===========================================================================
mode_line() {
  # shellcheck source=lib/build_accel.sh
  ( . "$LIB" && build_accel_mode_line "$1" )
}

out="$(mode_line '/usr/bin/sccache')"
if [[ "$out" != *"sccache /usr/bin/sccache"* || "$out" != *"RUSTC_WRAPPER"* ]]; then
  fail_case "mode-line/present: did not name the wrapper it applied" "$out"
else
  pass_case "the mode line names the sccache it applied"
fi

out="$(PATH="$EMPTY_BIN" mode_line '')"
if [[ "$out" != *"no sccache on PATH"* ]]; then
  fail_case "mode-line/absent: did not state the unaccelerated mode" "$out"
else
  pass_case "the mode line states the unaccelerated mode"
fi

# An opt-out and an absent binary are different facts. Reporting the opt-out as
# "not installed" would send an operator to `brew install` over a variable they
# set themselves.
out="$(PREFLIGHT_NO_SCCACHE=1 mode_line '')"
if [[ "$out" != *"disabled by PREFLIGHT_NO_SCCACHE"* ]]; then
  fail_case "mode-line/opt-out: reported the opt-out as an absent binary" "$out"
else
  pass_case "the mode line distinguishes the opt-out from an absent binary"
fi

# Exactly one line, always. A caller prints this unconditionally on the release
# path; two lines today is a paragraph after the next edit.
lengths_ok=1
for arg in '/usr/bin/sccache' ''; do
  n="$(mode_line "$arg" | wc -l | tr -d ' ')"
  if [[ "$n" != "1" ]]; then
    fail_case "mode-line/length: emitted ${n} lines for '${arg}'"
    lengths_ok=0
  fi
done
if [[ "$lengths_ok" -eq 1 ]]; then
  pass_case "the mode line is exactly one line in every state"
fi

# --- 10. The library must not reintroduce a persistent CARGO_TARGET_DIR. It was
#         built, measured and rejected: CARGO_TARGET_DIR moves the current
#         crate's rustdoc JSON to a flat, name-keyed path, which blinds
#         check_semver_types.sh and lets parallel worktrees overwrite each
#         other's document. The rationale is a comment, and a comment does not
#         fail a build — this assertion does.
if grep -q '^[^#]*CARGO_TARGET_DIR' "$LIB"; then
  fail_case "no-target-dir: build_accel.sh has live CARGO_TARGET_DIR code again — see the rejection note in its header" \
    "$(grep -n '^[^#]*CARGO_TARGET_DIR' "$LIB")"
else
  pass_case "the library sets no CARGO_TARGET_DIR (rejected on evidence, see header)"
fi

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "build_accel_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "build_accel_selftest: ${PASSED} passed, 0 failed."

#!/usr/bin/env bash
#
# check_context_budget_selftest.sh — mutation self-test for the startup-context
# size gate (#7424).
#
# Why: the gate it tests is the only thing that notices the startup prompt
#   growing one small addition at a time — the exact shape #4513 describes, where
#   no single PR ever looked like the problem. A gate in that position is
#   untested code unless something re-proves it can still FAIL, which is the
#   #4618 lesson and the reason most cases below are trees that must be
#   rejected.
#
# What: builds throwaway fixture trees under a temp dir, points the gate at each
#   with CONTEXT_BUDGET_ROOT / CONTEXT_BUDGET_BASELINE, and asserts the exit
#   status and the reason. Cases:
#     unchanged       every file matches the baseline                  -> PASS
#     shrunk          a file got smaller (one-directional gate)        -> PASS
#     total_over      TOTAL past +5% with no single file past +10%     -> FAIL
#     file_over       one file past +10% with TOTAL inside +5%         -> FAIL
#     unlisted        a new section file with no baseline row          -> FAIL
#     vanished        a baseline row whose file is gone                -> FAIL
#     scan_floor      a tree with too few sources to be a real scan    -> FAIL
#     update          --update records the current sizes and re-passes -> PASS
#   Plus a final case asserting the LIVE repo scan passes against the committed
#   baseline, so the gate and the tree it guards cannot disagree silently.
#
# Usage:
#   bash scripts/check_context_budget_selftest.sh            # every case
#   bash scripts/check_context_budget_selftest.sh file_over  # one by name
#
# Exit: 0 when every case reaches its expected verdict; 1 naming the first case
#   that did not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check_context_budget.sh"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TMPROOT="$(mktemp -d)"
trap 'rm -rf "${TMPROOT}"' EXIT

failures=0
only="${1:-}"

pass() { printf '  ok   %s\n' "$1"; }
bad() {
  printf '  FAIL %s: %s\n' "$1" "$2" >&2
  failures=$((failures + 1))
}

# fixture <name> — build a fixture tree whose shape matches the real sources,
# and echo its root. Six section files plus CLAUDE.md and the output style keeps
# it above the default scan floor of 5.
fixture() {
  local root="${TMPROOT}/$1"
  local sections="${root}/crates/trusty-mpm/src/assets/instructions/sections"
  local styles="${root}/crates/trusty-mpm/src/assets/output-styles"
  mkdir -p "${sections}" "${styles}"
  # 1000 bytes each: a round baseline makes every percentage in the cases below
  # exact rather than approximate.
  local name
  for name in core workflow identity memory search enforcement; do
    head -c 1000 /dev/zero | tr '\0' 'x' >"${sections}/${name}.md"
  done
  head -c 1000 /dev/zero | tr '\0' 'x' >"${root}/CLAUDE.md"
  head -c 1000 /dev/zero | tr '\0' 'x' >"${styles}/trusty-mpm.md"
  printf '%s' "${root}"
}

# grow <file> <bytes> — append `bytes` bytes to a fixture file.
grow() {
  head -c "$2" /dev/zero | tr '\0' 'y' >>"$1"
}

# run_gate <root> <baseline> [args...] — run the gate, capturing output.
run_gate() {
  local root="$1" baseline="$2"
  shift 2
  CONTEXT_BUDGET_ROOT="${root}" CONTEXT_BUDGET_BASELINE="${baseline}" \
    bash "${GATE}" "$@" >"${TMPROOT}/out" 2>&1
}

# want_case <name> <expect: pass|fail> <root> <baseline> [expected-substring]
want_case() {
  local name="$1" expect="$2" root="$3" baseline="$4" needle="${5:-}"
  if [ -n "${only}" ] && [ "${only}" != "${name}" ]; then
    return 0
  fi
  run_gate "${root}" "${baseline}"
  local status=$?
  if [ "${expect}" = "pass" ] && [ "${status}" -ne 0 ]; then
    bad "${name}" "expected exit 0, got ${status}: $(cat "${TMPROOT}/out")"
    return 0
  fi
  if [ "${expect}" = "fail" ] && [ "${status}" -eq 0 ]; then
    bad "${name}" "expected a non-zero exit, got 0: $(cat "${TMPROOT}/out")"
    return 0
  fi
  if [ -n "${needle}" ] && ! grep -q "${needle}" "${TMPROOT}/out"; then
    bad "${name}" "output did not mention '${needle}': $(cat "${TMPROOT}/out")"
    return 0
  fi
  pass "${name}"
}

printf 'context-budget gate self-test\n'

# --- unchanged ---------------------------------------------------------------
root="$(fixture unchanged)"
baseline="${TMPROOT}/unchanged.tsv"
run_gate "${root}" "${baseline}" --update
want_case unchanged pass "${root}" "${baseline}" "OK"

# --- shrunk ------------------------------------------------------------------
root="$(fixture shrunk)"
baseline="${TMPROOT}/shrunk.tsv"
run_gate "${root}" "${baseline}" --update
head -c 200 /dev/zero | tr '\0' 'x' >"${root}/CLAUDE.md"
want_case shrunk pass "${root}" "${baseline}" "OK"

# --- total_over --------------------------------------------------------------
# +60 bytes on each of the eight files: every file is +6% (inside the 10%
# per-file allowance) while the total is +6% (outside the 5% one), so only the
# TOTAL arm can catch it.
root="$(fixture total_over)"
baseline="${TMPROOT}/total_over.tsv"
run_gate "${root}" "${baseline}" --update
for f in "${root}/CLAUDE.md" \
  "${root}/crates/trusty-mpm/src/assets/instructions/sections/"*.md \
  "${root}/crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md"; do
  grow "${f}" 60
done
want_case total_over fail "${root}" "${baseline}" "more than 5% over baseline"

# --- file_over ---------------------------------------------------------------
# +150 bytes on ONE file: that file is +15% (outside its 10% allowance) while
# the total is +1.9% (inside the 5% one), so only the per-file arm can catch it.
root="$(fixture file_over)"
baseline="${TMPROOT}/file_over.tsv"
run_gate "${root}" "${baseline}" --update
grow "${root}/crates/trusty-mpm/src/assets/instructions/sections/core.md" 150
want_case file_over fail "${root}" "${baseline}" "more than 10% over baseline"

# --- unlisted ----------------------------------------------------------------
root="$(fixture unlisted)"
baseline="${TMPROOT}/unlisted.tsv"
run_gate "${root}" "${baseline}" --update
head -c 900 /dev/zero | tr '\0' 'z' \
  >"${root}/crates/trusty-mpm/src/assets/instructions/sections/brand-new.md"
want_case unlisted fail "${root}" "${baseline}" "absent from the baseline"

# --- vanished ----------------------------------------------------------------
root="$(fixture vanished)"
baseline="${TMPROOT}/vanished.tsv"
run_gate "${root}" "${baseline}" --update
rm "${root}/crates/trusty-mpm/src/assets/instructions/sections/search.md"
want_case vanished fail "${root}" "${baseline}" "which the scan did not find"

# --- scan_floor --------------------------------------------------------------
# A tree the enumeration cannot see is the #4618 shape: with no sources found,
# every comparison above is vacuously satisfied and the gate would report a
# clean pass over nothing.
root="${TMPROOT}/scan_floor"
mkdir -p "${root}"
baseline="${TMPROOT}/scan_floor.tsv"
printf 'TOTAL\t0\n' >"${baseline}"
want_case scan_floor fail "${root}" "${baseline}" "scan floor"

# --- update ------------------------------------------------------------------
if [ -z "${only}" ] || [ "${only}" = "update" ]; then
  root="$(fixture update)"
  baseline="${TMPROOT}/update.tsv"
  run_gate "${root}" "${baseline}" --update
  grow "${root}/crates/trusty-mpm/src/assets/instructions/sections/core.md" 400
  if run_gate "${root}" "${baseline}"; then
    bad update "a 40% growth must fail before --update records it"
  elif ! run_gate "${root}" "${baseline}" --update; then
    bad update "--update failed: $(cat "${TMPROOT}/out")"
  elif ! run_gate "${root}" "${baseline}"; then
    bad update "the gate must pass against the rewritten baseline: $(cat "${TMPROOT}/out")"
  elif ! grep -q '1400' "${baseline}"; then
    bad update "the rewritten baseline must record the new size"
  else
    pass update
  fi
fi

# --- live --------------------------------------------------------------------
if [ -z "${only}" ] || [ "${only}" = "live" ]; then
  if (cd "${REPO_ROOT}" && bash "${GATE}" >"${TMPROOT}/out" 2>&1); then
    pass live
  else
    bad live "the committed baseline must match the tree: $(cat "${TMPROOT}/out")"
  fi
fi

if [ "${failures}" -gt 0 ]; then
  printf '%s case(s) failed.\n' "${failures}" >&2
  exit 1
fi
printf 'all cases passed.\n'

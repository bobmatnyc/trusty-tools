#!/usr/bin/env bash
#
# check_context_budget.sh — the startup-context size gate (#7424, parent #4513).
#
# Why: every managed session pays for `CLAUDE.md`, the framework instruction
#   sections, and the active output style on its FIRST assistant turn, before
#   the operator's request is even read. #4513's audits (2026-08-01,
#   2026-09-11) measured turn-1 totals of 98k–107k tokens against a 50k target,
#   and the creep between those audits was many small additions rather than one
#   reviewable commit — no single PR ever looked like the problem, so nothing
#   caught it. `tm doctor`'s `startup_context` row reports the OUTCOME on a
#   machine that has run sessions; this gate reports the INPUT on the PR that
#   changes it, which is the only moment the growth is still cheap to refuse.
#
# What: sums the bytes of every source below, compares each file and the total
#   against the committed baseline, and fails when either grew past its
#   allowance. Sources:
#     CLAUDE.md
#     crates/trusty-mpm/src/assets/instructions/sections/*.md
#     crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md
#
#   The glob is taken literally, `README.md` included: the baseline names every
#   row explicitly, so what is measured is auditable in the diff rather than
#   hidden behind an exception list, and a file that legitimately grew is one
#   `--update` away.
#
#   BYTES, not tokens. A byte count needs no tokenizer, is identical on every
#   machine, and moves with the text — the only property a growth gate needs.
#   The absolute token figure is `tm doctor`'s job, measured from real sessions.
#
#   Fails CLOSED, in five ways, each naming the edit that closes it:
#     - TOTAL grew more than TOTAL_PCT (default 5%) over the baseline total
#     - one file grew more than FILE_PCT (default 10%) over its baseline row
#     - a scanned file has no baseline row (a new section file was added)
#     - a baseline row names a file that no longer exists
#     - fewer than MIN_FILES were scanned (#4618 scan floor: a broken
#       enumeration must not report success over nothing)
#
#   A file that SHRANK is never a failure — the gate is one-directional, and
#   `--update` is how a shrink is recorded.
#
# Usage:
#   bash scripts/check_context_budget.sh            # the gate
#   bash scripts/check_context_budget.sh --update   # rewrite the baseline
#
# Env (fixtures only): CONTEXT_BUDGET_ROOT overrides the tree scanned,
#   CONTEXT_BUDGET_BASELINE the baseline file, CONTEXT_BUDGET_TOTAL_PCT and
#   CONTEXT_BUDGET_FILE_PCT the two allowances, CONTEXT_BUDGET_MIN_FILES the
#   scan floor.
#
# Exit: 0 when every file and the total are inside their allowance; 1 naming
#   the first violation and the `--update` that records a deliberate growth.
#
# Test: scripts/check_context_budget_selftest.sh drives this script over
#   fixture trees — an unchanged tree, a total over its allowance, one file over
#   its own, an unlisted file, a vanished file, an empty scan, and `--update` —
#   and asserts the live repo scan passes against the committed baseline.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only, no
#   cargo and no network, so it runs in a toolchain-less shell job.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ROOT="${CONTEXT_BUDGET_ROOT:-${REPO_ROOT}}"
BASELINE="${CONTEXT_BUDGET_BASELINE:-${SCRIPT_DIR}/context-budget-baseline.tsv}"
TOTAL_PCT="${CONTEXT_BUDGET_TOTAL_PCT:-5}"
FILE_PCT="${CONTEXT_BUDGET_FILE_PCT:-10}"
MIN_FILES="${CONTEXT_BUDGET_MIN_FILES:-5}"

UPDATE=0
if [ "${1:-}" = "--update" ]; then
  UPDATE=1
elif [ -n "${1:-}" ]; then
  printf 'usage: %s [--update]\n' "$0" >&2
  exit 2
fi

failures=0

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  failures=$((failures + 1))
}

# ---------------------------------------------------------------------------
# sources — every measured path, repo-relative, one per line, in a stable order
# so the baseline diff reads the same way on every machine.
# ---------------------------------------------------------------------------
sources() {
  printf '%s\n' 'CLAUDE.md'
  # `find | sort` rather than a bare glob: the shell's glob order is locale
  # dependent, and a baseline that reorders itself between machines is noise in
  # every review.
  find "${ROOT}/crates/trusty-mpm/src/assets/instructions/sections" \
    -maxdepth 1 -name '*.md' 2>/dev/null |
    sed "s|^${ROOT}/||" | sort
  printf '%s\n' 'crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md'
}

# size_of <repo-relative path> — bytes, or empty when the file is absent.
size_of() {
  if [ -f "${ROOT}/$1" ]; then
    wc -c <"${ROOT}/$1" | tr -d ' '
  fi
}

# baseline_of <path> — the recorded byte count, or empty when unlisted.
baseline_of() {
  awk -F'\t' -v want="$1" '$1 == want { print $2; exit }' "${BASELINE}" 2>/dev/null
}

# over <current> <baseline> <percent> — 0 (true) when current exceeds the
# allowance. Integer arithmetic only; a zero baseline cannot have a percentage
# taken of it, so any growth from zero is over.
over() {
  if [ "$2" -eq 0 ]; then
    [ "$1" -gt 0 ]
    return
  fi
  [ $(($1 * 100)) -gt $(($2 * (100 + $3))) ]
}

# ---------------------------------------------------------------------------
# --update — rewrite the baseline from what is on disk right now.
# ---------------------------------------------------------------------------
if [ "${UPDATE}" -eq 1 ]; then
  tmp="${BASELINE}.tmp.$$"
  {
    printf '# context-budget baseline (#7424) — bytes per startup-context source.\n'
    printf '# Regenerate with: bash scripts/check_context_budget.sh --update\n'
    printf '# Growing TOTAL past 5%%, or one file past 10%%, fails the gate until\n'
    printf '# the growth is recorded here in the same PR.\n'
  } >"${tmp}"
  total=0
  count=0
  while IFS= read -r path; do
    bytes="$(size_of "${path}")"
    [ -n "${bytes}" ] || continue
    printf '%s\t%s\n' "${path}" "${bytes}" >>"${tmp}"
    total=$((total + bytes))
    count=$((count + 1))
  done <<EOF
$(sources)
EOF
  printf 'TOTAL\t%s\n' "${total}" >>"${tmp}"
  mv "${tmp}" "${BASELINE}"
  printf '[OK] baseline updated: %s file(s), %s bytes total -> %s\n' \
    "${count}" "${total}" "${BASELINE}"
  exit 0
fi

# ---------------------------------------------------------------------------
# gate
# ---------------------------------------------------------------------------
if [ ! -f "${BASELINE}" ]; then
  fail "no baseline at ${BASELINE} — seed it with: bash scripts/check_context_budget.sh --update"
  exit 1
fi

total=0
examined=0
scanned_list=""
while IFS= read -r path; do
  bytes="$(size_of "${path}")"
  if [ -z "${bytes}" ]; then
    continue
  fi
  scanned_list="${scanned_list}${path}
"
  total=$((total + bytes))
  examined=$((examined + 1))
  base="$(baseline_of "${path}")"
  if [ -z "${base}" ]; then
    fail "${path} is measured but absent from the baseline (${bytes} bytes) — record it with: bash scripts/check_context_budget.sh --update"
    continue
  fi
  if over "${bytes}" "${base}" "${FILE_PCT}"; then
    fail "${path} grew ${base} -> ${bytes} bytes, more than ${FILE_PCT}% over baseline — shrink it, or record the growth with: bash scripts/check_context_budget.sh --update"
  fi
done <<EOF
$(sources)
EOF

# A baseline row whose file is gone means the scan and the baseline disagree
# about what the startup context IS, which makes the TOTAL comparison
# meaningless in the quiet direction.
while IFS=$'\t' read -r path _bytes; do
  case "${path}" in
  '#'* | '' | TOTAL) continue ;;
  esac
  case "
${scanned_list}" in
  *"
${path}
"*) ;;
  *) fail "baseline lists ${path}, which the scan did not find — remove it with: bash scripts/check_context_budget.sh --update" ;;
  esac
done <"${BASELINE}"

if [ "${examined}" -lt "${MIN_FILES}" ]; then
  fail "scan floor: examined ${examined} file(s), expected at least ${MIN_FILES} — the source enumeration is broken, not the tree"
fi

base_total="$(baseline_of TOTAL)"
if [ -z "${base_total}" ]; then
  fail "the baseline has no TOTAL row — regenerate it with: bash scripts/check_context_budget.sh --update"
elif over "${total}" "${base_total}" "${TOTAL_PCT}"; then
  fail "startup context grew ${base_total} -> ${total} bytes, more than ${TOTAL_PCT}% over baseline — cut somewhere else in the same PR, or record the growth with: bash scripts/check_context_budget.sh --update"
fi

if [ "${failures}" -gt 0 ]; then
  printf 'context-budget: %s violation(s).\n' "${failures}" >&2
  exit 1
fi

printf 'context-budget: %s file(s), %s bytes (baseline %s, allowance +%s%% total / +%s%% per file) — OK.\n' \
  "${examined}" "${total}" "${base_total}" "${TOTAL_PCT}" "${FILE_PCT}"

#!/usr/bin/env bash
#
# check_contracts_selftest.sh — mutation self-test for the behavioural gate
# (scripts/check_contracts.sh, scripts/extract_contracts.py,
# scripts/diff_contracts.py).
#
# Why: this gate exists because #5620 and #5723 each shipped a checker that
#   reported success while it had verified nothing — a diff it never scanned,
#   a comparison that ran zero checks. `check_contracts.sh` and its two Python
#   helpers are written to fail closed instead: an unparseable contract, a
#   walk that found nothing, an unrecognised rustdoc/artifact schema, or a
#   comparison with no overlap all report NO VERDICT (exit 3), never a pass.
#   That posture is only real if something re-proves it on every change — a
#   later refactor that turns a `raise Unrecognised(...)` into `return None`
#   would make the gate green again while checking nothing, exactly the shape
#   of the two bugs above. This file is that proof, mirroring
#   `check_semver_selftest.sh` and `check_semver_types_selftest.sh`.
#
# What: two groups of cases.
#
#   EXTRACTION (scripts/extract_contracts.py, driven directly — it is the
#   piece `check_contracts.sh --crate` calls after building rustdoc JSON, and
#   driving it directly means this file needs no nightly toolchain and no
#   cargo build; see "Fixtures" below):
#     1. a clean extraction — one valid `# Code Contract` block, exit 0.
#     2. an unparseable contract block — free prose inside the block where
#        only a section header, a claim, or a continuation is legal. Exit 3.
#     3. zero contracts extracted — a valid document with no `# Code Contract`
#        block anywhere. Exit 3 (an extractor that found nothing has not
#        verified anything — #5620).
#     4. an unknown rustdoc schema — `format_version` outside
#        `extract_contracts.py`'s `SUPPORTED_FORMAT_VERSIONS`. Exit 3.
#
#   COMPARISON (`scripts/check_contracts.sh --diff`, the shell entry point —
#   driven through the gate itself rather than calling diff_contracts.py
#   directly, so this file also proves the shell wrapper's exit-code mapping,
#   0/1/3, and not only the Python helper underneath it):
#     5. a clean comparison — two artifacts with identical claims on the one
#        item they share. Exit 0.
#     6. drift detection — the same item, one claim reworded. Must be
#        reported as one REMOVED claim and one ADDED claim. Exit 1.
#     7. an unknown artifact version — `artifact_version` outside
#        `diff_contracts.py`'s `SUPPORTED_ARTIFACT_VERSIONS`. Exit 3.
#     8. zero common items — both artifacts are individually valid but name no
#        item in common, so nothing was actually compared. Exit 3.
#
#   Every fail-closed case (2, 3, 4, 7, 8) asserts exit 3 SPECIFICALLY, not
#   merely "nonzero" — case 6 (drift) is also nonzero (exit 1), and confusing
#   "no verdict was computed" with "a verdict found a problem" is exactly the
#   distinction this gate exists to keep visible to a caller (preflight-publish.sh
#   CHECK-style scripts branch on the difference).
#
# Fixtures: scripts/test-data/contracts/, all hand-written and synthetic (see
#   the README there). None is captured from a real `cargo +nightly rustdoc`
#   build — deliberately, so this file stays independent of whether a live
#   rustdoc build currently succeeds on this machine. That independence is not
#   theoretical: as of this writing `check_contracts.sh --crate` returns NO
#   VERDICT on `main` because the rustdoc JSON build itself fails for an
#   unrelated reason (build-environment, not this gate's logic) — a live-build
#   selftest would inherit that failure and prove nothing about the logic
#   under test. `extract_contracts.py` and `diff_contracts.py`'s own
#   fail-closed branches are pure Python over a JSON document, so fixtures
#   exercise them completely without a build.
#
# Usage:  bash scripts/check_contracts_selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs python3, which
# both helpers need anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${REPO_ROOT}/scripts/check_contracts.sh"
EXTRACTOR="${REPO_ROOT}/scripts/extract_contracts.py"
FIXTURES="${REPO_ROOT}/scripts/test-data/contracts"

PASSED=0
FAILED=0

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

for f in \
  "${FIXTURES}/rustdoc-good.json" \
  "${FIXTURES}/rustdoc-bad-contract.json" \
  "${FIXTURES}/rustdoc-no-contracts.json" \
  "${FIXTURES}/rustdoc-bad-schema.json" \
  "${FIXTURES}/artifact-clean-a.json" \
  "${FIXTURES}/artifact-clean-b.json" \
  "${FIXTURES}/artifact-drift-a.json" \
  "${FIXTURES}/artifact-drift-b.json" \
  "${FIXTURES}/artifact-bad-version.json" \
  "${FIXTURES}/artifact-empty-common-a.json" \
  "${FIXTURES}/artifact-empty-common-b.json"; do
  if [[ ! -f "$f" ]]; then
    echo "SELF-TEST FAIL: fixture ${f} is missing; see ${FIXTURES}/README.md." >&2
    exit 1
  fi
done

# run_extractor <rustdoc.json> — capture combined output into $OUT, status
# into $RC.
OUT=""
RC=0
run_extractor() {
  RC=0
  OUT="$(python3 "$EXTRACTOR" "$1" demo 2>&1 1>/dev/null)" || RC=$?
}

# run_gate_diff <baseline.json> <current.json> — same, through the shell gate.
run_gate_diff() {
  RC=0
  OUT="$(cd "$REPO_ROOT" && bash "$GATE" --diff "$1" "$2" 2>&1)" || RC=$?
}

# ===========================================================================
# 1. A clean extraction: one valid `# Code Contract` block.
# ===========================================================================
run_extractor "${FIXTURES}/rustdoc-good.json"
if [[ "$RC" -ne 0 ]]; then
  fail_case "extraction/clean: expected exit 0, got ${RC}" "$OUT"
elif [[ "$OUT" != *"extracted: 1 contract(s)"* ]]; then
  fail_case "extraction/clean: exit 0 but did not report extracting 1 contract" "$OUT"
else
  pass_case "a valid Code Contract block is extracted (exit 0)"
fi

# ===========================================================================
# 2. Unparseable contract block: free prose where only a header, a claim, or
#    a continuation is legal.
# ===========================================================================
run_extractor "${FIXTURES}/rustdoc-bad-contract.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "extraction/unparseable: a malformed Code Contract block exited 0" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "extraction/unparseable: expected exit 3 (NO VERDICT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"NO VERDICT"* ]]; then
  fail_case "extraction/unparseable: exit 3 without saying NO VERDICT" "$OUT"
elif [[ "$OUT" != *"neither a section header"* ]]; then
  fail_case "extraction/unparseable: refused without naming the free-prose line, so it may be failing for an unrelated reason" "$OUT"
else
  pass_case "an unparseable Code Contract block is refused (exit 3)"
fi

# ===========================================================================
# 3. Zero contracts extracted: a valid document, no # Code Contract block
#    anywhere. "It found nothing" must not read as "nothing changed."
# ===========================================================================
run_extractor "${FIXTURES}/rustdoc-no-contracts.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "extraction/zero: a crate with no Code Contract blocks exited 0" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "extraction/zero: expected exit 3 (NO VERDICT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"0 contracts were extracted"* ]]; then
  fail_case "extraction/zero: exit 3 without saying 0 contracts were extracted" "$OUT"
else
  pass_case "zero contracts extracted is refused, not reported as clean (exit 3)"
fi

# ===========================================================================
# 4. Unknown rustdoc schema: format_version outside SUPPORTED_FORMAT_VERSIONS.
# ===========================================================================
run_extractor "${FIXTURES}/rustdoc-bad-schema.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "extraction/bad-schema: an unrecognised format_version exited 0" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "extraction/bad-schema: expected exit 3 (NO VERDICT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"format_version"* ]]; then
  fail_case "extraction/bad-schema: refused without naming format_version, so it may be failing for an unrelated reason" "$OUT"
else
  pass_case "an unrecognised rustdoc format_version is refused (exit 3)"
fi

# ===========================================================================
# 5. A clean comparison: identical claims on the one shared item.
# ===========================================================================
run_gate_diff "${FIXTURES}/artifact-clean-a.json" "${FIXTURES}/artifact-clean-b.json"
if [[ "$RC" -ne 0 ]]; then
  fail_case "comparison/clean: expected exit 0, got ${RC}" "$OUT"
elif [[ "$OUT" != *"1 contracted item(s) in both; 0 claim change(s)"* ]]; then
  fail_case "comparison/clean: exit 0 but did not report 1 item / 0 claim changes" "$OUT"
else
  pass_case "identical contracts on a shared item compare clean (exit 0)"
fi

# ===========================================================================
# 6. Drift detection: the same item, one postcondition claim reworded.
# ===========================================================================
run_gate_diff "${FIXTURES}/artifact-drift-a.json" "${FIXTURES}/artifact-drift-b.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "comparison/drift: a reworded claim exited 0 — the change was not seen" "$OUT"
elif [[ "$RC" -ne 1 ]]; then
  fail_case "comparison/drift: expected exit 1 (DRIFT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"REMOVED function demo::f [postconditions]: returns x doubled"* ]]; then
  fail_case "comparison/drift: the removed claim was not reported" "$OUT"
elif [[ "$OUT" != *"ADDED   function demo::f [postconditions]: returns x squared"* ]]; then
  fail_case "comparison/drift: the added claim was not reported" "$OUT"
else
  pass_case "a reworded claim is reported as REMOVED + ADDED (exit 1)"
fi

# ===========================================================================
# 7. Unknown artifact version: artifact_version outside
#    SUPPORTED_ARTIFACT_VERSIONS. Must be exit 3, distinct from drift's exit 1.
# ===========================================================================
run_gate_diff "${FIXTURES}/artifact-clean-a.json" "${FIXTURES}/artifact-bad-version.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "comparison/bad-version: an unrecognised artifact_version exited 0" "$OUT"
elif [[ "$RC" -eq 1 ]]; then
  fail_case "comparison/bad-version: exited 1 (DRIFT) — an unreadable artifact is NO VERDICT, not a computed drift" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "comparison/bad-version: expected exit 3 (NO VERDICT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"artifact_version"* ]]; then
  fail_case "comparison/bad-version: refused without naming artifact_version, so it may be failing for an unrelated reason" "$OUT"
else
  pass_case "an unrecognised artifact_version is refused (exit 3)"
fi

# ===========================================================================
# 8. Zero common items: both artifacts individually valid, no item shared.
#    "0 compared" must never print as a pass (#5620).
# ===========================================================================
run_gate_diff "${FIXTURES}/artifact-empty-common-a.json" "${FIXTURES}/artifact-empty-common-b.json"
if [[ "$RC" -eq 0 ]]; then
  fail_case "comparison/empty-common: 0 shared items exited 0 — nothing was compared and it reported clean" "$OUT"
elif [[ "$RC" -eq 1 ]]; then
  fail_case "comparison/empty-common: exited 1 (DRIFT) — there was nothing to compute a drift over" "$OUT"
elif [[ "$RC" -ne 3 ]]; then
  fail_case "comparison/empty-common: expected exit 3 (NO VERDICT), got ${RC}" "$OUT"
elif [[ "$OUT" != *"0 contracted items are present in both artifacts"* ]]; then
  fail_case "comparison/empty-common: refused without saying nothing was shared, so it may be failing for an unrelated reason" "$OUT"
else
  pass_case "two artifacts with no item in common are refused, not compared (exit 3)"
fi

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_contracts_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_contracts_selftest: ${PASSED} passed, 0 failed."

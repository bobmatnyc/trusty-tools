#!/usr/bin/env bash
#
# check_deny_duplicates.sh — turn cargo-deny's duplicate-version WARNINGS into
# a gate.
#
# Why: `cargo deny check bans` reports every crate that appears at more than one
#   version, and with `multiple-versions = "warn"` it exits 0 while doing so.
#   That is the right severity for this workspace — 83 crates are duplicated
#   today, and 83 `[[bans.skip]]` rows would be a way of switching the check off
#   while appearing to run it — but a warning nobody counts is a warning nobody
#   reads. Duplicate versions cost compile time and binary size on every build,
#   and they are how two copies of a transitive dependency end up at different
#   patch levels with only one of them carrying a security fix.
#
# What: runs `cargo deny check bans`, counts the `duplicate` warnings, and
#   compares that count against `scripts/deny-duplicate-baseline.txt`. Above the
#   baseline is a failure; below it is a pass that says so and asks for the
#   baseline to be lowered.
#
# WHY A COUNT AND NOT A LIST. A per-crate list would be more precise and would
#   also be 83 rows that churn on every dependency bump, generating diff noise
#   that reviewers learn to skim. The count is the number that matters for the
#   ratchet, and `cargo deny check bans` prints the full list on every run for
#   anyone who wants the detail.
#
# FAIL-CLOSED BEHAVIOUR:
#   - cargo-deny exiting nonzero (a real ban, a wildcard, a parse error) FAILS,
#     and is reported as itself rather than folded into the count.
#   - Output with no parseable summary FAILS as a vacuous scan: zero duplicates
#     found and zero examined must never print the same thing (#5620).
#   - A missing baseline file FAILS rather than defaulting to a permissive
#     number.
#
# Exit codes: 0 = at or below baseline. 1 = above baseline, or cargo-deny found
#   a hard violation. 3 = the gate could not compute a verdict. 2 = usage.
#
# Test: verified by construction against real `cargo deny check bans` output at
#   e39183c3 — 83 duplicates under the default feature set (PASS at baseline)
#   and 117 under --all-features (FAIL, +34), which is how the ratchet's failing
#   arm was exercised. `--from-output` accepts a captured file, which is the
#   entry point a fixture-driven self-test would use; that self-test is NOT yet
#   written.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

BASELINE_FILE="${REPO_ROOT}/scripts/deny-duplicate-baseline.txt"
FROM_FILE=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --from-output) FROM_FILE="${2:-}"; shift 2 ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "check_deny_duplicates: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ ! -f "$BASELINE_FILE" ]; then
  echo "[FAIL] deny-duplicates: baseline file missing: ${BASELINE_FILE}" >&2
  echo "       Refusing to default to a permissive number." >&2
  exit 3
fi

BASELINE="$(grep -vE '^\s*(#|$)' "$BASELINE_FILE" | head -1 | tr -d '[:space:]')"
if ! [[ "$BASELINE" =~ ^[0-9]+$ ]]; then
  echo "[FAIL] deny-duplicates: baseline file does not contain a bare integer: '${BASELINE}'" >&2
  exit 3
fi

TMP_OUT="$(mktemp "${TMPDIR:-/tmp}/deny-bans.XXXXXX")"
trap 'rm -f "$TMP_OUT"' EXIT

DENY_RC=0
if [ -n "$FROM_FILE" ]; then
  [ -f "$FROM_FILE" ] || { echo "check_deny_duplicates: no such file: $FROM_FILE" >&2; exit 2; }
  cp "$FROM_FILE" "$TMP_OUT"
else
  # DEFAULT FEATURES, not --all-features. `--all-features` resolves a graph no
  # artifact ever has: trusty-common's 45 features include mutually exclusive
  # embedder backends (embedder-cuda / embedder-coreml / embedder-candle), so
  # enabling them together produces duplicates that exist in no build anyone
  # ships. Measured: 117 duplicated crates under --all-features against 83 under
  # the default set, the extra 34 being candle/gemm/pulp copies from backends
  # that are never on at the same time. Counting those would make the ratchet
  # track an artifact nobody builds.
  # `--color never` because this output is PARSED, not read. The pre-publish
  # workflow sets CARGO_TERM_COLOR=always (inherited from ci.yml's env block),
  # and cargo-deny honours it: its verdict line arrives as
  # `bans \e[32mok\e[0m`, so an anchored `^bans ok` match fails and the
  # vacuous-scan refusal below fires on a run that actually succeeded. That is
  # what happened on the gate's first real execution (run 31858449749, Gate 3).
  # The refusal behaved correctly — it declined to report a count it could not
  # trust — but the gate was unusable until the colour was turned off.
  cargo deny --color never check bans > "$TMP_OUT" 2>&1 || DENY_RC=$?
fi

# Strip any ANSI escape sequences that survived, so every match below reads
# plain text. Belt and braces alongside `--color never`: a future cargo-deny
# that colours on a different switch, or a caller passing captured colourised
# output to --from-output, must not silently turn a real verdict into a refusal.
if command -v perl >/dev/null 2>&1; then
  perl -pe 's/\e\[[0-9;]*[A-Za-z]//g' "$TMP_OUT" > "${TMP_OUT}.plain" && mv "${TMP_OUT}.plain" "$TMP_OUT"
else
  sed -E $'s/\033\\[[0-9;]*[A-Za-z]//g' "$TMP_OUT" > "${TMP_OUT}.plain" && mv "${TMP_OUT}.plain" "$TMP_OUT"
fi

# A hard violation (a banned crate, a wildcard requirement) is cargo-deny's own
# failure and is reported as itself. Folding it into the duplicate count would
# describe a wildcard dependency as "too many duplicate versions".
if [ "$DENY_RC" -ne 0 ]; then
  echo "[FAIL] deny-duplicates: 'cargo deny check bans' exited ${DENY_RC} — a hard" >&2
  echo "       ban violation, not a duplicate-count question:" >&2
  grep -E '^(error|warning)\[' "$TMP_OUT" | head -40 | sed 's/^/       /' >&2
  exit 1
fi

COUNT="$(grep -cE '^warning\[duplicate\]' "$TMP_OUT" || true)"

# Vacuous-scan refusal. cargo-deny always prints a trailing verdict line
# ("bans ok" / "bans FAILED"); its absence means the output is not what this
# parser was written against, and a zero count read off unrecognised output is
# not a finding.
if ! grep -qE '^bans (ok|FAILED)' "$TMP_OUT"; then
  echo "[FAIL] deny-duplicates: could not find cargo-deny's 'bans ok'/'bans FAILED'" >&2
  echo "       verdict line in the output. The format this parser expects has" >&2
  echo "       changed, so a count of ${COUNT} is not trustworthy. Output:" >&2
  tail -20 "$TMP_OUT" | sed 's/^/       /' >&2
  exit 3
fi

if [ "$COUNT" -gt "$BASELINE" ]; then
  echo "[FAIL] deny-duplicates: ${COUNT} duplicated crate(s), above the baseline of ${BASELINE}." >&2
  echo "       A dependency change added $(( COUNT - BASELINE )) new duplicate version(s)." >&2
  echo "       Either unify the versions, or raise the baseline in the same PR and" >&2
  echo "       say why in the PR body. Full list:" >&2
  grep -E '^warning\[duplicate\]' "$TMP_OUT" | sed 's/^/       /' >&2
  exit 1
fi

if [ "$COUNT" -lt "$BASELINE" ]; then
  echo "[PASS] deny-duplicates: ${COUNT} duplicated crate(s), below the baseline of ${BASELINE}." >&2
  echo "       Lower the number in ${BASELINE_FILE} to lock the gain in." >&2
  exit 0
fi

echo "[PASS] deny-duplicates: ${COUNT} duplicated crate(s), at the baseline." >&2
exit 0

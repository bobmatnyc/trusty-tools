#!/usr/bin/env bash
#
# check_owner_scenarios_selftest.sh — fixtures for the owner-scenario lint (#10576).
#
# Why: a lint that stops failing the click-by-click baseline still reports
#   green, and the owner gets screen scripts again. Fixture F5 of the Matt-mini
#   reliability brief is the regression: a numbered-UI-step scenario must fail,
#   an outcome scenario tracing to the matrix must pass.
# What: builds a matrix with test IDs T1-T12 and asserts the lint
#   - fails the F5 baseline ("1. Click Add. 2. Type the name. 3. Press Save.")
#     on one line, and the same steps as a numbered list;
#   - passes F5 outcome scenarios with Intent, Situation, Result and Trace,
#     at any length, and a scenario with one numbered UI step or a numbered
#     list of outcomes;
#   - fails a missing field, an empty Trace, a Trace ID absent from the matrix,
#     and a file with no scenario (scan floor);
#   - exits 2 on an unreadable matrix.
# Usage: ./scripts/check_owner_scenarios_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing each mismatch.
# Test: this IS the test.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LINT="${REPO_ROOT}/scripts/check_owner_scenarios.py"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/owner-scenarios-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0
CASES=0
MATRIX="$WORK/matrix.md"

check() {
  CASES=$((CASES + 1))
  local label="$1" want="$2" file="$3" pattern="${4:-}" matrix="${5:-$MATRIX}" out rc
  out="$(python3 "$LINT" --matrix "$matrix" "$file" 2>&1)"
  rc=$?
  if [ "$rc" != "$want" ]; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: expected exit ${want}, got ${rc}"
    printf '%s\n' "$out" | sed 's/^/        /'
  elif [ -n "$pattern" ] && ! printf '%s\n' "$out" | grep -qF -- "$pattern"; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: output does not name '${pattern}'"
    printf '%s\n' "$out" | sed 's/^/        /'
  else
    printf '  ok   %-58s -> exit %s\n' "$label" "$rc"
  fi
}

{
  echo "# Test matrix"
  echo
  echo "| ID | Test |"
  echo "|---|---|"
  for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo "| T$i | test $i |"; done
} > "$MATRIX"

# F5 baseline: numbered UI steps on one line (the failing control).
cat > "$WORK/baseline-inline.md" <<'EOF'
# Owner scenarios: adding a product

## S1 Add a product
Intent: The owner adds a new product to the shop.
Situation: The owner is signed in on the products page.
Result: 1. Click Add. 2. Type the name. 3. Press Save.
Trace: T1
EOF
check "F5 baseline, inline numbered UI steps" 1 "$WORK/baseline-inline.md" "numbered UI steps"

# F5 baseline as a numbered list, with no fields at all.
cat > "$WORK/baseline-list.md" <<'EOF'
## Add a product
1. Click **Add**.
2. Type the name.
3. Press Save.
EOF
check "F5 baseline, numbered list" 1 "$WORK/baseline-list.md" "numbered UI steps"
check "F5 baseline, numbered list misses fields" 1 "$WORK/baseline-list.md" "missing or empty \`Intent:\`"

# F5 pass: outcome scenarios that trace to the matrix.
cat > "$WORK/pass.md" <<'EOF'
# Owner scenarios: adding a product

Ordinary cases the owner reviews; about 5 minutes of review time.

## S1 Add a product with a price
- **Intent:** The owner wants a new product on sale today.
- **Situation:** The owner is signed in; the shop has 40 products.
- **Result:** The new product shows in the shop list with its price, and
  customers can buy it.
- **Trace:** T1, T4

## S2 Add a product that already exists
Intent: The owner tries to add a product whose name is taken.
Situation: A product named "Blue mug" exists.
Result: The owner sees that the name is taken and nothing is duplicated.
Outcomes the owner checks:
1. The list still shows one "Blue mug".
2. The stock count is unchanged.
Trace: T7 T12
EOF
check "F5 outcome scenarios pass" 0 "$WORK/pass.md" "2 scenario(s)"

# One numbered UI step is not a run.
cat > "$WORK/one-step.md" <<'EOF'
## S3 Find the new product
Intent: The owner checks the product is live.
Situation: The product was added a minute ago.
Result: 1. Open the shop page; the product is listed.
Trace: T2
EOF
check "one numbered UI step passes" 0 "$WORK/one-step.md"

# Length is not checked.
{
  echo "## S4 Long scenario"
  echo "Intent: The owner adds a product with a long description."
  printf 'Situation: %s\n' "$(printf 'The shop has many products. %.0s' $(seq 1 200))"
  echo "Result: The full description shows on the product page."
  echo "Trace: T3"
} > "$WORK/long.md"
check "a long scenario passes" 0 "$WORK/long.md"

cat > "$WORK/missing-trace.md" <<'EOF'
## S5 No trace
Intent: The owner adds a product.
Situation: Signed in.
Result: The product shows.
EOF
check "missing Trace fails" 1 "$WORK/missing-trace.md" "missing or empty \`Trace:\`"

cat > "$WORK/empty-trace.md" <<'EOF'
## S6 Empty trace
Intent: The owner adds a product.
Situation: Signed in.
Result: The product shows.
Trace:
EOF
check "empty Trace fails" 1 "$WORK/empty-trace.md" "missing or empty \`Trace:\`"

cat > "$WORK/unknown-id.md" <<'EOF'
## S7 Unknown test
Intent: The owner adds a product.
Situation: Signed in.
Result: The product shows.
Trace: T1, T13
EOF
check "Trace ID absent from the matrix fails" 1 "$WORK/unknown-id.md" "Trace ID \`T13\` is not in the test matrix"

cat > "$WORK/no-scenario.md" <<'EOF'
# Notes

Nothing here is a scenario.
EOF
check "a file with no scenario fails (scan floor)" 1 "$WORK/no-scenario.md" "no scenario found"

check "an unreadable matrix exits 2" 2 "$WORK/pass.md" "cannot read matrix" "$WORK/absent-matrix.md"

echo
if [ "$FAILURES" -ne 0 ]; then
  echo "check_owner_scenarios selftest: ${FAILURES} of ${CASES} case(s) FAILED."
  exit 1
fi
echo "check_owner_scenarios selftest: ${CASES} case(s) passed."

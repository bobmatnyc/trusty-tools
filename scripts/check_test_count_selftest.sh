#!/usr/bin/env bash
#
# check_test_count_selftest.sh — prove scripts/check_test_count.sh can fail.
#
# Why: the gate's whole value is the branch that refuses a zero-count run. A
#   guard whose failing branch is never exercised is untested code, and #4307 is
#   precisely a case of "green" being printed by something that did nothing —
#   the gate must not repeat the defect it exists to catch.
#
# What: drives the gate over captured `cargo test` output fixtures (the real
#   shapes from the issue's reproduction) plus live commands, and asserts the
#   exit code for each: 0 for a run with tests, 3 for a vacuous one, and the
#   command's own status when the command fails.
#
# Exit codes: 0 = every case behaved; 1 = a case did not.
#
# Test: this IS the test; .github/workflows/test-count.yml runs it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check_test_count.sh"
WORK="$(mktemp -d -t check_test_count_selftest.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0

# Assert that running the gate the given way exits with $1.
expect_exit() {
  local want="$1" name="$2"
  shift 2
  local got=0
  "$@" > "${WORK}/out.txt" 2>&1 || got=$?
  if [ "$got" -eq "$want" ]; then
    echo "  ok    $name (exit $got)"
  else
    echo "  FAIL  $name — wanted exit $want, got $got" >&2
    sed 's/^/        /' "${WORK}/out.txt" >&2
    FAILURES=$((FAILURES + 1))
  fi
}

echo "check_test_count selftest"

# ── Fixtures: the two runs from #4307, captured verbatim in shape ──

# The mistyped filter. `resolver_tests` is the FILE stem; the module is
# `collect::identity::resolver::tests`, so this matches nothing and cargo
# still prints `ok`.
cat > "${WORK}/vacuous.txt" <<'EOF'
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.09s
     Running unittests src/lib.rs (target/debug/deps/tga-1f0e3dad99908345)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 775 filtered out; finished in 0.00s
EOF

# The correct filter.
cat > "${WORK}/real.txt" <<'EOF'
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.09s
     Running unittests src/lib.rs (target/debug/deps/tga-1f0e3dad99908345)

running 33 tests

test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 742 filtered out; finished in 0.02s
EOF

# Several targets, only some of which carry tests — the aggregate is what counts.
cat > "${WORK}/multi.txt" <<'EOF'
     Running unittests src/lib.rs (target/debug/deps/tga-aaa)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s

     Running tests/audit_sweep.rs (target/debug/deps/audit_sweep-bbb)

running 4 tests

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
EOF

# Every target empty — an aggregate of zero, which is the failure.
cat > "${WORK}/multi_empty.txt" <<'EOF'
     Running unittests src/lib.rs (target/debug/deps/tga-aaa)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s

     Running tests/audit_sweep.rs (target/debug/deps/audit_sweep-bbb)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s
EOF

# cargo nextest's own shape.
cat > "${WORK}/nextest.txt" <<'EOF'
    Starting 27 tests across 9 binaries (441 skipped)
        PASS [   0.005s] tga collect::identity::resolver::tests::alias_wins
     Summary [   0.041s] 27 tests run: 27 passed, 441 skipped
EOF

expect_exit 3 "the mistyped filter from #4307 is refused" \
  bash "$GATE" --from-file "${WORK}/vacuous.txt"
expect_exit 0 "the correct filter passes" \
  bash "$GATE" --from-file "${WORK}/real.txt"
expect_exit 0 "an empty target alongside a non-empty one passes" \
  bash "$GATE" --from-file "${WORK}/multi.txt"
expect_exit 3 "every target empty is refused" \
  bash "$GATE" --from-file "${WORK}/multi_empty.txt"
expect_exit 0 "cargo nextest output is scored, not read as a vacuum" \
  bash "$GATE" --from-file "${WORK}/nextest.txt"

# ── Live commands: the wrapper must not swallow a real failure ──

expect_exit 3 "a live command that prints a zero-count run is refused" \
  bash "$GATE" -- printf 'running 0 tests\ntest result: ok. 0 passed; 12 filtered out\n'
expect_exit 0 "a live command that prints a real count passes" \
  bash "$GATE" -- printf 'running 7 tests\ntest result: ok. 7 passed; 0 filtered out\n'
expect_exit 1 "a failing command keeps its own exit status" \
  bash "$GATE" -- sh -c 'echo "running 3 tests"; echo "test result: FAILED. 2 passed; 1 failed"; exit 1'
expect_exit 101 "a cargo-shaped exit 101 is passed through, not rewritten" \
  bash "$GATE" -- sh -c 'echo "error: could not compile"; exit 101'

# ── Usage errors ──

expect_exit 2 "no arguments is a usage error" bash "$GATE"
expect_exit 2 "--from-file on a missing path is a usage error" \
  bash "$GATE" --from-file "${WORK}/does-not-exist.txt"

if [ "$FAILURES" -ne 0 ]; then
  echo "check_test_count selftest: $FAILURES case(s) failed" >&2
  exit 1
fi

echo "check_test_count selftest: all cases passed"

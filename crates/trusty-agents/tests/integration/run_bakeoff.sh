#!/usr/bin/env bash
# run_bakeoff.sh — Run one bake-off level in a staged install dir and verify.
#
# Why: Provides an automated smoke test of the full harness pipeline.
# What: Invokes `./open-mpm --workflow prescriptive --task-file task.txt` and
#       asserts that the resulting out/ directory contains Python files.
# Test: Stage a dir via install.sh, then run
#       `./run_bakeoff.sh <test_dir> 2` — expect exit 0 and "PASS" message.
#
# Usage: run_bakeoff.sh <test_dir> [level]

set -euo pipefail

TEST_DIR="${1:?usage: run_bakeoff.sh <test_dir> [level]}"
LEVEL="${2:-2}"

if [[ ! -d "$TEST_DIR" ]]; then
  echo "❌ test dir not found: $TEST_DIR" >&2
  exit 1
fi
if [[ ! -x "$TEST_DIR/open-mpm" ]]; then
  echo "❌ binary missing: $TEST_DIR/open-mpm" >&2
  exit 1
fi

echo "=== Running bake-off Level $LEVEL in $TEST_DIR ==="
cd "$TEST_DIR"
./open-mpm --workflow prescriptive --task-file task.txt

# Verify: out/ exists and contains at least one Python file.
PY_COUNT=$(find out/ -name "*.py" 2>/dev/null | wc -l | tr -d ' ')
if [[ "$PY_COUNT" -gt 0 ]]; then
  echo "✅ PASS: Found $PY_COUNT Python file(s) in output directory"
  exit 0
else
  echo "❌ FAIL: No Python files found in out/"
  exit 1
fi

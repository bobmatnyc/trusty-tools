#!/usr/bin/env bash
# Run --compare for all multi-language eval tasks
# Usage: ./run-all.sh [--out-dir out/eval]
set -euo pipefail

OUT_DIR="${1:-out/eval-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT_DIR"
TASKS_DIR="$(dirname "$0")"
BINARY="${BINARY:-cargo run --release --}"

for task_file in "$TASKS_DIR"/*.txt; do
  name="$(basename "$task_file" .txt)"
  echo "=== Running: $name ==="
  $BINARY --compare --workflow prescriptive \
    --task-file "$task_file" \
    --out-dir "$OUT_DIR/$name" \
    2>&1 | tee "$OUT_DIR/$name.log"
  echo "=== Done: $name ==="
done

echo ""
echo "All runs complete. Results in: $OUT_DIR"
ls "$OUT_DIR"/*/compare-report-*.md 2>/dev/null || echo "(no compare reports found)"

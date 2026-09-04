#!/usr/bin/env bash
# scripts/render-console-saver-preview.sh
#
# Why: the screen saver's gallery tile and its offline fallback cannot show the
# live console — one never builds a WKWebView, the other has no daemon to reach
# (#6838, #6839). Both draw a bundled PNG of the real services frame instead,
# and #6839 requires that PNG be produced by a repeatable script so it can be
# regenerated whenever the dashboard changes, not hand-captured once.
#
# What: checks the console is answering, drives `render-console-saver-preview.mjs`
# (Chromium via the Playwright install already cached under `website/`), and
# writes `crates/trusty-console/macos/saver/Resources/ConsolePreview.png` at
# 1920x1080. A capture over the size budget is downscaled with `sips` until it
# fits, because the PNG is committed and rides in every `.saver` bundle.
#
# Usage:
#   bash scripts/render-console-saver-preview.sh
#   CONSOLE_URL=http://127.0.0.1:7790/ui/screensaver bash scripts/render-console-saver-preview.sh
#   PREVIEW_MAX_BYTES=300000 bash scripts/render-console-saver-preview.sh
#
# The console must be running: the capture is of the REAL page, which is the
# point — a hand-drawn mock would drift from the dashboard it stands in for.
#
# Test: run it, then `bash scripts/build-console-saver.sh` and the `preview`
# mode of `crates/trusty-console/macos/saver/PaintHarness.swift`, which fails
# when the bundled asset is missing or does not decode.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_PATH="${PREVIEW_OUT:-$REPO_ROOT/crates/trusty-console/macos/saver/Resources/ConsolePreview.png}"
CONSOLE_URL="${CONSOLE_URL:-http://127.0.0.1:7788/ui/screensaver}"
# The asset is committed and copied into every bundle; 500 KB is the ceiling
# agreed on #6839.
MAX_BYTES="${PREVIEW_MAX_BYTES:-500000}"
# Widths tried in order when the 1920-wide capture is over budget.
DOWNSCALE_WIDTHS=(1600 1440 1280 1024)

for tool in node curl; do
  command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found on PATH." >&2; exit 1; }
done

echo "==> console check: $CONSOLE_URL"
if ! curl -fsS -o /dev/null --max-time 5 "$CONSOLE_URL"; then
  echo "ERROR: $CONSOLE_URL is not answering. Start trusty-console first." >&2
  exit 1
fi

echo "==> rendering"
PREVIEW_OUT="$OUT_PATH" CONSOLE_URL="$CONSOLE_URL" \
  node "$REPO_ROOT/scripts/render-console-saver-preview.mjs" >/dev/null

file_bytes() {
  # `stat` is BSD on macOS and GNU on Linux; wc reads the same number on both.
  wc -c < "$1" | tr -d ' '
}

bytes="$(file_bytes "$OUT_PATH")"
echo "==> captured $OUT_PATH ($bytes bytes, budget $MAX_BYTES)"

if [[ "$bytes" -gt "$MAX_BYTES" ]]; then
  command -v sips >/dev/null 2>&1 || {
    echo "ERROR: capture is $bytes bytes (over $MAX_BYTES) and sips is unavailable to downscale." >&2
    exit 1
  }
  for width in "${DOWNSCALE_WIDTHS[@]}"; do
    echo "==> over budget — resampling to ${width}px wide"
    sips --resampleWidth "$width" "$OUT_PATH" --out "$OUT_PATH" >/dev/null
    bytes="$(file_bytes "$OUT_PATH")"
    echo "    now $bytes bytes"
    [[ "$bytes" -le "$MAX_BYTES" ]] && break
  done
fi

if [[ "$bytes" -gt "$MAX_BYTES" ]]; then
  echo "ERROR: could not get $OUT_PATH under $MAX_BYTES bytes." >&2
  exit 1
fi

echo
echo "preview: $OUT_PATH ($bytes bytes)"
echo "next:    bash scripts/build-console-saver.sh   # copies it into Contents/Resources"

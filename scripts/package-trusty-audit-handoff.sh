#!/usr/bin/env bash
#
# package-trusty-audit-handoff.sh — assemble the trusty-audit handoff zip,
# shaped for issue #5483's REAL target layout (not the superseded
# pre-compiled-binaries plan).
#
# Why: #5483 supersedes the earlier "includes all necessary pre-compiled
#   binaries" plan. The auditor client is a standalone Tauri app (#5477) that
#   downloads `tga`/`trusty-analyze`/`trusty-review`/`gh` itself at pinned
#   versions at runtime, so the handoff zip carries no tool binaries at all —
#   only the signed/notarized client `.app` (#5477/#5484), the readable
#   engagement config (#5478), and a README.
#
# 🔴 THIS DOES NOT CLOSE #5483. As of this script's introduction, none of its
#   three inputs exist as real build outputs in this repo:
#     #5477 — the Tauri client app itself (no crate, no src-tauri/ anywhere
#             under crates/trusty-audit/)
#     #5478 — the engagement config generator / schema
#     #5484 — Developer-ID signing + notarization of the `.app`
#   This script only assembles whatever it is handed — it does not build,
#   sign, or generate any of the three. See
#   scripts/verify-trusty-audit-handoff-selftest.sh for how it is exercised
#   today, with synthetic fixtures standing in for all three.
#
# What: takes three inputs as PARAMETERS (never hardcoded paths, so this
#   script works unchanged the day #5477/#5478/#5484 produce real outputs)
#   and zips them into one canonical top-level layout:
#
#     <AppBundleName>.app/...   (recursive copy of --app, basename preserved)
#     <config-basename>         (--config, basename preserved)
#     README.md                 (--readme content, normalized to this name)
#
#   No tool binaries, no HTML entry point — both are explicitly out of scope
#   per #5483's current text.
#
# Platform: macOS arm64 only (#5483's own scope). The `.app` this script
#   accepts is expected to be an arm64 bundle; this script does not itself
#   check architecture — that is verify-trusty-audit-handoff.sh's job, run
#   separately, since packaging and verifying are different failure surfaces.
#
# Usage:
#   scripts/package-trusty-audit-handoff.sh \
#     --app <path/to/Name.app> --config <path/to/config.toml> \
#     --readme <path/to/README.md> --out <path/to/output.zip>
#
# Test: scripts/verify-trusty-audit-handoff-selftest.sh builds synthetic
#   fixtures with this script and asserts the verifier's pass/fail behavior
#   against what it produces.

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/package-trusty-audit-handoff.sh --app <DIR.app> --config <FILE> --readme <FILE> --out <FILE.zip>

  --app     path to the client .app bundle (a directory ending in .app,
            containing Contents/MacOS/)
  --config  path to the readable engagement config file
  --readme  path to the README to include (copied in as README.md)
  --out     path to write the resulting zip to
EOF
}

APP=""
CONFIG=""
README=""
OUT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --app) APP="${2:-}"; shift 2 ;;
    --config) CONFIG="${2:-}"; shift 2 ;;
    --readme) README="${2:-}"; shift 2 ;;
    --out) OUT="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "FAIL: unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [ -z "$APP" ] || [ -z "$CONFIG" ] || [ -z "$README" ] || [ -z "$OUT" ]; then
  echo "FAIL: --app, --config, --readme, and --out are all required" >&2
  usage
  exit 2
fi

if ! command -v zip >/dev/null 2>&1; then
  echo "FAIL: 'zip' is not available in this environment" >&2
  exit 1
fi

case "$APP" in
  *.app) ;;
  *) echo "FAIL: --app must end in .app: $APP" >&2; exit 1 ;;
esac
if [ ! -d "$APP" ]; then
  echo "FAIL: --app is not a directory: $APP" >&2
  exit 1
fi
if [ ! -d "$APP/Contents/MacOS" ]; then
  echo "FAIL: --app is not a real bundle shape (missing Contents/MacOS/): $APP" >&2
  exit 1
fi
if [ ! -f "$CONFIG" ]; then
  echo "FAIL: --config does not exist: $CONFIG" >&2
  exit 1
fi
if [ ! -f "$README" ]; then
  echo "FAIL: --readme does not exist: $README" >&2
  exit 1
fi

# Resolve to absolute paths before we `cd` into the staging directory below.
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
CONFIG="$(cd "$(dirname "$CONFIG")" && pwd)/$(basename "$CONFIG")"
README="$(cd "$(dirname "$README")" && pwd)/$(basename "$README")"
case "$OUT" in
  /*) ;;
  *) OUT="$(pwd)/$OUT" ;;
esac

APP_BASENAME="$(basename "$APP")"
CONFIG_BASENAME="$(basename "$CONFIG")"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STAGE="$WORK/stage"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/$APP_BASENAME"
cp "$CONFIG" "$STAGE/$CONFIG_BASENAME"
cp "$README" "$STAGE/README.md"

mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
( cd "$STAGE" && zip -r -X -q "$OUT" "$APP_BASENAME" "$CONFIG_BASENAME" "README.md" )

if [ ! -s "$OUT" ]; then
  echo "FAIL: zip was not produced (or is empty): $OUT" >&2
  exit 1
fi

echo "PACKAGED: $OUT"
echo "  app:    $APP_BASENAME"
echo "  config: $CONFIG_BASENAME"
echo "  readme: README.md"

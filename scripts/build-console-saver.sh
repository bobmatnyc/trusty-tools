#!/usr/bin/env bash
# scripts/build-console-saver.sh
#
# Why: trusty-console's screensaver route (#6519) needs a native macOS `.saver`
# bundle to run as an actual screen saver (#6520, epic #6516). No existing
# pipeline in this workspace produces a bundle — `tctl sign` and the
# `install-trusty-*-signed.sh` scripts sign flat Mach-O binaries on PATH, which
# is a different codesign shape. This is that missing pipeline.
#
# What: compiles crates/trusty-console/macos/saver/TrustyConsoleSaver.swift to a
# dylib with swiftc, assembles target/console-saver/TrustyConsole.saver from it
# plus the Info.plist template (injecting the trusty-console crate version) and
# the static preview asset the in-pane Preview and the offline fallback draw
# (#6839), derives the two gallery-tile thumbnails from that same asset with
# sips, lints the plist, codesigns the bundle, verifies the signature, and zips
# the result with ditto.
#
# Signing: `CODESIGN_IDENTITY` set → Developer ID with `--options runtime
# --timestamp` (Gatekeeper/notarization path). Unset → ad-hoc (`--sign -`), which
# the real host still loads: `legacyScreenSaver.appex` carries
# `com.apple.security.cs.disable-library-validation`, so an ad-hoc bundle is
# loadable locally and only distribution needs the certificate.
#
# Usage:
#   bash scripts/build-console-saver.sh                  # host arch
#   SAVER_ARCHS="arm64 x86_64" bash scripts/build-console-saver.sh   # universal
#   CODESIGN_IDENTITY="Developer ID Application: …" bash scripts/build-console-saver.sh
#
# Test: run it, then `crates/trusty-console/macos/saver/LoadHarness.swift` and
# `PaintHarness.swift` against the bundle it produces — see that directory's
# README.md, "Smoke test" and "Paint harness".
#
# Idempotent: every run removes and rebuilds the bundle and the zip in place.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$REPO_ROOT/crates/trusty-console/macos/saver"
OUT_DIR="$REPO_ROOT/target/console-saver"
OBJ_DIR="$OUT_DIR/obj"
BUNDLE="$OUT_DIR/TrustyConsole.saver"
ZIP="$OUT_DIR/TrustyConsole.saver.zip"
CARGO_TOML="$REPO_ROOT/crates/trusty-console/Cargo.toml"
# #6839: the render of the dashboard the gallery tile and the offline fallback
# draw. Regenerate with scripts/render-console-saver-preview.sh.
PREVIEW_ASSET="$SRC_DIR/Resources/ConsolePreview.png"

# #6839: gallery reads thumbnail.png/@2x by name; the isPreview draw never feeds
# the tile. Sizes match Random.saver's pair, the only Apple saver on this host
# that ships them and the only one that gets a real tile.
THUMBNAIL_WIDTH=90
THUMBNAIL_HEIGHT=58

MODULE_NAME="TrustyConsoleSaver"
DEPLOYMENT_TARGET="13.0"

# #6540: the saver's CFBundleIdentifier / codesign identifier — the bundle
# namespace, NOT a launchd label. A `.saver` is loaded by legacyScreenSaver and
# is never a launchd job. The `_IDENTIFIER` name is what exempts it from
# trusty-common's launchd-label scan, whose advice would break signing (#2558).
readonly SAVER_IDENTIFIER="com.trusty.console.saver"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: a .saver bundle is macOS-only; this host is $(uname -s)." >&2
  exit 1
fi

for tool in swiftc codesign plutil ditto sips; do
  command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found on PATH." >&2; exit 1; }
done

# The first top-level `version = "…"` in the crate manifest. Every line above it
# is a `#` comment, so a first-match read is unambiguous.
VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$CARGO_TOML")"
if [[ -z "$VERSION" ]]; then
  echo "ERROR: could not read a version from $CARGO_TOML" >&2
  exit 1
fi

ARCHS="${SAVER_ARCHS:-$(uname -m)}"

echo "==> trusty-console $VERSION → $BUNDLE"
echo "    archs: $ARCHS"

rm -rf "$BUNDLE" "$OBJ_DIR"
rm -f "$ZIP"
mkdir -p "$OBJ_DIR" "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

# #6839: the fallback asset must exist BEFORE the slow compile, so a missing one
# fails in seconds rather than after a full build.
if [[ ! -f "$PREVIEW_ASSET" ]]; then
  echo "ERROR: preview asset missing: $PREVIEW_ASSET" >&2
  echo "       generate it with: bash scripts/render-console-saver-preview.sh" >&2
  exit 1
fi

# --- compile ---------------------------------------------------------------
SLICES=()
for arch in $ARCHS; do
  slice="$OBJ_DIR/$MODULE_NAME-$arch"
  echo "==> swiftc $arch"
  swiftc \
    -emit-library \
    -O \
    -swift-version 5 \
    -module-name "$MODULE_NAME" \
    -framework ScreenSaver \
    -framework WebKit \
    -target "${arch}-apple-macosx${DEPLOYMENT_TARGET}" \
    -o "$slice" \
    "$SRC_DIR/TrustyConsoleSaver.swift"
  SLICES+=("$slice")
done

EXECUTABLE="$BUNDLE/Contents/MacOS/$MODULE_NAME"
if [[ "${#SLICES[@]}" -gt 1 ]]; then
  echo "==> lipo ${#SLICES[@]} slices"
  lipo -create "${SLICES[@]}" -output "$EXECUTABLE"
else
  cp "${SLICES[0]}" "$EXECUTABLE"
fi
chmod 755 "$EXECUTABLE"

# --- assemble --------------------------------------------------------------
cp "$PREVIEW_ASSET" "$BUNDLE/Contents/Resources/ConsolePreview.png"
echo "==> preview asset: $(wc -c < "$PREVIEW_ASSET" | tr -d ' ') bytes"

# #6839: gallery reads thumbnail.png/@2x by name; the isPreview draw never feeds
# the tile. Derived here from the same source render so the two never diverge.
SRC_W="$(sips -g pixelWidth "$PREVIEW_ASSET" | awk '/pixelWidth/ { print $2 }')"
SRC_H="$(sips -g pixelHeight "$PREVIEW_ASSET" | awk '/pixelHeight/ { print $2 }')"
if [[ -z "$SRC_W" || -z "$SRC_H" ]]; then
  echo "ERROR: sips could not read the pixel size of $PREVIEW_ASSET" >&2
  exit 1
fi

# The largest box at the tile's aspect that fits inside the source. sips crops
# on the centre, so this keeps the middle of the render and drops the overhang.
read -r CROP_W CROP_H < <(
  awk -v sw="$SRC_W" -v sh="$SRC_H" -v tw="$THUMBNAIL_WIDTH" -v th="$THUMBNAIL_HEIGHT" \
    'BEGIN {
       if (sw * th > sh * tw) { printf "%d %d\n", int(sh * tw / th), sh }
       else                   { printf "%d %d\n", sw, int(sw * th / tw) }
     }'
)

for scale in 1 2; do
  if [[ "$scale" == 1 ]]; then
    thumb="$BUNDLE/Contents/Resources/thumbnail.png"
  else
    thumb="$BUNDLE/Contents/Resources/thumbnail@2x.png"
  fi
  cp "$PREVIEW_ASSET" "$thumb"
  sips --cropToHeightWidth "$CROP_H" "$CROP_W" "$thumb" >/dev/null
  sips --resampleHeightWidth \
    "$((THUMBNAIL_HEIGHT * scale))" "$((THUMBNAIL_WIDTH * scale))" "$thumb" >/dev/null
  echo "==> thumbnail ${scale}x: $((THUMBNAIL_WIDTH * scale))x$((THUMBNAIL_HEIGHT * scale)) from ${CROP_W}x${CROP_H} crop of ${SRC_W}x${SRC_H}"
done

cp "$SRC_DIR/Info.plist" "$BUNDLE/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$BUNDLE/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$BUNDLE/Contents/Info.plist"
plutil -lint "$BUNDLE/Contents/Info.plist"

# --- sign ------------------------------------------------------------------
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "==> codesign (Developer ID): $CODESIGN_IDENTITY"
  codesign --force \
    --sign "$CODESIGN_IDENTITY" \
    --options runtime \
    --timestamp \
    --identifier "$SAVER_IDENTIFIER" \
    "$BUNDLE"
else
  echo "==> codesign (ad-hoc; set CODESIGN_IDENTITY for a distributable bundle)"
  codesign --force --sign - --identifier "$SAVER_IDENTIFIER" "$BUNDLE"
fi

codesign --verify --deep --strict --verbose=2 "$BUNDLE"
codesign -dv "$BUNDLE"

# --- package ---------------------------------------------------------------
# ditto, not `zip`: it preserves the bundle structure and extended attributes
# that notarization submission expects.
ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ZIP"

echo
echo "bundle: $BUNDLE"
echo "zip:    $ZIP"
echo "install with: bash scripts/install-console-saver.sh --from \"$BUNDLE\""

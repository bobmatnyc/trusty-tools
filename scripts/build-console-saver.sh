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
# plus the Info.plist template (injecting the trusty-console crate version),
# lints the plist, codesigns the bundle, verifies the signature, and zips the
# result with ditto.
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
# Test: run it, then `crates/trusty-console/macos/saver/LoadHarness.swift`
# against the bundle it produces — see that crate directory's README.md.
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

MODULE_NAME="TrustyConsoleSaver"
BUNDLE_ID="com.trusty.console.saver"
DEPLOYMENT_TARGET="13.0"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: a .saver bundle is macOS-only; this host is $(uname -s)." >&2
  exit 1
fi

for tool in swiftc codesign plutil ditto; do
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
mkdir -p "$OBJ_DIR" "$BUNDLE/Contents/MacOS"

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
    --identifier "$BUNDLE_ID" \
    "$BUNDLE"
else
  echo "==> codesign (ad-hoc; set CODESIGN_IDENTITY for a distributable bundle)"
  codesign --force --sign - --identifier "$BUNDLE_ID" "$BUNDLE"
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

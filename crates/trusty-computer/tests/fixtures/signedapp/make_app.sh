#!/bin/sh
# Build and sign SignedCheck.app, the second fixture bundle for the bundle-id
# scoping check (issue #11697). Usage: make_app.sh <out_dir> [identity] [impostor]
# A third argument "impostor" builds Impostor.app: different code, same bundle id.
# identity defaults to "-" (ad-hoc); pass a Developer ID name to test a
# team-signed bundle. Prints the .app path.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
ident="${2:--}"
name=SignedCheck
flags=""
if [ "${3:-}" = impostor ]; then name=Impostor; flags="-D IMPOSTOR"; fi
app="$1/$name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.trusty.spike.signedcheck</string>
<key>CFBundleName</key><string>$name</string>
<key>CFBundleExecutable</key><string>signedcheck</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.0</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PL
# shellcheck disable=SC2086  # $flags is empty or one -D word
/usr/bin/swiftc -O $flags -o "$app/Contents/MacOS/signedcheck" "$here/main.swift" >&2
/usr/bin/codesign --force --sign "$ident" --options runtime "$app" >&2
/usr/bin/codesign --verify --strict "$app" >&2
echo "$app"

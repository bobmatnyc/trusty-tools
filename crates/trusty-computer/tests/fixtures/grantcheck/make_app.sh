#!/bin/sh
# Wrap fixture.js in a minimal .app so a bounded cua manifest can scope it by
# bundle id (macOS entries need bundle_id, not executable). Usage: make_app.sh <out_dir> <state_file>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
app="$1/GrantCheck.app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.trusty.spike.grantcheck</string>
<key>CFBundleName</key><string>GrantCheck</string>
<key>CFBundleExecutable</key><string>grantcheck</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSArchitecturePriority</key><array><string>arm64</string></array>
<key>LSRequiresNativeExecution</key><true/>
</dict></plist>
PL
printf '#!/bin/sh\nexec /usr/bin/osascript -l JavaScript "%s/fixture.js" "%s"\n' "$here" "$2" > "$app/Contents/MacOS/grantcheck"
chmod +x "$app/Contents/MacOS/grantcheck"
echo "$app"

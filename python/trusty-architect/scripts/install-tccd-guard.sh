#!/bin/bash
# install-tccd-guard.sh — install or remove the tccd deadlock guard LaunchDaemon.
# Usage: sudo scripts/install-tccd-guard.sh [--uninstall]
set -eu
[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
LABEL=com.trusty.tccd-deadlock-guard
BIN=/usr/local/libexec/tccd-deadlock-guard.sh
PLIST=/Library/LaunchDaemons/$LABEL.plist
if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout system/$LABEL 2>/dev/null || true
  rm -f "$PLIST" "$BIN"; echo "removed $LABEL"; exit 0
fi
SRC="$(cd "$(dirname "$0")" && pwd)/tccd-deadlock-guard.sh"
install -d -o root -g wheel -m 755 /usr/local/libexec
install -o root -g wheel -m 755 "$SRC" "$BIN"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/bash</string><string>$BIN</string></array>
  <key>StartInterval</key><integer>20</integer>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
EOF
chown root:wheel "$PLIST"; chmod 644 "$PLIST"
launchctl bootout system/$LABEL 2>/dev/null || true
launchctl bootstrap system "$PLIST"
launchctl print system/$LABEL | grep -E 'state|last exit' | head -3
echo "installed $LABEL; log: /var/log/tccd-deadlock-guard.log"

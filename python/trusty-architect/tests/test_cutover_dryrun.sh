#!/usr/bin/env bash
# Test: scripts/cutover.sh --dry-run exits 0 and changes nothing; no flags exits 2.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CFG="$HOME/.trusty-mpm/config.toml"
snap() { { shasum "$CFG"; tmux list-sessions -F '#{session_name}' | sort; git -C "$ROOT" status --porcelain; ls -la "$ROOT/inbox"; } 2>&1; }
before=$(snap)
out=$("$ROOT/scripts/cutover.sh" --dry-run)
after=$(snap)
[ "$before" = "$after" ] || { echo "FAIL: dry run changed state"; exit 1; }
[ ! -e "$ROOT/inbox/cutover.log" ] || { echo "FAIL: dry run wrote the log"; exit 1; }
printf '%s\n' "$out" | grep -q 'B7' || { echo "FAIL: dry run did not reach B7"; exit 1; }
rc=0; "$ROOT/scripts/cutover.sh" >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || { echo "FAIL: no flags exited $rc, want 2"; exit 1; }
echo "PASS"

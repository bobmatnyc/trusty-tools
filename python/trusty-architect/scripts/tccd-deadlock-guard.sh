#!/bin/bash
# tccd-deadlock-guard.sh — break a WindowServer/tccd deadlock loop by restarting tccd.
#
# Why: on macOS 26.6.2 WindowServer's main thread can block in a synchronous
# Screen Recording TCC preflight (kTCCServiceScreenCapture) whose reply from the
# system tccd never arrives. The watchdog kills WindowServer after 40 s, but tccd
# stays wedged, so every relaunched WindowServer hangs on the same request and is
# killed again about once a minute until a reboot (2026-09-24: 13:22-13:40Z).
# Stackshots for 09-18, 09-23 and 09-24 show the same chain; see
# local/actions.md and the supervisor's crash write-up.
# What: run by launchd as root every 20 s. It restarts tccd (launchd relaunches it
# on demand) when either
#   (a) a WindowServer watchdog report newer than the last check names a TCC
#       preflight on its main thread, or
#   (b) two or more WindowServer reports were written in the last 180 s.
# At most one restart per 300 s. Every action goes to /var/log/tccd-deadlock-guard.log.
# Install: sudo scripts/install-tccd-guard.sh (copies this file to a root-owned path).
set -u
# The env overrides exist only for testing without root.
REPORTS=${TCCD_GUARD_REPORTS:-/Library/Logs/DiagnosticReports}
STATE=${TCCD_GUARD_STATE:-/var/db/tccd-deadlock-guard}
LOG=${TCCD_GUARD_LOG:-/var/log/tccd-deadlock-guard.log}
KILL=${TCCD_GUARD_KILL:-/usr/bin/killall -9 tccd}
mkdir -p "$STATE"
now=$(date +%s)
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" >> "$LOG"; }

last_check=$(cat "$STATE/last_check" 2>/dev/null || echo "$now")
echo "$now" > "$STATE/last_check"
last_kill=$(cat "$STATE/last_kill" 2>/dev/null || echo 0)

reason=""
recent=0
for f in "$REPORTS"/WindowServer-*.ips; do
  [ -e "$f" ] || continue
  m=$(stat -f %m "$f")
  [ $((now - m)) -le 180 ] && recent=$((recent + 1))
  if [ "$m" -gt "$last_check" ] && grep -q -E 'tcc(\.auth)?\.preflight\.kTCCService' "$f"; then
    reason="new watchdog report with a TCC preflight on the main thread: $(basename "$f")"
  fi
done
[ -z "$reason" ] && [ "$recent" -ge 2 ] && reason="$recent WindowServer reports in the last 180 s"
[ -z "$reason" ] && exit 0

if [ $((now - last_kill)) -lt 300 ]; then
  log "SKIP ($reason): tccd restarted $((now - last_kill)) s ago"
  exit 0
fi
log "RESTART tccd: $reason; tccd pids: $(pgrep -x tccd | tr '\n' ' ')"
$KILL
echo "$now" > "$STATE/last_kill"

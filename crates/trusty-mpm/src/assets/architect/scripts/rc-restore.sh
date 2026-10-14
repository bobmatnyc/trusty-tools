#!/usr/bin/env bash
# rc-restore.sh — re-enable Remote Control and the session name on live PM panes.
#
# Why: a Claude account change (/login in any session) drops Remote Control, and
# every PM pane's status line then shows `/rc failed`. The fix is per pane:
# /rename <session>, then /remote-control <session> (names the RC session). fleet-poll.py runs this on its own
# when it sees the marker (the owner, 2026-10-02 17:27Z: "This should be automatic").
# What: per session, skips (and logs why) a pane that is at a shell, busy, or
# holds typed input; else types /rename <session>, then rc_connect (rc-lib.sh): /remote-control <session>,
# disconnect-and-retype if RC was already active, accept a `❯ Continue` prompt,
# confirm a https://claude.ai/code/session_... URL that differs from the earlier one.
# Each command is typed, echo-verified by input-state.py, then submitted with a
# separate Enter (same pattern as session-relaunch.sh).
# Exit: 0 all restored; 1 any session failed; 2 none failed but some skipped.
# Stdout: one `<session> restored <url>`, `<session> skipped <why>` or
# `<session> failed <why>` line per session (fleet-poll.py parses it).
# Test: scripts/test_fleet_poll_rc.py covers the poller side; this script is bash -n checked.
# Usage: scripts/rc-restore.sh <session>... [log via RC_RESTORE_LOG]
set -u
[ $# -ge 1 ] || { echo "usage: $0 <session>..." >&2; exit 1; }
SUPERVISOR_DIR="$(cd "$(dirname "$0")/.." && pwd)"
L="${RC_RESTORE_LOG:-$SUPERVISOR_DIR/inbox/relaunch.log}"
mkdir -p "$(dirname "$L")"
IS="$SUPERVISOR_DIR/scripts/input-state.py"
s=""; t=""
log() { printf '%s %s %s\n' "$(date -u +%H:%M:%SZ)" "$s" "$*" >> "$L"; }
# shellcheck source=rc-lib.sh
. "$SUPERVISOR_DIR/scripts/rc-lib.sh"

# Prints one result line; returns 0 restored, 1 failed, 2 skipped.
restore_one() {
  s="$1"; t="=$s:"
  if ! tmux has-session -t "=$s" 2>/dev/null; then
    log "rc-restore FAILED: no such session"; echo "$s failed no such session"; return 1
  fi
  local cmd st cap
  cmd=$(tmux display -p -t "$t" '#{pane_current_command}')
  case "$cmd" in
    bash|zsh|sh|fish|dash|ksh|-*) log "rc-restore SKIP: foreground is $cmd"
      echo "$s skipped foreground is $cmd"; return 2 ;;
  esac
  cap=$(tmux capture-pane -t "$t" -p)
  if printf '%s\n' "$cap" | grep -q 'esc to interrupt'; then
    log "rc-restore SKIP: busy"; echo "$s skipped busy"; return 2
  fi
  st=$(python3 "$IS" "$t" | awk '{print $2}')
  case "$st" in
    empty|suggestion) ;;
    *) log "rc-restore SKIP: input=$st"; echo "$s skipped input=$st"; return 2 ;;
  esac
  rc_send_cmd "/rename $s" || { echo "$s failed /rename not echoed"; return 1; }
  log "sent /rename $s"; sleep 3
  tmux rename-window -t "=$s:" "$s" 2>/dev/null
  if ! rc_connect; then
    log "rc: NOT-CONFIRMED ($RC_FAIL)"; echo "$s failed $RC_FAIL"; return 1
  fi
  log "rc: $RC_URL"; echo "$s restored $RC_URL"; return 0
}

failed=0; skipped=0
for name in "$@"; do
  restore_one "$name"
  case $? in 0) ;; 2) skipped=1 ;; *) failed=1 ;; esac
done
[ $failed = 1 ] && exit 1
[ $skipped = 1 ] && exit 2
exit 0

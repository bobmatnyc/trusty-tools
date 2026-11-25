#!/usr/bin/env bash
# rc-disconnect.sh <session> — turn Remote Control off for one live Claude pane, without killing it.
# Why: RC on tm-architect-questions follows the open interactive questions (questions_rc.py; see #10571).
# What: skips unless RC is connected and the input box is empty/suggestion and no turn runs (input-state.py gate);
#   types /remote-control (echo-verified, rc-lib.sh), which opens the "already active" dialog, moves the
#   selection to "Disconnect this session" and presses Enter. Esc closes the dialog if it cannot be found.
# Exit: 0 disconnected or already off; 2 skipped (busy, typed input, no session); 1 failed.
# Test: bash -n in tests/test_shell_parse.py; the RC-follows-questions decision logic and its test ship later (S7).
set -u
s="${1:?usage: $0 <session>}"; t="=$s:"
SUPERVISOR_DIR="$(cd "$(dirname "$0")/.." && pwd)"
L="${RC_RESTORE_LOG:-$SUPERVISOR_DIR/inbox/relaunch.log}"
log() { printf '%s %s %s\n' "$(date -u +%H:%M:%SZ)" "$s" "$*" >> "$L"; }
# shellcheck source=rc-lib.sh
. "$SUPERVISOR_DIR/scripts/rc-lib.sh"
tmux has-session -t "=$s" 2>/dev/null || { echo "$s skipped no such session"; exit 2; }
_rc_active || { echo "$s disconnected already off"; exit 0; }
case "$(_rc_box | cut -d' ' -f1)" in empty|suggestion) ;; *) log "rc-disconnect SKIP: input busy/typed"; echo "$s skipped input busy"; exit 2 ;; esac
rc_send_cmd "/remote-control" || { echo "$s failed /remote-control not echoed"; exit 1; }
sleep 3
for _ in 1 2 3 4 5; do
  _rc_dialog || { sleep 1; continue; }
  if _rc_cap | grep -q '❯.*Disconnect this session'; then
    _rc_tx send-keys Enter; sleep 3
    if _rc_active; then log "rc-disconnect FAILED: still active"; echo "$s failed still active"; exit 1; fi
    log "rc-disconnect: RC off"; echo "$s disconnected"; exit 0
  fi
  _rc_tx send-keys Down; sleep 1
done
_rc_tx send-keys Escape
log "rc-disconnect FAILED: no Disconnect option"; echo "$s failed no disconnect option"; exit 1

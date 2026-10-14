#!/usr/bin/env bash
# session-relaunch.sh — restart one PM pane under the current Claude login.
#
# Why: a running Claude Code session keeps the login it started with, and
# Remote Control disconnects when the account changes. Restarting per pane
# leaves every other session (and its running agents) untouched, unlike
# fleet-restart.sh, which kills the whole tmux server.
# What: /exit → wait for the pane's shell → `tm` (relaunches that session in
# place) → /rename <session> → /remote-control <session> → /tm-session-resume. Logs one line per step.
# Precondition: the PM was checkpointed with /tm-session-pause and the
# snapshot verified. The input box must be empty or a suggestion.
# If Claude asks "Background work is running", the script stops unless
# --stop-tasks is given, which picks "Exit and stop tasks".
# Usage: scripts/session-relaunch.sh [--stop-tasks] <session> [log]
set -u
STOP=0
if [ "${1:-}" = "--stop-tasks" ]; then STOP=1; shift; fi
s="$1"; t="=$s:"
SUPERVISOR_DIR="$(cd "$(dirname "$0")/.." && pwd)"
L="${2:-$SUPERVISOR_DIR/inbox/relaunch.log}"
mkdir -p "$(dirname "$L")"
log() { printf '%s %s %s\n' "$(date -u +%H:%M:%SZ)" "$s" "$*" >> "$L"; }
# After a reboot tm recreates the tmux sessions at a bare shell: skip /exit and launch.
if [ "$(tmux display -p -t "$t" '#{pane_current_command}')" = zsh ]; then
  log "already at shell"
else
st=$(python3 "$SUPERVISOR_DIR/scripts/input-state.py" "$t" | awk '{print $2}')
case "$st" in empty|suggestion) ;; *) log "SKIP input=$st"; exit 1 ;; esac
tmux send-keys -t "$t" -l '/exit'; sleep 1; tmux send-keys -t "$t" Enter
ok=0
for i in $(seq 1 20); do
  sleep 2
  [ "$(tmux display -p -t "$t" '#{pane_current_command}')" = zsh ] && { ok=1; break; }
  if tmux capture-pane -t "$t" -p | grep -q '❯ 1. Exit and stop tasks'; then
    if [ $STOP = 1 ]; then tmux send-keys -t "$t" Enter
    else log "STOPPED: background work running; rerun with --stop-tasks or Esc"; exit 1; fi
  fi
done
[ $ok = 1 ] || { log "EXIT-FAILED: $(tmux capture-pane -t "$t" -p | grep -v '^\s*$' | tail -3 | tr '\n' '|')"; exit 1; }
log "exited"
fi
tmux send-keys -t "$t" -l 'tm'; tmux send-keys -t "$t" Enter
ok=0
for i in $(seq 1 40); do
  sleep 3
  c=$(tmux capture-pane -t "$t" -p)
  # tm may restore the prior conversation, which scrolls the 'Claude Code' banner away:
  # a non-shell foreground process with a prompt line counts as launched.
  [ "$(tmux display -p -t "$t" '#{pane_current_command}')" != zsh ] && echo "$c" | grep -q '^❯' && { ok=1; break; }
done
[ $ok = 1 ] || { log "LAUNCH-TIMEOUT"; exit 1; }
sleep 3; log "launched"
# A restored conversation keeps the TUI busy for up to a minute and it drops or
# queues keystrokes meanwhile: type once, wait until the prompt echoes the command,
# and only then press Enter.
# shellcheck source=rc-lib.sh
. "$SUPERVISOR_DIR/scripts/rc-lib.sh"
RC_ECHO_TRIES=40; RC_ECHO_SLEEP=2
send_cmd() { rc_send_cmd "$@"; }
send_cmd "/rename $s" || exit 1; log "sent /rename $s"; sleep 3
tmux rename-window -t "=$s:" "$s" 2>/dev/null
rc_connect && log "rc: $RC_URL" || log "rc: NOT-CONFIRMED ($RC_FAIL)"
send_cmd /tm-session-resume || exit 1
log "sent /tm-session-resume"

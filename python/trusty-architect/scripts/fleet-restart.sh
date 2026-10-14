#!/usr/bin/env bash
# fleet-restart.sh — restart the fleet tmux server at normal priority.
#
# Why: a tmux server started under launchd ProcessType Background runs at
# Darwin priority 4, and every pane inherits it (example-org/trusty-tools#8415).
# The clamp cannot be cleared at runtime; only a new server started from a
# normal-priority context avoids it. The trusty-mpm daemon runs at normal
# priority, so sessions restarted through `tm sessions resume` get a clean
# server.
# What: kills the tmux server, resumes each named session through the daemon,
# waits for each Claude prompt, sends /tm-session-resume, and logs priorities.
# Precondition: every session was checkpointed with /tm-session-pause and the
# snapshot verified. Run it detached (nohup), because it kills its caller.
# kill-server takes down EVERY session on the socket, so the script refuses to
# run while a session outside its list is live (2026-09-23: an unlisted PM was
# lost uncheckpointed). Pass --force first to override after checkpointing it.
# --rc sends /rename <session> then /remote-control <session> (rc-lib.sh) before /tm-session-resume, for a restart after a
# Claude account change (Remote Control disconnects on the account switch).
# --wait-server SECONDS: after kill-server, do NOT let the daemon start the new
# tmux server (a launchd context is not exempt from the macOS Gatekeeper
# Developer Tools exemption). Poll every 2 s for a server the user starts from an
# exempted terminal (iTerm2) on the same socket, with at least one session. If
# none appears within SECONDS, abort without resuming (exit 1), so the daemon
# never creates the server. Default off.
# Usage: nohup scripts/fleet-restart.sh [--force] [--rc] [--wait-server SECONDS] tm-a tm-b ... tm-architect &
set -u
FORCE=0; RC=0; WAIT=0
while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1; shift ;;
    --rc) RC=1; shift ;;
    --wait-server)
      case "${2:-}" in
        ''|*[!0-9]*) echo "fleet-restart: --wait-server needs a whole number of seconds" >&2; exit 2 ;;
      esac
      WAIT="$2"; shift 2 ;;
    *) break ;;
  esac
done
SUPERVISOR_DIR="$(cd "$(dirname "$0")/.." && pwd)"
S="${TMUX_SOCKET:-$(tmux display-message -p '#{socket_path}' 2>/dev/null)}"
S="${S:-/tmp/tmux-$(id -u)/default}"
mkdir -p "$SUPERVISOR_DIR/inbox"
LOG="$SUPERVISOR_DIR/inbox/restart.log"
log() { printf '%s %s\n' "$(date -u +%H:%M:%SZ)" "$*" >> "$LOG"; }
trap '' HUP
export TMUX_SOCKET="$S"
s=""; t=""
. "$SUPERVISOR_DIR/scripts/rc-lib.sh"
# Fail closed: if the socket cannot be listed, nothing is known about what
# kill-server would take down.
if ! live="$(tmux -S "$S" list-sessions -F '#{session_name}' 2>>"$LOG")" || [ -z "$live" ]; then
  log "REFUSED: cannot list sessions on $S"
  exit 1
fi
unlisted=""
for l in $live; do
  case " $* " in *" $l "*) ;; *) unlisted="$unlisted $l" ;; esac
done
if [ -n "$unlisted" ] && [ $FORCE = 0 ]; then
  log "REFUSED: live sessions not in the list:$unlisted (checkpoint them, add them, or pass --force)"
  exit 1
fi
sleep 8
log "start; socket: $S; sessions: $*; unlisted killed:${unlisted:- none}"
log "old server pri: $(ps -o pri= -p "$(pgrep -f 'tmux new-session -A -d' | head -1)" 2>/dev/null)"
tmux -S "$S" kill-server 2>>"$LOG"; log "kill-server rc=$?"
sleep 3
if [ "$WAIT" -gt 0 ]; then
  log "waiting for an externally started tmux server on $S"
  waited=0; found=0
  while [ "$waited" -lt "$WAIT" ]; do
    if [ -n "$(tmux -S "$S" list-sessions -F '#{session_name}' 2>/dev/null)" ]; then found=1; break; fi
    sleep 2; waited=$((waited + 2))
  done
  if [ $found = 0 ]; then
    log "no external server after ${WAIT}s; aborting resume"
    exit 1
  fi
  xpid="$(tmux -S "$S" display-message -p '#{pid}' 2>/dev/null)"
  log "external server found after ${waited}s: pid=$xpid pri=$(ps -o pri= -p "$xpid" 2>/dev/null)"
fi
for s in "$@"; do
  out="$(tm sessions resume "$s" 2>&1 | tail -1)"; log "resume $s: $out"
  sleep 4
done
for s in "$@"; do
  ok=0
  for i in $(seq 1 60); do
    if tmux -S "$S" capture-pane -t "=$s:" -p 2>/dev/null | grep -q 'Claude Code'; then
      if tmux -S "$S" capture-pane -t "=$s:" -p | grep -q $'❯'; then ok=1; break; fi
    fi
    sleep 3
  done
  if [ $ok = 1 ]; then
    sleep 3
    if [ $RC = 1 ]; then
      t="=$s:"
      rc_send_cmd "/rename $s" && log "sent /rename $s"
      sleep 3
      tmux -S "$S" rename-window -t "=$s:" "$s" 2>/dev/null
      if rc_connect; then log "rc: $RC_URL"; else log "rc: NOT-CONFIRMED ($RC_FAIL)"; fi
    fi
    tmux -S "$S" send-keys -t "=$s:" -l "/tm-session-resume"; sleep 1
    tmux -S "$S" send-keys -t "=$s:" Enter
    log "sent /tm-session-resume to $s"
  else
    log "TIMEOUT waiting for Claude prompt in $s; nothing sent"
  fi
done
spid="$(tmux -S "$S" display-message -p '#{pid}' 2>/dev/null)"
log "new server pid=$spid pri=$(ps -o pri= -p "$spid" 2>/dev/null)"
for s in "$@"; do
  pp="$(tmux -S "$S" display-message -p -t "=$s:" '#{pane_pid}' 2>/dev/null)"
  log "$s pane_pid=$pp pri=$(ps -o pri= -p "$pp" 2>/dev/null)"
done
log "done"

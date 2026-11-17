#!/usr/bin/env bash
# Gated self-refresh (dry run unless --live): /clear then a short plain resume prompt in the Architect's own pane (Db8 2026-10-14: no /tm-session-resume, whose catch-up read costs context).
# Readiness gate (the owner 2026-10-19: the IL alone is the handoff): refuses before any key is sent unless `datg.py --project local/il ready` exits 0, checked before and again after the idle wait (#10599).
# Idle = pane not turn-active AND transcript idle (architect-idle.py) AND empty box (3 checks, 5 s apart) immediately before each step.
# #10599: the Architect dir is the one this script sits in, with no env override, so it runs only the scripts beside it and pm-guard can judge them.
# Usage: self-clear.sh [--live] [--locked] [--socket PATH] [--target PANE]. The poller passes the socket and pane it
# checked, and --locked: it holds an flock on inbox/self-clear.pid for this whole run (fleet_poll_selfrefresh.py).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
S=${ARCHITECT_SESSION:-tm-architect}
LIVE=0 LOCKED=0 SOCK="" T=""  # dry run is the default; only --live sends keys
while [ $# -gt 0 ]; do case "$1" in
  --live) LIVE=1;; --locked) LOCKED=1;; --socket|--target) [ -n "${2:-}" ] || exit 64; [ "$1" = --socket ] && SOCK=$2 || T=$2; shift;;
  *) echo "usage: self-clear.sh [--live] [--locked] [--socket PATH] [--target PANE]" >&2; exit 64;; esac; shift; done
log(){ echo "$(date -u +%H:%M:%SZ) self-refresh $1" | tee -a "$ROOT/inbox/self-clear.log"; }
t=${T:-"=$S:"}
# #10628: the target is a %N pane id (the poller's --target) or the exact =session: default. Anything else is refused,
# so the one tmux -t below never prefix-matches another session (#8443).
[[ $t =~ ^%[0-9]+$ || $t =~ ^=.+:$ ]] || { echo "self-clear.sh: target '$t' is not a %N pane id or =session:" >&2; exit 64; }
tx(){ local verb=$1; shift; tmux -S "$SOCK" "$verb" -t "$t" "$@"; }
# #10599: pane ids are reissued after a tmux server restart, so the pane must still be in $S before every key
# and at the start of every idle wait (the poller's send_to_architect checks the same way). Else: send nothing.
own(){ local s; s=$(tx display-message -p '#{session_name}' 2>/dev/null) && [ "$s" = "$S" ] && return 0
  log "pane $t is in session '${s:-unknown}', not $S; nothing sent"; exit 6; }
# idle [fresh]: with "fresh" (after /clear) skip architect-idle.py, which reports unknown on a transcript holding only /clear records (2026-10-21 RCA); pane not turn-active + empty/suggestion box still gate the send.
idle(){ local ok=0 i ta fresh=$1; own; for i in $(seq 1 360); do
  cap=$(tx capture-pane -e -p -S -80); box=$(python3 "$ROOT/scripts/input-state.py" "$t" | awk '{print $2}')
  # #10599: turn-active.py exits 0 running, 10 idle, 3 internal error. Only 10 is idle. A missing helper or any other code is busy.
  python3 "$ROOT/scripts/turn-active.py" <<<"$cap"
  ta=$?
  if [ "$ta" = 10 ] && { [ "$fresh" = fresh ] || python3 "$ROOT/scripts/architect-idle.py" >/dev/null; } && { [ "$box" = empty ] || [ "$box" = suggestion ]; }; then ok=$((ok+1)); else ok=0; fi
  [ $ok -ge 3 ] && return 0; sleep "${SELF_CLEAR_POLL:-5}"; done; return 1; }
send(){ own; tx send-keys -l "$1"; sleep "${SELF_CLEAR_ECHO:-1.5}"
  # The slash-command menu appends its description after the typed text, and input-state shows only the first
  # pane row (cut at 100 chars), so match the first 60 characters as a prefix.
  local p="${1:0:60}"
  case "$(python3 "$ROOT/scripts/input-state.py" "$t" | cut -d' ' -f2-)" in "typed | $p"*) ;; *) log "echo mismatch for $p"; return 1;; esac
  own; tx send-keys Enter; }
# #10599: the one readiness check. Only exit 0 permits a clear. 1 not ready, 2 IL missing or unreadable, any other code refuses.
il_ready(){ python3 "$ROOT/scripts/datg.py" --project "$ROOT/local/il" ready; }
il_ok(){ local out rc line; out=$(il_ready 2>&1); rc=$?
  while IFS= read -r line; do [ -n "$line" ] && log "datg ready: $line"; done <<<"$out"
  [ "$rc" = 0 ]; }
il_ok || { log "IL not ready; update the Architect IL, nothing sent"; exit 4; }
# #10599: one tmux server and one pane for every step: capture, input-state, send-keys. No bare tmux call.
# No --socket: TMUX_SOCKET, else the server in $TMUX, else tmux's default socket. Not a live socket: refuse.
[ -n "$SOCK" ] || SOCK=${TMUX_SOCKET:-${TMUX%%,*}}
[ -n "$SOCK" ] || SOCK="$(cd -P "${TMUX_TMPDIR:-/tmp}" 2>/dev/null && pwd)/tmux-$(id -u)/default"
[ -S "$SOCK" ] || { log "no tmux socket at $SOCK; nothing sent"; exit 5; }
export TMUX_SOCKET="$SOCK"  # input-state.py reads the server from TMUX_SOCKET
[ $LIVE = 1 ] || { echo "dry run (pass --live to clear): IL ready; would wait for idle in $t on $SOCK, check the IL again, send /clear, wait for idle, send the resume prompt"; exit 0; }
# #10599: a run by hand takes no lock; it refuses while the poller's run holds inbox/self-clear.pid, or when that
# cannot be checked (lock-state exits 0 only when free).
if [ "$LOCKED" != 1 ]; then python3 "$ROOT/scripts/fleet_poll_selfrefresh.py" lock-state >/dev/null
  [ $? = 0 ] || { log "another self-refresh holds the lock, or it cannot be checked; nothing sent"; exit 7; }; fi
idle || { log "never idle; nothing sent"; exit 2; }
il_ok || { log "IL no longer ready after the idle wait; nothing sent"; exit 4; }
send "/clear" || exit 1; log "sent /clear"; sleep "${SELF_CLEAR_SETTLE:-6}"
idle fresh || { log "not idle after clear"; exit 3; }
RESUME="Resume from the Architect IL: run \`python3 scripts/datg.py --project local/il digest\` and \`python3 scripts/dashboard-db.py summary\`; do not run session_context_catchup."
send "$RESUME" || exit 1; log "sent resume prompt"

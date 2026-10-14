#!/usr/bin/env bash
# Gated self-refresh: /clear then /tm-session-resume in the Architect's own pane.
# Idle = pane not turn-active AND transcript idle (architect-idle.py) AND empty box (3 checks, 5 s apart) immediately before each step.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; t="=tm-architect:"
log(){ echo "$(date -u +%H:%M:%SZ) self-refresh $1" | tee -a "$ROOT/inbox/self-clear.log"; }
idle(){ local ok=0 i; for i in $(seq 1 360); do
  cap=$(tmux capture-pane -t "$t" -e -p -S -80); box=$(python3 "$ROOT/scripts/input-state.py" "$t" | awk '{print $2}')
  if ! python3 "$ROOT/scripts/turn-active.py" <<<"$cap" && python3 "$ROOT/scripts/architect-idle.py" >/dev/null && { [ "$box" = empty ] || [ "$box" = suggestion ]; }; then ok=$((ok+1)); else ok=0; fi
  [ $ok -ge 3 ] && return 0; sleep 5; done; return 1; }
send(){ tmux send-keys -t "$t" -l "$1"; sleep 1.5
  # The slash-command menu appends its description after the typed text, so match the prefix.
  case "$(python3 "$ROOT/scripts/input-state.py" "$t" | cut -d' ' -f2-)" in "typed | $1"|"typed | $1 "*) ;; *) log "echo mismatch for $1"; return 1;; esac
  tmux send-keys -t "$t" Enter; }
idle || { log "never idle; nothing sent"; exit 2; }
send "/clear" || exit 1; log "sent /clear"; sleep 6
idle || { log "not idle after clear"; exit 3; }
send "/tm-session-resume" || exit 1; log "sent /tm-session-resume"

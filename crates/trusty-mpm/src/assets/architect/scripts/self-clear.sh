#!/usr/bin/env bash
# Gated self-refresh (dry run unless --live): /clear then a short plain resume prompt in the Architect's own pane (Db8 2026-10-14: no /tm-session-resume, whose catch-up read costs context).
# Freshness gate (the owner 2026-10-19: the IL alone is the handoff): refuses before any key is sent unless `datg.py --project local/il fresh` exits 0 and the digest SESSION block is recorded.
# Idle = pane not turn-active AND transcript idle (architect-idle.py) AND empty box (3 checks, 5 s apart) immediately before each step.
# #10599: the Architect dir is the one this script sits in, with no env override, so it runs only the scripts beside it and pm-guard can judge them.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
S=${ARCHITECT_SESSION:-tm-architect}; t="=$S:"
LIVE=0; [ "$1" = "--live" ] && LIVE=1  # dry run is the default; only --live sends keys
log(){ echo "$(date -u +%H:%M:%SZ) self-refresh $1" | tee -a "$ROOT/inbox/self-clear.log"; }
# idle [fresh]: with "fresh" (after /clear) skip architect-idle.py, which reports unknown on a transcript holding only /clear records (2026-10-21 RCA); pane not turn-active + empty/suggestion box still gate the send.
idle(){ local ok=0 i ta fresh=$1; for i in $(seq 1 360); do
  cap=$(tmux capture-pane -t "=$S:" -e -p -S -80); box=$(python3 "$ROOT/scripts/input-state.py" "=$S:" | awk '{print $2}')
  # #10599: only exit 1 from turn-active.py (not active) is idle. A missing helper (python3 exits 2) or any other code is busy.
  python3 "$ROOT/scripts/turn-active.py" <<<"$cap"
  ta=$?
  if [ "$ta" = 1 ] && { [ "$fresh" = fresh ] || python3 "$ROOT/scripts/architect-idle.py" >/dev/null; } && { [ "$box" = empty ] || [ "$box" = suggestion ]; }; then ok=$((ok+1)); else ok=0; fi
  [ $ok -ge 3 ] && return 0; sleep "${SELF_CLEAR_POLL:-5}"; done; return 1; }
send(){ tmux send-keys -t "=$S:" -l "$1"; sleep "${SELF_CLEAR_ECHO:-1.5}"
  # The slash-command menu appends its description after the typed text, and input-state shows only the first
  # pane row (cut at 100 chars), so match the first 60 characters as a prefix.
  local p="${1:0:60}"
  case "$(python3 "$ROOT/scripts/input-state.py" "=$S:" | cut -d' ' -f2-)" in "typed | $p"*) ;; *) log "echo mismatch for $p"; return 1;; esac
  tmux send-keys -t "=$S:" Enter; }
il_ok(){ local d; python3 "$ROOT/scripts/datg.py" --project "$ROOT/local/il" fresh >/dev/null 2>&1 || return 1
  d=$(python3 "$ROOT/scripts/datg.py" --project "$ROOT/local/il" digest 2>/dev/null) || return 1
  [ -n "$d" ] && ! grep -q "^  none recorded" <<<"$d"; }
il_ok || { log "IL stale or SESSION not recorded; update the Architect IL, nothing sent"; exit 4; }
[ $LIVE = 1 ] || { echo "dry run (pass --live to clear): IL fresh and SESSION recorded; would wait for idle in $t, send /clear, wait for idle, send the resume prompt"; exit 0; }
idle || { log "never idle; nothing sent"; exit 2; }
send "/clear" || exit 1; log "sent /clear"; sleep "${SELF_CLEAR_SETTLE:-6}"
idle fresh || { log "not idle after clear"; exit 3; }
RESUME="Resume from the Architect IL: run \`python3 scripts/datg.py --project local/il digest\` and \`python3 scripts/dashboard-db.py summary\`; do not run session_context_catchup."
send "$RESUME" || exit 1; log "sent resume prompt"

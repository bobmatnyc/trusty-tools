#!/bin/sh
# datg-hook.sh: SessionStart and PreToolUse(Agent|Task) hook for a DATG pilot PM (the owner D93, 2026-10-09).
# Why: the PM must see its goal rules at session start and be warned (later blocked) when a
# dispatch names no goal id. Never writes state; a warn-mode warning is appended to datg/log.jsonl (kind hook_warning).
# What: passes the hook JSON on stdin to `datg.py hook`; prints its JSON or nothing. Every
# failure path exits 0 with no output (fail open); the block is a documented permissionDecision.
# SPEC-IL-01: also serves Stop (datg.py stamp -> stale_il log) and injects the digest at SessionStart. The exported
# DATG_SESSION selects datg/<session>/ when that per-PM directory exists; otherwise the legacy datg/ layout is used.
# Session filter (T08 steer, 2026-10-11): when a session name is given (arg 1 or $DATG_SESSION), exit 0 with
# no output unless the tmux session of $TMUX_PANE is exactly that name. Needed where one project dir is
# shared by several PM sessions (trusty-tools: tm-dogfood is the pilot). No name given: unfiltered.
WANT="${1:-$DATG_SESSION}"
if [ -n "$WANT" ]; then
  [ -n "$TMUX_PANE" ] || exit 0
  TMUX_BIN=$(command -v tmux 2>/dev/null || echo /opt/homebrew/bin/tmux)
  S=$("$TMUX_BIN" display-message -p -t "$TMUX_PANE" '#{session_name}' 2>/dev/null) || exit 0
  [ "$S" = "$WANT" ] || exit 0
  export DATG_SESSION="$WANT"  # datg.py logs warn-mode warnings under this session name
fi
R=$(cd "$(dirname "$0")/.." && pwd)
python3 "$R/scripts/datg.py" hook 2>/dev/null
exit 0

#!/usr/bin/env bash
# session-relaunch.sh — restart one PM pane under the current Claude login.
#
# Why: a running Claude Code session keeps the login it started with, and
# Remote Control disconnects when the account changes. Restarting per pane
# leaves every other session (and its running agents) untouched, unlike
# fleet-restart.sh, which kills the whole tmux server.
# What: /exit → wait for the pane's shell → `tm` (relaunches that session in
# place) → /rename <session> → [/remote-control <session>, only with --rc] → /tm-session-resume. Logs one line per step.
# --rc: also connect Remote Control. Off by default (owner, 2026-11-12 18:41Z: not automatic for PMs).
# Precondition: the PM was checkpointed with /tm-session-pause and the
# snapshot verified; with --il the checkpoint is a fresh Intent Ledger digest
# verified by the Architect, and the script refuses (IL-MISSING, exit 1, before
# /exit) unless <project-dir>/.trusty-mpm/datg/state.json exists or exactly one
# <project-dir>/.trusty-mpm/dbatg/<uuid>/state.json does (dbatg layout: the brief says
# to read the digest with the dbatg MCP tool; several uuids: IL-AMBIGUOUS, exit 1).
# The input box must be empty or a suggestion.
# If Claude asks "Background work is running", the script stops unless
# --stop-tasks is given, which picks "Exit and stop tasks".
# --il <project-dir>: skip /tm-session-resume; sync the one shared brief
# local/briefs/relaunch-resume.md (override dir: RELAUNCH_BRIEF_DIR; rewritten only when its
# text changes, no per-session file) and send "[<Name> Architect HH:MMZ] Architect answer: <text>" (<Name> is
# owner_name in local/architect.json, read before /exit; an unloadable config is IL-CONFIG, exit 1; none: "[Architect ...]"),
# the brief's text with this session's ledger filled in (scripts/relaunch_brief.py render).
# input-state.py reports at most 100 characters of the box, so send_long verifies a prefix
# of 40+ characters before Enter.
# --il-session <name> (only with --il): the PM keeps a per-session ledger in a shared project
# dir. IL-MISSING (exit 1, before /exit) unless <project-dir>/.trusty-mpm/datg/<name>/state.json
# or, failing that, <project-dir>/.trusty-mpm/dbatg/<name>/state.json exists. The datg text
# then names `datg.py --project <dir> --session <name> digest`; the dbatg text names that folder.
# Usage: [SNAP=<snapshot path>] scripts/session-relaunch.sh [--stop-tasks] [--rc] [--il <project-dir> [--il-session <name>]] <session> [log]
# SNAP resumes that exact snapshot (the session's own), never the latest overall.
# Test: tests/test_session_relaunch_il.py (fake tmux), tests/test_relaunch_brief.py.
set -u
STOP=0; IL=""; ILS=""; RC=0
while :; do
  case "${1:-}" in
    --stop-tasks) STOP=1; shift ;;
    --rc) RC=1; shift ;;
    --il) [ -n "${2:-}" ] || { echo "usage: --il needs <project-dir>" >&2; exit 2; }; IL="$2"; shift 2 ;;
    --il-session) [ -n "${2:-}" ] || { echo "usage: --il-session needs <name>" >&2; exit 2; }; ILS="$2"; shift 2 ;;
    *) break ;;
  esac
done
[ -z "$ILS" ] || [ -n "$IL" ] || { echo "usage: --il-session needs --il" >&2; exit 2; }
case "$ILS" in */*|..) echo "usage: --il-session name must not contain '/'" >&2; exit 2 ;; esac
s="$1"; t="=$s:"
SUPERVISOR_DIR="$(cd "$(dirname "$0")/.." && pwd)"
L="${2:-$SUPERVISOR_DIR/inbox/relaunch.log}"
mkdir -p "$(dirname "$L")"
log() { printf '%s %s %s\n' "$(date -u +%H:%M:%SZ)" "$s" "$*" >> "$L"; }
# --il: refuse before /exit when there is no ledger, then write the resume brief.
BRIEF=""
if [ -n "$IL" ]; then
  # Two ledger layouts: datg (<project>/.trusty-mpm/datg/state.json, read with datg.py)
  # and dbatg (<project>/.trusty-mpm/dbatg/<uuid>/state.json, read with the dbatg MCP tool).
  [ -d "$IL" ] || { log "IL-MISSING $IL"; exit 1; }
  DBATG_DIR=""; SESS_ARG=""
  if [ -n "$ILS" ]; then
    # Per-session ledger (shared project dir): datg/<name>/ first, then dbatg/<name>/.
    if [ -f "$IL/.trusty-mpm/datg/$ILS/state.json" ]; then SESS_ARG=" --session $ILS"
    elif [ -f "$IL/.trusty-mpm/dbatg/$ILS/state.json" ]; then DBATG_DIR="$IL/.trusty-mpm/dbatg/$ILS"
    else log "IL-MISSING $IL (session $ILS)"; exit 1; fi
  elif [ ! -f "$IL/.trusty-mpm/datg/state.json" ]; then
    n=0
    for f in "$IL"/.trusty-mpm/dbatg/*/state.json; do
      [ -f "$f" ] || continue
      n=$((n+1)); DBATG_DIR="$(dirname "$f")"
    done
    [ "$n" -le 1 ] || { log "IL-AMBIGUOUS $IL ($n dbatg ledgers)"; exit 1; }
    [ "$n" = 1 ] || { log "IL-MISSING $IL"; exit 1; }
  fi
  # One brief for every session (owner, 2026-11-13: "similar briefs should be edited, not duplicated"):
  # rewritten only when its text changes; the per-session ledger is filled in at send time.
  BRIEF="${RELAUNCH_BRIEF_DIR:-$SUPERVISOR_DIR/local/briefs}/relaunch-resume.md"
  RB="$SUPERVISOR_DIR/scripts/relaunch_brief.py"
  python3 "$RB" sync "$BRIEF" || { log "IL-BRIEF-WRITE-FAILED $BRIEF"; exit 1; }
  if [ -z "$DBATG_DIR" ]; then
    RESUME=$(python3 "$RB" render datg "python3 $SUPERVISOR_DIR/scripts/datg.py --project $IL$SESS_ARG digest")
  else
    CRONS=""
    [ -f "$DBATG_DIR/cron-prompts.md" ] && CRONS="$DBATG_DIR/cron-prompts.md"
    RESUME=$(python3 "$RB" render dbatg "$DBATG_DIR" ${CRONS:+"$CRONS"})
  fi
  [ -n "$RESUME" ] || { log "IL-BRIEF-RENDER-FAILED $BRIEF"; exit 1; }
  # #10571 S2: the relay marker's name is instance data (local/architect.json owner_name), never a constant.
  OWNER=$(python3 "$SUPERVISOR_DIR/scripts/architect_config.py" owner-name 2>>"$L") || { log "IL-CONFIG local/architect.json"; exit 1; }
fi
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
# Remote Control is opt-in (owner, 2026-11-12 18:41Z): only with --rc; fleet-poll passes it for tm-architect.
if [ "$RC" = 1 ]; then rc_connect && log "rc: $RC_URL" || log "rc: NOT-CONFIRMED ($RC_FAIL)"; else log "rc: skipped (no --rc)"; fi
# Like rc_send_cmd, for a line longer than input-state.py reports (100 characters):
# the box must show a prefix of 40+ characters of the line before Enter.
# Typed in pieces of at most 300 characters, split before a space so no piece starts with "-":
# one send-keys of ~1,700 characters was taken as a paste and lost its start (architect-tmux-ops).
send_long() {
  local line="$1" i box rest="$1" piece
  while [ ${#rest} -gt 300 ]; do
    piece=${rest:0:300}; piece=${piece% *}
    [ -n "$piece" ] && [ "$piece" != "${rest:0:300}" ] || piece=${rest:0:300}
    _rc_tmux send-keys -t "$t" -l "$piece"; rest=${rest:${#piece}}; sleep 1
  done
  _rc_tmux send-keys -t "$t" -l "$rest"
  for i in $(seq 1 "$RC_ECHO_TRIES"); do
    sleep "$RC_ECHO_SLEEP"
    box=$(_rc_box); box=${box#"typed | "}
    if [ ${#box} -ge 40 ] && [ "${line#"$box"}" != "$line" ]; then _rc_tmux send-keys -t "$t" Enter; return 0; fi
  done
  box=$(_rc_box); box=${box#"typed | "}
  log "NOT-ECHOED: IL resume (input: $box)"
  [ -n "$box" ] && [ "${line#"$box"}" != "$line" ] && _rc_tmux send-keys -t "$t" C-u
  return 1
}
if [ -n "$IL" ]; then
  send_long "[${OWNER:+$OWNER }Architect $(date -u +%H:%MZ)] Architect answer: $RESUME" || exit 1
  log "sent IL resume"
else
send_cmd "/tm-session-resume${SNAP:+ $SNAP}" || exit 1
log "sent /tm-session-resume ${SNAP:-}"
fi

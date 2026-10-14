#!/usr/bin/env bash
# rc-lib.sh — shared Remote Control steps; source it, do not run it.
#
# Why: `/rename <s>` names only the local Claude session. The Remote Control
# session name (shown in the Claude app) comes from the argument of
# `/remote-control <s>`; without it the app shows a generated name such as
# "<host>-<rc-name>" (the owner, 2026-10-02 17:3xZ). When RC is
# already active, `/remote-control` opens a dialog instead, so the session must
# be disconnected first and the command typed again.
# What: rc_send_cmd types a command and submits it once input-state.py echoes it
# (exact, or followed by 2+ spaces: the slash-command menu column);
# rc_connect runs the verified manual procedure and sets RC_URL or RC_FAIL.
# Caller defines before use: s (session), t (tmux target, "=$s:"), log (one-arg
# logger). Optional: TMUX_SOCKET (also read by input-state.py),
# RC_ECHO_TRIES/RC_ECHO_SLEEP (echo wait, default 20 x 1 s),
# SUPERVISOR_DIR (repo root, derived when unset).
# Test: scripts/test_rc_lib.py drives rc_connect against a fake tmux.

: "${SUPERVISOR_DIR:=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RC_IS="$SUPERVISOR_DIR/scripts/input-state.py"
RC_URL=""; RC_FAIL=""

_rc_tmux() { tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"} "$@"; }
_rc_cap() { _rc_tmux capture-pane -t "$t" -p -J -S -200; }
_rc_url() { _rc_cap | grep -o 'https://claude.ai/code/session_[A-Za-z0-9]*' | tail -1; }
_rc_box() { python3 "$RC_IS" "$t" | cut -d' ' -f2-; }

# True when the input box shows exactly $1, or $1 followed by two or more
# spaces: the slash-command menu draws its description after the command, and
# input-state.py reports that as `typed | /cmd    Description...`. Any other
# text (a user draft) is not a match, so it is never submitted.
_rc_echoed() {
  local box; box=$(_rc_box)
  case "$box" in
    "typed | $1"|"typed | $1  "*) return 0 ;;
  esac
  return 1
}

# True when the box holds only $1 repeated (optionally with the menu column
# after it): text this script typed itself, e.g. `/rename x/rename x`.
_rc_box_is_ours() {
  local rem; rem=$(_rc_box)
  case "$rem" in "typed | "*) rem=${rem#"typed | "} ;; *) return 1 ;; esac
  rem=${rem%%  *}
  [ -n "$rem" ] || return 1
  while [ -n "$rem" ] && [ "${rem#"$1"}" != "$rem" ]; do rem=${rem#"$1"}; done
  [ -z "$rem" ]
}

# Type $1, wait for the prompt to echo it, then press Enter separately. On a
# failed echo, clear the box with C-u only when it holds nothing but our own
# text, so a retry never stacks a second copy and a user draft is never touched.
rc_send_cmd() {
  _rc_tmux send-keys -t "$t" -l "$1"
  local i
  for i in $(seq 1 "${RC_ECHO_TRIES:-20}"); do
    sleep "${RC_ECHO_SLEEP:-1}"
    _rc_echoed "$1" && { _rc_tmux send-keys -t "$t" Enter; return 0; }
  done
  log "NOT-ECHOED: $1 (input: $(_rc_box))"
  _rc_box_is_ours "$1" && _rc_tmux send-keys -t "$t" C-u
  return 1
}

# True when the capture shows the RC "already active" dialog.
_rc_dialog() {
  local c; c=$(_rc_cap)
  printf '%s\n' "$c" | grep -q 'Remote Control' &&
    printf '%s\n' "$c" | grep -q 'Disconnect this session' &&
    printf '%s\n' "$c" | grep -q 'Show QR code'
}

# Move the dialog selection to Disconnect (at most 3 Ups), then submit it.
_rc_disconnect() {
  local i
  for i in 0 1 2 3; do
    _rc_cap | grep -q '❯ Disconnect this session' && break
    [ "$i" = 3 ] && break
    _rc_tmux send-keys -t "$t" Up; sleep 1
  done
  if ! _rc_cap | grep -q '❯ Disconnect this session'; then
    _rc_tmux send-keys -t "$t" Escape
    return 1
  fi
  _rc_tmux send-keys -t "$t" Enter
  for i in $(seq 1 10); do
    sleep 1
    _rc_cap | grep -q 'Remote Control disconnected' && return 0
  done
  return 1
}

# Run `/remote-control <s>`; disconnect and retype when RC was already active.
# Sets RC_URL on success (returns 0); sets RC_FAIL and returns 1 otherwise.
rc_connect() {
  local before i
  RC_URL=""; RC_FAIL=""
  before=$(_rc_url)
  rc_send_cmd "/remote-control $s" || { RC_FAIL="/remote-control not echoed"; return 1; }
  sleep 4
  if _rc_dialog; then
    log "rc: already active, disconnecting"
    _rc_disconnect || { RC_FAIL="RC dialog: Disconnect not reached"; return 1; }
    sleep 3
    rc_send_cmd "/remote-control $s" || { RC_FAIL="/remote-control not echoed after disconnect"; return 1; }
    sleep 4
  fi
  for i in $(seq 1 8); do
    _rc_cap | grep -q '❯ Continue' && { _rc_tmux send-keys -t "$t" Enter; sleep 2; }
    RC_URL=$(_rc_url)
    [ -n "$RC_URL" ] && [ "$RC_URL" != "$before" ] && return 0
    sleep 2
  done
  RC_URL=""
  RC_FAIL="rc not confirmed (no new session URL)"
  return 1
}

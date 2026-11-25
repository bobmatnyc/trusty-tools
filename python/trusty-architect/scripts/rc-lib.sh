#!/usr/bin/env bash
# rc-lib.sh — shared Remote Control steps. A caller sources this file and never runs it.
# #10570: these comments hold no backtick and no semicolon before a verb: pm-guard reads them as commands.
#
# Why: `/rename <s>` names only the local Claude session. The Remote Control
# session name (shown in the Claude app) comes from the argument of
# `/remote-control <s>`; without it the app shows a generated name such as
# "<host>-local-woolly-dahl" (see #10571). When RC is
# already active, `/remote-control` opens a dialog instead; rc_connect then leaves
# the connection alone (see #10571) and does not disconnect it.
# What: rc_send_cmd types a command and submits it once input-state.py echoes it
# (exact, or followed by 2+ spaces: the slash-command menu column);
# rc_connect runs the verified manual procedure (no-op when RC is connected) and sets RC_URL or RC_FAIL.
# Caller defines before use: s (session), t (tmux target, "=$s:"), log (one-arg
# logger). Optional: TMUX_SOCKET (also read by input-state.py),
# RC_ECHO_TRIES/RC_ECHO_SLEEP (echo wait, default 20 x 1 s),
# SUPERVISOR_DIR (repo root, derived when unset).
# Test: scripts/test_rc_lib.py drives rc_connect against a fake tmux.

: "${SUPERVISOR_DIR:=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RC_IS="$SUPERVISOR_DIR/scripts/input-state.py"
RC_URL=""; RC_FAIL=""

_rc_tmux() { tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"} "$@"; }
# #8443: the one tmux -t in this file. $t must be a %N pane id or the exact =session: form.
# Any other form is refused before tmux runs, so no target can prefix-match another session.
_rc_tx() {
  [[ $t =~ ^%[0-9]+$ || $t =~ ^=.+:$ ]] || { echo "rc-lib.sh: target '$t' is not a %N pane id or =session:" >&2; return 64; }
  local verb=$1; shift; _rc_tmux "$verb" -t "$t" "$@"
}
_rc_cap() { _rc_tx capture-pane -p -J -S -200; }
_rc_url() { _rc_cap | grep -o 'https://claude.ai/code/session_[A-Za-z0-9]*' | tail -1; }
_rc_box() { python3 "$RC_IS" "$t" | cut -d' ' -f2-; }

# True when the input box shows exactly $1, or $1 followed by two or more
# spaces: the slash-command menu draws its description after the command, and
# input-state.py reports that as "typed | /cmd    Description...". Any other
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
  _rc_tx send-keys -l "$1"
  local i
  for i in $(seq 1 "${RC_ECHO_TRIES:-20}"); do
    sleep "${RC_ECHO_SLEEP:-1}"
    _rc_echoed "$1" && { _rc_tx send-keys Enter; return 0; }
  done
  log "NOT-ECHOED: $1 (input: $(_rc_box))"
  _rc_box_is_ours "$1" && _rc_tx send-keys C-u
  return 1
}

# True when the capture shows the RC "already active" dialog.
_rc_dialog() {
  local c; c=$(_rc_cap)
  printf '%s\n' "$c" | grep -q 'Remote Control' &&
    printf '%s\n' "$c" | grep -q 'Disconnect this session' &&
    printf '%s\n' "$c" | grep -q 'Show QR code'
}

# True when the status line (bottom of the screen) ends in `/rc`: Remote Control
# is connected. `/rc failed` ends in "failed", so it does not match.
_rc_active() { _rc_cap | tail -n 12 | grep -Eq '(^|[[:space:]|·])/rc[[:space:]]*$'; }

# Run `/remote-control <s>` unless Remote Control is already connected.
# See #10571 (the owner: "You disconnected yourself."): an active RC is
# success, never toggled; the old disconnect-and-retype dropped a connection the owner
# had just made by hand. Sets RC_URL on success (returns 0); sets RC_FAIL and
# returns 1 otherwise.
rc_connect() {
  local before i
  RC_URL=""; RC_FAIL=""
  before=$(_rc_url)
  if _rc_active; then
    log "rc: already active (status line /rc), left alone"
    RC_URL="${before:-already-active}"; return 0
  fi
  rc_send_cmd "/remote-control $s" || { RC_FAIL="/remote-control not echoed"; return 1; }
  sleep 4
  if _rc_dialog; then
    # The status line was not visible but RC was active: close the dialog, keep RC.
    log "rc: already active (dialog), left alone"
    _rc_tx send-keys Escape
    RC_URL="${before:-already-active}"; return 0
  fi
  for i in $(seq 1 8); do
    _rc_cap | grep -q '❯ Continue' && { _rc_tx send-keys Enter; sleep 2; }
    RC_URL=$(_rc_url)
    [ -n "$RC_URL" ] && [ "$RC_URL" != "$before" ] && return 0
    sleep 2
  done
  RC_URL=""
  RC_FAIL="rc not confirmed (no new session URL)"
  return 1
}

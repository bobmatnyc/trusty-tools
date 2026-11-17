#!/usr/bin/env bash
# relay.sh — the Architect's verified two-step send, in one call.
#
# Why: the Architect core gates every send on the input-box classifier, and a
# relay typed over a draft or submitted into someone else's text cannot be
# taken back. This script refuses whenever it is in doubt.
# What: refuses (exit 1, NO-MARKER) unless <text> starts with an Architect
# relay marker `[<Name> Architect HH:MMZ]`; refuses (exit 1,
# CONTROL-CHAR) any C0 control byte except LF and TAB; classifies the box with
# scripts/input-state.py and refuses (exit 1, SKIP) unless it is `empty` or
# `suggestion`; types the text literally with no Enter; classifies again and
# refuses (exit 1, NOT-OURS, no Enter) unless the box is `typed` and starts
# with the same marker; only then sends Enter as a separate call. Text it
# typed and could not confirm stays in the box for the next pass to see.
# #10780: before the box is touched, refuses (exit 3, NO-OWNER-CITE) a
# `User ruling (<name>):` or `User instruction (<name>):` label that is not
# the owner's quoted words plus `(owner turn: <cite>)` verified against the
# owner-turn record (scripts/relay_provenance.py, which logs every check).
# Usage: scripts/relay.sh <session> <text>
#   <session> is the tmux session name; every call uses the exact target
#   '=<session>:'. Works from any directory. Honours TMUX_SOCKET.
#   Exit 0: sent. Exit 1: refused, nothing submitted. Exit 2: usage error.
#   Exit 3: an owner label without a verified owner citation; nothing typed.
# Test: tests/test_relay.py (a stub tmux on PATH).
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
s="${1:-}"
text="${2:-}"
if [ -z "$s" ] || [ -z "$text" ]; then
  echo "usage: relay.sh <session> <text>" >&2
  exit 2
fi
# #9997: refuse CR, ESC and every other C0 control byte except LF and TAB
# before typing; the message names the rule, never the text.
if [ -n "$(printf '%s' "$text" | LC_ALL=C tr -d '\011\012\040-\377')" ]; then
  echo "$s CONTROL-CHAR: the text may hold no C0 control character except LF and TAB"
  exit 1
fi
t="=$s:"
T=(tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"})
# #9997: the marker is `Architect` only; no other role is a relay of ours.
MARKER_RE='^\[([A-Za-z][A-Za-z -]* )?Architect [0-9]{2}:[0-9][0-9x]Z\]'
mark="$(printf '%s\n' "$text" | head -n 1 | grep -oE "$MARKER_RE")" || mark=""
if [ -z "$mark" ]; then
  echo "$s NO-MARKER: the text must start with [<Name> Architect HH:MMZ]"
  exit 1
fi
# #10780: an owner label needs the owner's cited words; refuse before typing.
prov="$(python3 "$HERE/relay_provenance.py" check "$s" "$text")"
case $? in
  0) ;;
  3) echo "$s ${prov}"; exit 3 ;;
  *) echo "$s PROVENANCE input=error; nothing typed"; exit 1 ;;
esac

# Prints "<state>|<box text>" from input-state.py's "<pane> <state> | <text>".
box_state() {
  local out
  out="$(python3 "$HERE/input-state.py" "$t" 2>/dev/null)" || return 1
  out="$(printf '%s\n' "$out" | head -n 1)"
  local rest="${out#* }"
  local state="${rest%% *}"
  local boxtext=""
  case "$rest" in *"| "*) boxtext="${rest#*| }" ;; esac
  [ -n "$state" ] || return 1
  printf '%s|%s' "$state" "$boxtext"
}

if ! before="$(box_state)"; then
  echo "$s SKIP input=error"
  exit 1
fi
case "${before%%|*}" in
  empty|suggestion) ;;
  *) echo "$s SKIP input=${before%%|*}"; exit 1 ;;
esac

if ! "${T[@]}" send-keys -t "=$s:" -l "$text"; then
  echo "$s FAILED send-keys; nothing submitted"
  exit 1
fi
sleep 1

if ! after="$(box_state)"; then
  echo "$s NOT-OURS input=error"
  exit 1
fi
boxtext="${after#*|}"
if [ "${after%%|*}" != typed ]; then
  echo "$s NOT-OURS input=${after%%|*}"
  exit 1
fi
case "$boxtext" in
  "$mark"*) ;;
  *) echo "$s NOT-OURS ${boxtext:0:70}"; exit 1 ;;
esac

if ! "${T[@]}" send-keys -t "=$s:" Enter; then
  echo "$s FAILED Enter; the text is typed, not submitted"
  exit 1
fi
echo "$s sent $(date -u +%H:%M:%SZ)"

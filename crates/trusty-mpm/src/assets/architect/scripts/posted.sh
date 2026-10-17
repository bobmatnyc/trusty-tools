#!/usr/bin/env bash
# posted.sh — double-post guard for a PM pane more than one Architect watches.
#
# Why: two Architects may both hold the same question for the owner, and a
# relay reaches the PM within seconds of the owner's answer. So the guard is
# not a claim taken in advance; it is a last look at the session, right
# before typing, for a relay that already answered the question.
#
# Usage: scripts/posted.sh <session> <since HH:MMZ> [self-name]
#   <session> is the tmux session name; the capture uses the exact target
#   '=<session>:'. <since> is the time the question appeared. self-name (the
#   <Name> in your own relay marker) excludes your own relays.
#   Exit 1: it prints each submitted relay stamped at or after <since>; do
#   not send. Exit 0: it prints NONE; send. Exit 2: it could not check (a
#   usage error, a malformed time, a pane it cannot capture); do not send.
#   scripts/relay-marker.py does the parsing. Honours TMUX_SOCKET.
# Test: tests/test_relay.py (a stub tmux on PATH).
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
s="${1:-}"
since="${2:-}"
self="${3:-}"
if [ -z "$s" ] || [ -z "$since" ]; then
  echo "usage: posted.sh <session> <since HH:MMZ> [self-name]" >&2
  exit 2
fi
T=(tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"})
# #9997: capture first, so a failed capture is an error, never an empty pane.
if ! cap="$("${T[@]}" capture-pane -p -J -t "=$s:" -S -400 2>&1)"; then
  echo "$s ERROR cannot capture the pane: ${cap:0:120}" >&2
  exit 2
fi
printf '%s\n' "$cap" | python3 "$HERE/relay-marker.py" "$since" "$self"

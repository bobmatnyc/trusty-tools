#!/usr/bin/env bash
# effort-send.sh <session> <level> — gated `/effort <level>` into one PM pane.
#
# Why: the effort governor (effort-governor.py) only decides; this script sends,
# and only at an idle prompt, because a command typed during a turn is queued.
# What: target `=<session>:`. Gate (one command, no `;`): input-state.py says
# empty or suggestion AND the last 15 lines show no running turn ("esc to
# interrupt" or a "(... tokens)" working line). Types `/effort <level>` with
# send-keys -l, re-checks the box holds exactly that text, then Enter as a
# separate call, then `effort-governor.py --commit`. Any gate failure prints
# `<session> SKIP <reason>` and exits 1 so the next poll retries.
# Test: scripts/test_effort_governor.py (effort_send_*) against a fake tmux.
set -u
s="${1:?session}"; level="${2:?level}"
case "$level" in medium|low|auto) ;; *) echo "$s SKIP bad level $level"; exit 1 ;; esac
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
t="=$s:"
tm() { tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"} "$@"; }
box() { python3 "$HERE/input-state.py" "$t" | cut -d' ' -f2-; }
skip() { echo "$s SKIP $1"; exit 1; }

state=$(box | cut -d' ' -f1)
case "$state" in empty|suggestion) ;; *) skip "input box is $state" ;; esac
tm capture-pane -t "=$s:" -p -J | tail -n 15 | grep -Eq 'esc to interrupt|\([^)]*tokens\)' && skip "turn running"
cmd="/effort $level"
tm send-keys -t "=$s:" -l "$cmd"
sleep "${EFFORT_ECHO_SLEEP:-1}"
b=$(box)
case "$b" in "typed | $cmd"|"typed | $cmd  "*) ;; *) skip "echo mismatch: $b" ;; esac
tm send-keys -t "=$s:" Enter
python3 "$HERE/effort-governor.py" --commit "$s" "$level" || skip "commit failed"
echo "$s SENT $cmd"

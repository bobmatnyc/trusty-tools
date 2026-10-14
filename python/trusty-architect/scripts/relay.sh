#!/usr/bin/env bash
# relay.sh <session> <text> — the two-step send in one call.
# Refuses unless the input box is empty or a dim suggestion, types the text
# literally, confirms the box now holds a relay (marker "supervisor" or
# "Architect"), then sends Enter.
cd "$(dirname "$0")/.." || exit 1
s=$1; t="=$s:"; st=$(python3 scripts/input-state.py "$t" | awk '{print $2}')
case "$st" in empty|suggestion) ;; *) echo "$s SKIP input=$st"; exit 1;; esac
tmux send-keys -t "$t" -l "$2"; sleep 1
st=$(python3 scripts/input-state.py "$t" | cut -c1-70)
echo "$st" | grep -Eq 'supervisor|Architect' || { echo "$s NOT-OURS $st"; exit 1; }
tmux send-keys -t "$t" Enter; echo "$s sent $(date -u +%H:%M:%SZ)"

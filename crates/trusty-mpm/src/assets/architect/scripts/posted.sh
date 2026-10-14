#!/usr/bin/env bash
# posted.sh — double-post guard for a pane more than one supervisor watches.
#
# Why: two supervisors may both hold the same question for the user, and a
# relay reaches the PM within seconds of the user's answer. So the guard is
# not a claim taken in advance; it is a last look at the session, right
# before typing, for a relay that already answered the question.
#
# Usage: posted.sh <tmux-target> <since HH:MMZ> [self-name]
#   self-name (e.g. "Global") excludes the caller's own relays; only another
#   supervisor's relay can be a double post.
#   Lists every submitted relay turn in the pane's recent history stamped at or
#   after <since> (the time the question appeared). Both marker forms count:
#   "[<Name> supervisor HH:MMZ]" and "[<Name> Architect HH:MMZ]", typed after
#   "❯" or delivered as "› Message from @…:" (scripts/relay-marker.py).
#   Exit 1 and print them if any exist: do not send. Exit 0 and print NONE if
#   there are none: send.
set -euo pipefail
target="${1:?usage: posted.sh <tmux-target> <since HH:MMZ>}"
since="${2:?usage: posted.sh <tmux-target> <since HH:MMZ>}"
self="${3:-}"

tmux capture-pane -p -J -t "$target" -S -400 | python3 "$(dirname "$0")/relay-marker.py" "$since" "$self"

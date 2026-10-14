#!/usr/bin/env bash
# start-question-collector.sh — run question-collector.py in its own tmux session.
# Why: the collector must outlive supervisor restarts and keep running while the
# supervisor waits on a question to the user. Idempotent: does nothing if the
# session already exists (one collector only).
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
S=tm-supervisor-collector
if tmux has-session -t "=$S" 2>/dev/null; then echo "running: $S"; exit 0; fi
# The tmux server's environment lacks CLAUDE_CONFIG_DIR, so without it `claude -p`
# reads the default ~/.claude login instead of the fleet's (stale after an account
# switch: "OAuth session expired"). Pass the caller's value, or the trusty default.
CFG="${CLAUDE_CONFIG_DIR:-$HOME/.trusty-tools/trusty-mpm/claude-config}"
[ -d "$CFG" ] || { echo "no Claude config dir: $CFG" >&2; exit 1; }
tmux new-session -d -s "$S" -c "$ROOT" -e "CLAUDE_CONFIG_DIR=$CFG" "python3 '$ROOT/scripts/question-collector.py'"
echo "started: $S"

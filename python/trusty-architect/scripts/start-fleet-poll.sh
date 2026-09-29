#!/usr/bin/env bash
# start-fleet-poll.sh — run fleet-poll.py in its own tmux session.
# Why: the deterministic poller must outlive Architect restarts and keep
# watching while the Architect's session is busy or waiting on the user.
# Idempotent: does nothing if the session already exists (one poller only).
# Extra arguments pass through to the poller, e.g. --interval 30.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The poller's own session; ARCHITECT_* config passes through to the poller.
S="${ARCHITECT_POLL_SESSION:-${ARCHITECT_SESSION:-tm-architect}-poll}"
E=()
for v in TMUX_SOCKET ARCHITECT_SESSION ARCHITECT_POLL_SESSION ARCHITECT_INBOX_DIR \
         ARCHITECT_QUIET_SESSIONS_FILE ARCHITECT_PROJECT_DIR CLAUDE_CONFIG_DIR \
         LOAD_FACTOR SELF_CTX_DIR SELF_CTX_WINDOW SELF_CTX_THRESHOLD; do
  if [ -n "${!v:-}" ]; then E+=(-e "$v=${!v}"); fi
done
# TMUX_SOCKET selects the server, as in every script here; the poller inherits it.
T=(tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"})
if "${T[@]}" has-session -t "=$S" 2>/dev/null; then echo "running: $S"; exit 0; fi
"${T[@]}" new-session -d -s "$S" -c "$ROOT" ${E[@]+"${E[@]}"} \
  "python3 '$ROOT/scripts/fleet-poll.py' $*"
echo "started: $S"

#!/usr/bin/env bash
# architect-start.sh — the shell part of the post-launch step.
#
# Why: the poller must run at the configured cadence and Remote Control must be
# on, or a fresh install is half-deployed. #11369: the cadence has one source,
# poll_interval_s in local/architect.json, which every start path reads.
# What: starts the poller session via start-fleet-poll.sh with no --interval
# (which does nothing if the session exists: one poller only), so fleet-poll.py
# reads poll_interval_s; prints that interval; reports whether the Architect
# session (ARCHITECT_SESSION, default tm-architect) is running. With --rc it also
# runs rc-restore.sh on it (run it from outside the Architect pane: it skips a
# busy pane). A local/architect.json that does not load stops it (exit 1)
# before anything starts; a failed start-fleet-poll.sh stops it (exit 1) before
# the interval line.
# In-session steps (cron, /remote-control, inbox Monitor) are in README.md, "Post-launch".
# Test: tests/test_poll_cadence.py; start-fleet-poll.sh idempotence is its own check.
# Usage: scripts/architect-start.sh [--rc]
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
A="${ARCHITECT_SESSION:-tm-architect}"
# #11369: the same key and loader fleet-poll.py uses; no --interval here.
if ! interval="$(python3 "$ROOT/scripts/architect_config.py" poll-interval)"; then
  echo "not started: local/architect.json does not load (see above)" >&2
  exit 1
fi
# #10571: a poller that did not start must not read as started.
if ! "$ROOT/scripts/start-fleet-poll.sh"; then
  echo "not started: start-fleet-poll.sh failed (see above)" >&2
  exit 1
fi
echo "poll interval: ${interval}s (poll_interval_s)"
if tmux ${TMUX_SOCKET:+-S "$TMUX_SOCKET"} has-session -t "=$A" 2>/dev/null; then
  echo "running: $A"
  [ "${1:-}" = "--rc" ] && "$ROOT/scripts/rc-restore.sh" "$A"
else
  echo "not running: $A. Launch: tm sessions resume $A (or tm fleet init), then re-run with --rc."
fi
exit 0

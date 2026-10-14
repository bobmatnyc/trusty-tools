#!/usr/bin/env bash
# architect-start.sh — the shell part of the post-launch step.
#
# Why: the poller must run at 600 s (architect.md "Cadence"; the script default
# is 60 s) and Remote Control must be on, or a fresh install is half-deployed.
# What: starts tm-architect-poll via start-fleet-poll.sh --interval 600 (which
# does nothing if the session exists: one poller only); reports whether
# tm-architect is running. With --rc it also runs rc-restore.sh tm-architect
# (run it from outside the Architect pane: it skips a busy pane).
# In-session steps (cron, /remote-control, inbox Monitor) are in README.md, "Post-launch".
# Test: bash -n; start-fleet-poll.sh idempotence is its own check.
# Usage: scripts/architect-start.sh [--rc]
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/scripts/start-fleet-poll.sh" --interval 600
if tmux has-session -t '=tm-architect' 2>/dev/null; then
  echo "running: tm-architect"
  [ "${1:-}" = "--rc" ] && "$ROOT/scripts/rc-restore.sh" tm-architect
else
  echo "not running: tm-architect. Launch: tm sessions resume tm-architect (or tm fleet init), then re-run with --rc."
fi
exit 0

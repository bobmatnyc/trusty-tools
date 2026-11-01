"""fleet_poll_cfg.py — fleet-poll.py's environment-derived settings (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap with the watchdog, IL and self-refresh steps still to land.
What: the paths and names fleet-poll.py reads at load: inbox files, tmux socket, the Architect and poller
session names, the shells and event types, the host thresholds, the quiet-session list. Read once, when
fleet-poll.py loads this file by path; fleet-poll.py re-exports every name, so callers see no change.
Test: tests/test_config.py (through fleet-poll.py's re-exports)
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
# ARCHITECT_INBOX_DIR relocates the queues and logs, e.g. in tests.
INBOX = os.environ.get("ARCHITECT_INBOX_DIR") or os.path.join(ROOT, "inbox")
EVENTS = os.path.join(INBOX, "events.jsonl")
ALERTS = os.path.join(INBOX, "alerts.jsonl")
STATE = os.path.join(INBOX, "poll-state.json")
LOG = os.path.join(INBOX, "poll.log")
LOCK = os.path.join(INBOX, "poll.lock")  # #10571: one live poller per inbox
SOCKET = os.environ.get("TMUX_SOCKET", "")
# #8928 step 4: the name prefix of a workspace's private tmux server.
WORKSPACE_PREFIX = "tm-ws-"

# The Architect's own tmux session: the only pane this poller sends keys to.
ARCHITECT = os.environ.get("ARCHITECT_SESSION") or "tm-architect"
POLL_SESSION = os.environ.get("ARCHITECT_POLL_SESSION") or f"{ARCHITECT}-poll"
SKIP_SESSIONS = {ARCHITECT, POLL_SESSION}
SHELLS = {"bash", "zsh", "sh", "fish", "dash", "ksh", "tcsh", "csh", "login"}
EVENT_TYPES = {"permission_prompt", "agent_needs_input"}  # idle_prompt is not an alert

DISK_HIGH = 90
LOAD_FACTOR_DEFAULT = 1.5  # x cores, vs. the 5-min load avg; env LOAD_FACTOR overrides

# The watchdog's one relaunch path (fleet_poll_watchdog.py); not shipped yet, so a relaunch reports failed.
RELAUNCH_SCRIPT = os.path.join(HERE, "session-relaunch.sh")

QUIET_SESSIONS_FILE = (os.environ.get("ARCHITECT_QUIET_SESSIONS_FILE")
                       or os.path.join(ROOT, "quiet-sessions.txt"))

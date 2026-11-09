"""fleet_poll_cfg.py — fleet-poll.py's environment-derived settings (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap with the watchdog, IL and self-refresh steps still to land.
What: the paths and names fleet-poll.py reads at load: inbox files, tmux socket, the Architect and poller
session names, the shells and event types, the host thresholds, the quiet-session list. Read once, when
fleet-poll.py loads this file by path; fleet-poll.py re-exports every name, so callers see no change.
Test: tests/test_config.py (through fleet-poll.py's re-exports)
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
# ARCHITECT_INBOX_DIR relocates the queues and logs, e.g. in tests.
INBOX = os.environ.get("ARCHITECT_INBOX_DIR") or os.path.join(ROOT, "inbox")
EVENTS = os.path.join(INBOX, "events.jsonl")
ALERTS = os.path.join(INBOX, "alerts.jsonl")
STATE = os.path.join(INBOX, "poll-state.json")
LOG = os.path.join(INBOX, "poll.log")
LOCK = os.path.join(INBOX, "poll.lock")  # #10571: one live poller per inbox
HOLD = os.path.join(INBOX, "hold.json")  # #10857: the Architect-wide hold (alert_triage.read_hold)
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

# #10861: <root>/local/, the file `tm architect quiet` and the Rust poller read.
def resolve_quiet_sessions_file(root, env=None):
    """The quiet list's path: env override, else <root>/local/, else the legacy <root>/ file.

    #10861: legacy-path fallback. A list left at the pre-#10861 `<root>/quiet-sessions.txt` is still read,
    with one stderr warning, so a host that has not moved it does not start alerting on quieted sessions.
    """
    env = os.environ if env is None else env
    if env.get("ARCHITECT_QUIET_SESSIONS_FILE"):
        return env["ARCHITECT_QUIET_SESSIONS_FILE"]
    new_path = os.path.join(root, "local", "quiet-sessions.txt")
    legacy = os.path.join(root, "quiet-sessions.txt")
    if not os.path.exists(new_path) and os.path.exists(legacy):
        print(f"fleet-poll: reading the quiet list from the legacy path {legacy}; "
              f"move it to {new_path} (#10861)", file=sys.stderr)
        return legacy
    return new_path


QUIET_SESSIONS_FILE = resolve_quiet_sessions_file(ROOT)

#!/usr/bin/env python3
"""poll-reset-wake.py — one-shot reset of the stuck wake backlog in inbox/poll-state.json.

Why (see #10571): wake_pending/wake_alerts grew to 2000+ while every wake failed.
The running poller rewrites poll-state.json each cycle, so a reset made while it runs is lost;
this refuses while the poller's tmux session exists (ARCHITECT_POLL_SESSION, else ARCHITECT_SESSION-poll, else
tm-architect-poll: the name start-fleet-poll.sh gives it).
What: backs the file up to poll-state.json.bak-<UTC>, then removes wake_pending, wake_logged,
wake_alerts and wake_alerts_dropped (sets digest_cursor to now so old PM messages do not refill the digest).
Nothing else changes; the next confirmed wake sets wake_sent_at.
Test: tests/test_poll_supervise.py
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KEYS = ("wake_pending", "wake_logged", "wake_alerts", "wake_alerts_dropped")


def poll_session(environ=None):
    """#10571 S2: the poller's session name from the environment, as start-fleet-poll.sh derives it."""
    env = os.environ if environ is None else environ
    return env.get("ARCHITECT_POLL_SESSION") or f"{env.get('ARCHITECT_SESSION') or 'tm-architect'}-poll"


def poller_running():
    cmd = ["tmux"] + (["-S", os.environ["TMUX_SOCKET"]] if os.environ.get("TMUX_SOCKET") else [])
    return subprocess.run(cmd + ["has-session", "-t", f"={poll_session()}"],
                          capture_output=True).returncode == 0


def reset(path, now=None):
    """Back up and reset; returns (backup path, removed key -> old size)."""
    now = time.time() if now is None else now
    with open(path) as f:
        state = json.load(f)
    backup = f"{path}.bak-{datetime.fromtimestamp(now, timezone.utc):%Y%m%dT%H%M%SZ}"
    shutil.copy2(path, backup)
    removed = {k: (len(state[k]) if isinstance(state[k], list) else state[k])
               for k in KEYS if k in state}
    for k in KEYS:
        state.pop(k, None)
    state["digest_cursor"] = now
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, path)
    return backup, removed


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--state", default=os.path.join(ROOT, "inbox", "poll-state.json"))
    ap.add_argument("--skip-poller-check", action="store_true", help="tests only")
    a = ap.parse_args(argv)
    if not a.skip_poller_check and poller_running():
        print(f"refused: tmux session {poll_session()} is running; stop it first", file=sys.stderr)
        return 2
    backup, removed = reset(a.state)
    print(f"backup {backup}; removed {removed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

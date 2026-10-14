#!/usr/bin/env python3
"""user_state.py: the owner's presence state, set from the dashboard settings pane.

Why: the owner (2026-10-09) wants engagement and token use tuned to whether he is at the computer, on a
device, away or asleep. The dashboard writes meta/settings; the poller and the pending-answers hook read it.
What: read(root) returns {state, at, by} from <root>/meta/settings.json. State is one of STATES;
unset, unreadable or unknown values give DEFAULT ("active") with at "".
Test: scripts/test_user_state.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

STATES = ("active", "remote", "away", "sleeping")
DEFAULT = "active"


def read(root=None):
    path = os.path.join(root or ds.DEFAULT_ROOT, "meta", "settings.json")
    try:
        with open(path, encoding="utf-8") as f:
            raw = json.load(f)
    except (OSError, ValueError):
        return {"state": DEFAULT, "at": "", "by": ""}
    data = raw.get("data", raw) if isinstance(raw, dict) else {}
    if not isinstance(data, dict) or data.get("user_state") not in STATES:
        return {"state": DEFAULT, "at": "", "by": ""}
    return {"state": data["user_state"], "at": str(data.get("user_state_at", "")), "by": str(data.get("user_state_by", ""))}

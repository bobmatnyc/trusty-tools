"""auto_sleep.py: set the owner's user state to sleeping at the configured local start time.

Why: the owner (2026-10-11) wants sleep to start by itself at a set time; only the owner leaves it.
What: step(root, now_epoch) reads meta/settings (sleep_start "HH:MM", tz, user_state, sleep_auto_on).
Once per local date in tz, when local time >= sleep_start, it writes user_state "sleeping",
user_state_at, user_state_by "Architect (auto-sleep)" and sleep_auto_on = the local date, pinned with
if_version. It never sets another state. When the state is already sleeping it only records
sleep_auto_on, so a later wake the same day is not undone. Returns "fired", "marked" or None;
a version conflict returns None (retried next cycle). Missing or invalid sleep_start/tz: None.
Test: tests/test_auto_sleep.py
"""
import os
import sys
from datetime import datetime, timezone
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

BY = "Architect (auto-sleep)"


def _hhmm(text):
    try:
        h, m = str(text).split(":")
        h, m = int(h), int(m)
    except ValueError:
        return None
    return (h, m) if 0 <= h < 24 and 0 <= m < 60 else None


def step(root=None, now=None):
    store = ds.Store(root or ds.DEFAULT_ROOT, actor="poller")
    try:
        doc = store.get("meta", "settings")
    except ds.NotFound:
        return None
    data = doc["data"]
    start = _hhmm(data.get("sleep_start"))
    if start is None:
        return None
    try:
        local = datetime.fromtimestamp(now if now is not None else datetime.now(timezone.utc).timestamp(),
                                       ZoneInfo(str(data.get("tz") or "")))
    except (ZoneInfoNotFoundError, ValueError, KeyError):
        return None
    today = local.strftime("%Y-%m-%d")
    if data.get("sleep_auto_on") == today or (local.hour, local.minute) < start:
        return None
    stamp = local.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")
    if data.get("user_state") == "sleeping":
        patch, result = {"sleep_auto_on": today}, "marked"
    else:
        patch = {"user_state": "sleeping", "user_state_at": stamp, "user_state_by": BY, "sleep_auto_on": today}
        result = "fired"
    try:
        store.update("meta", "settings", patch, if_version=doc["version"])
    except ds.Conflict:
        return None
    return result

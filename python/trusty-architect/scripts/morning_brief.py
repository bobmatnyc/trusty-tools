#!/usr/bin/env python3
"""morning_brief.py: schedule for the owner's morning brief (owner, 2026-10-25: "Let's do a morning brief each weekday morning").

Why: the brief is a standard Architect behaviour, so a deterministic clock (not the Architect's memory) decides
when it is due. Each user customises it in the dashboard settings pane (meta/settings.morning_brief).
What: resolve(data) turns meta/settings into {enabled, time, days, tz, warnings}; defaults when absent are
enabled, "07:30", "weekdays", tz = meta/settings.tz else the host zone. step(state, now, root) returns one dict
{date, start, end, time, tz} when a brief is due, else None. Due = a configured day, local time at or past the
configured time, at most 4 hours late (a poller that was down longer skips that day), and not yet fired for
that local date (state["morning_brief"]["last"], persisted in poll-state.json by the caller). The window starts
at the previous configured day's brief time (Monday's starts Friday's, so it covers the weekend) and ends now,
both ISO UTC. zoneinfo handles DST. An invalid setting falls back to its default and is named in warnings.
Test: tests/test_morning_brief.py
"""
import os
import re
import sys
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

DEFAULT_TIME = "07:30"
DEFAULT_DAYS = "weekdays"
LATE_LIMIT = timedelta(hours=4)   # first tick later than this past the time: skip that day
DAYS = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")
WEEKDAYS = DAYS[:5]


def _hhmm(text):
    if not isinstance(text, str) or not re.fullmatch(r"\d{2}:\d{2}", text):
        return None
    h, m = int(text[:2]), int(text[3:])
    return (h, m) if h < 24 and m < 60 else None


def host_zone():
    """The host's IANA zone: TZ, else the /etc/localtime link target, else UTC."""
    for cand in (os.environ.get("TZ", ""), _localtime_name()):
        zone = _zone(cand)
        if zone is not None:
            return zone
    return ZoneInfo("UTC")


def _localtime_name():
    try:
        target = os.path.realpath("/etc/localtime")
    except OSError:
        return ""
    return target.split("zoneinfo/", 1)[1] if "zoneinfo/" in target else ""


def _zone(name):
    try:
        return ZoneInfo(name) if name else None
    except (ZoneInfoNotFoundError, ValueError, OSError):
        return None


def _days(spec):
    """Set of weekday indexes (Monday 0) for "weekdays", "daily" or a list of mon..sun; None if invalid."""
    if spec == "weekdays":
        return set(range(5))
    if spec == "daily":
        return set(range(7))
    if isinstance(spec, list) and all(isinstance(d, str) and d.strip().lower()[:3] in DAYS for d in spec):
        return {DAYS.index(d.strip().lower()[:3]) for d in spec}
    return None


def resolve(data):
    """Effective config from the meta/settings body (a dict, or None when the doc is absent)."""
    data = data if isinstance(data, dict) else {}
    raw = data.get("morning_brief")
    warnings = []
    if raw is not None and not isinstance(raw, dict):
        warnings.append("morning_brief is not an object; defaults used")
    raw = raw if isinstance(raw, dict) else {}
    enabled = raw.get("enabled", True)
    if not isinstance(enabled, bool):
        warnings.append(f"enabled {enabled!r} is not a boolean; default true used")
        enabled = True
    text = raw.get("time", DEFAULT_TIME)
    if _hhmm(text) is None:
        warnings.append(f"time {text!r} is not HH:MM; default {DEFAULT_TIME} used")
        text = DEFAULT_TIME
    spec = raw.get("days", DEFAULT_DAYS)
    days = _days(spec)
    if days is None:
        warnings.append(f"days {spec!r} is not weekdays, daily or a list of mon..sun; default weekdays used")
        days = _days(DEFAULT_DAYS)
    zone = None
    for label, name in (("morning_brief.tz", raw.get("tz")), ("tz", data.get("tz"))):
        if not name:
            continue
        zone = _zone(str(name))
        if zone is not None:
            break
        warnings.append(f"{label} {name!r} is not an IANA zone; next fallback used")
    return {"enabled": enabled, "time": text, "days": days, "tz": zone or host_zone(), "warnings": warnings}


def _due_at(day, hhmm, tz):
    h, m = _hhmm(hhmm)
    return datetime(day.year, day.month, day.day, h, m, tzinfo=tz)


def _utc(dt):
    return dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def read_settings(root=None):
    """meta/settings body, {} when the doc is absent. Other store errors propagate to the caller."""
    store = ds.Store(root or os.environ.get("DASHBOARD_ROOT") or ds.DEFAULT_ROOT, actor="poller")
    try:
        return store.get("meta", "settings")["data"]
    except ds.NotFound:
        return {}


def step(state, now, root=None):
    """The due brief, or None. Records state["morning_brief"]["last"] = local date when it returns a brief.
    Returns {"date", "start", "end", "time", "tz", "warnings"}; warnings is the resolve() list."""
    cfg = resolve(read_settings(root))
    if not cfg["enabled"]:
        return None
    tz = cfg["tz"]
    end = datetime.fromtimestamp(now, timezone.utc)
    today = end.astimezone(tz).date()
    if today.weekday() not in cfg["days"]:
        return None
    due = _due_at(today, cfg["time"], tz)
    if end < due.astimezone(timezone.utc) or end - due.astimezone(timezone.utc) > LATE_LIMIT:
        return None
    mark = state.setdefault("morning_brief", {})
    if mark.get("last") == today.isoformat():
        return None
    prev = today - timedelta(days=1)
    for _ in range(8):   # at most a week back: the previous configured day
        if prev.weekday() in cfg["days"]:
            break
        prev -= timedelta(days=1)
    else:
        prev = None
    start = _due_at(prev, cfg["time"], tz) if prev is not None else end - timedelta(hours=24)
    mark["last"] = today.isoformat()
    return {"date": today.isoformat(), "start": _utc(start), "end": _utc(end), "time": cfg["time"],
            "tz": getattr(tz, "key", str(tz)), "warnings": cfg["warnings"]}

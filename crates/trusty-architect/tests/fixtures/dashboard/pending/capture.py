#!/usr/bin/env python3
"""Capture the #10403 pending/derive goldens from the live dashboard_derive.py.

Usage: python3 -I capture.py <architect scripts dir>
Reads cases.json and derive_cases.json next to this file; writes goldens.json
and isotime.json next to it. Read-only toward the scripts dir.
"""
import json
import os
import sys
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, sys.argv[1])
import dashboard_derive as dd  # noqa: E402

EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)


def micros(t):
    return None if t is None else (t - EPOCH) // timedelta(microseconds=1)


def docs(rows):
    return [{"id": r["id"], "version": 1, "data": r["data"]} for r in rows]


def at(now):
    return datetime.fromtimestamp(now, timezone.utc)


def load(name):
    with open(os.path.join(HERE, name), encoding="utf-8") as f:
        return json.load(f)


out = {"pending": {}, "derive": {}}
for case in load("cases.json"):
    snap = {k: docs(v) for k, v in case["snap"].items()}
    out["pending"][case["name"]] = json.dumps(dd.pending(snap, at(case["now"])))

dc = load("derive_cases.json")
now = at(dc["now"])
items, goals, sessions = docs(dc["items"]), docs(dc["goals"]), docs(dc["sessions"])
out["derive"]["stale_tasks"] = json.dumps([[micros(r[0]), r[1], r[2], r[3]] for r in dd.stale_tasks(items, now)])
out["derive"]["last_activity"] = json.dumps([[d["id"], micros(dd.last_activity(d["data"]))] for d in items])
by_goal, by_session = dd.goal_sessions(goals, sessions)
out["derive"]["goal_sessions"] = json.dumps([list(by_goal.items()), list(by_session.items())])
out["derive"]["ordered"] = json.dumps([d["id"] for d in dd.ordered(docs(dc["ranked"]))])
out["derive"]["run_progress"] = json.dumps([[d["id"], dd.run_progress(d["data"], now)] for d in docs(dc["runs"])])

with open(os.path.join(HERE, "goldens.json"), "w", encoding="utf-8") as f:
    json.dump(out, f, indent=1, sort_keys=True)
    f.write("\n")

CORPUS = [
    "2026-10-12", "20261012", "2026-10", "2026-1012", "202610-12", "2026-10-12T04:12",
    "2026-10-12T04:12:30", "2026-10-12T04:12:30.5", "2026-10-12T04:12:30.123456",
    "2026-10-12T04:12:30.1234567", "2026-10-12T04:12:30,25", "2026-10-12T04:12:30.",
    "2026-10-12T0412", "2026-10-12T041230", "2026-10-12T04", "2026-10-12T4",
    "2026-10-12 04:12:30", "2026-10-12x04:12", "2026-10-12T04:12+00:00", "2026-10-12T04:12+02:00",
    "2026-10-12T04:12-05:30", "2026-10-12T04:12+0530", "2026-10-12T04:12+05", "2026-10-12T04:12+5",
    "2026-10-12T04:12+05:30:15", "2026-10-12T04:12+05:30:15.5", "2026-10-12T04:12+24:00",
    "2026-10-12T04:12+23:59", "2026-10-12T04:12Z", "2026-10-12T04:12+00:00+00:00",
    "2026-10-12T24:00", "2026-10-12T24:00:01", "2026-12-31T24:00", "9999-12-31T24:00",
    "2026-10-12T25:00", "2026-10-12T23:60", "2026-10-12T23:59:60", "2026-02-29", "2024-02-29",
    "2026-13-01", "2026-00-10", "0000-01-01", "0001-01-01", "9999-12-31", "2026-W01", "2026W01",
    "2026-W01-1", "2026W017", "2026-W53", "2020-W53-7", "2026-W00", "2026-W01-8", "2026-W011",
    "2026W01-1", "2026-W01T04:12", "2026W017T0412", "2026-10-12T04:12:30.5+01:00",
    "2026-10-12T04:12:30Z", "next tuesday", "None", "True", "42", "", "2026-10-12T", "2026-10-1",
    "2026-10-12T04:12 ", " 2026-10-12", "2026-10-12é04:12", "٢٠٢٦-10-12",
    "2026-10-12T04:1", "2026-10-12T04:12:3", "2026-10-12T04:12:30.12Z", "2026-10-12T04-12",
    "2026-10-12T04:12-", "2026-10-12T04:12+", "2026-10-12T04:12+0", "2026-10-12T04:12+000",
    "2026-10-12T04:12:30.5-00:00", "2026-10-12T104Z", "2026-10-12T104", "2026-10-12T10:Z",
    "2026-10-12T10:41:Z", "2026-10-12T10:41:30.Z", "2026-10-12T10.5", "2026-10-12T04+01-02",
    "2026-10-12T04-01+02", "2026-10-12T0412305", "2026-10-12T0412305Z", "2026-10-12T04:12:30x",
    "2026-10-12T041230x", "2026-10-12T1Z", "2026-10-12T10:41:30,5Z", "2026-10-12T04:12:30.5x",
    "2026-10-12T04:12+01:30x", "2026-10-12T04:12+01:", "2026-10-12T04:12Zx", "2026-10-12T0Z", "1970-01-01T00:00:00.000001", "1969-12-31T23:59:59.999999",
]
DATES = ["2026-10-12", "20261012", "2026-W41-1", "2026W411", "2026-W41", "2024-02-29", "2026-02-29"]
SEPS = ["", "T", " ", "x"]
TIMES = ["", "04", "04:12", "0412", "04:12:30", "041230", "04:12:30.5", "04:12:30.1234567", "24:00", "4:12"]
ZONES = ["", "Z", "+01", "+0130", "+01:30", "-01:30:15", "+01:30:15.25", "+1", "+013", "Z+01"]
GEN = [d + s + t + z for d in DATES for s in SEPS for t in TIMES for z in ZONES]
rows = []
for s in CORPUS + GEN:
    try:
        t = datetime.fromisoformat(s)
        t = t if t.tzinfo else t.replace(tzinfo=timezone.utc)
        rows.append([s, micros(t)])
    except ValueError:
        rows.append([s, None])
with open(os.path.join(HERE, "isotime.json"), "w", encoding="utf-8") as f:
    json.dump(rows, f, indent=0)
    f.write("\n")

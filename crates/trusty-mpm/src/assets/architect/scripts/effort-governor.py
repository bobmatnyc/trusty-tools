#!/usr/bin/env python3
"""Effort governor: pure decision step (an owner ruling; see #10571).

Why: slow plan usage down when the weekly limit passes 90% by lowering
the `/effort` of the PM sessions on that account, and restore it after the reset.
What: reads local/usage/ledger.jsonl and inbox/effort-state.json and prints one
line per session that needs a change: `<session> <level> <reason>` (level is
medium, low or auto). Never sends keys. Writes only inbox/effort-state.json, and
only with `--commit <session> <level>` after scripts/effort-send.sh sent it.
Policy: pctwk for the active account (no 5-hour gate; the owner: "no 5 hour
limits"; the account of the newest ledger row; no session-to-account map exists, so every session is taken
to run on it). Under 90 normal, 90 to under 95 medium, 95 and over low. Restore
(`auto`) only for sessions the governor lowered, when the reading drops under 90
or the account changes. A stale reading (usage.STALE_AFTER) changes nothing.
Usage: effort-governor.py [--sessions a,b] [--commit SESSION LEVEL]
Test: scripts/test_effort_governor.py
"""
import argparse
import importlib.util
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
STATE = os.environ.get("EFFORT_STATE") or os.path.join(ROOT, "inbox", "effort-state.json")
# The Architect and its poller are never governed. #10571 S1: the names come from
# ARCHITECT_SESSION / ARCHITECT_POLL_SESSION as in fleet_poll_cfg.py, never an instance list.
# tm-supervisor-collector is fleet infrastructure, not an instance name, so it stays fixed.
ARCHITECT = os.environ.get("ARCHITECT_SESSION") or "tm-architect"
SKIP = {ARCHITECT, os.environ.get("ARCHITECT_POLL_SESSION") or f"{ARCHITECT}-poll",
        "tm-supervisor-collector"}
MEDIUM_AT, LOW_AT = 90, 95
LEVELS = ("medium", "low", "auto")


def _usage():
    spec = importlib.util.spec_from_file_location("usage", os.path.join(HERE, "usage.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def level_for(pct):
    """Target level for a usage percentage: None (normal), medium or low."""
    if pct is None or pct < MEDIUM_AT:
        return None
    return "medium" if pct < LOW_AT else "low"


def reading(rows, now, usage=None):
    """Freshest reading for the active account: dict or None.

    Keys: account, pct (weekly), reset (the weekly reset, ISO),
    stale. None when the ledger is empty.
    """
    usage = usage or _usage()
    if not rows:
        return None
    rec = rows[-1]  # rows are sorted by ts: the newest row names the active account
    e = usage.effective(rec, now)
    pw = e["pctwk"] or 0
    drv = e["resetwk"]
    return {"account": rec["account"], "pct": pw, "stale": e["stale"],
            "reset": drv.strftime("%Y-%m-%dT%H:%M:%SZ") if drv else ""}


def decide(sessions, rd, state):
    """List of dicts {session, level, reason, fingerprint, account, reset}.

    `state` is the effort-state dict ({"sessions": {s: {level, at, account,
    reset}}}). A session absent from `state` was not lowered by the governor.
    """
    if rd is None or rd["stale"]:
        return []
    want = level_for(rd["pct"])
    mine = state.get("sessions", {})
    out = []
    for s in sorted(sessions):
        if s in SKIP:
            continue
        cur = mine.get(s)
        if want is None:
            if cur:
                why = ("account changed" if cur.get("account") != rd["account"]
                       else "window reset") + f", reading {rd['pct']}% under {MEDIUM_AT}%"
                out.append(_chg(s, "auto", why, cur.get("reset", ""), rd))
        elif not cur or cur["level"] != want:
            out.append(_chg(s, want, f"{rd['account']} at {rd['pct']}%", rd["reset"], rd))
    return out


def _chg(s, level, reason, reset, rd):
    return {"session": s, "level": level, "reason": reason, "account": rd["account"],
            "reset": reset, "fingerprint": f"{s}|{level}|{reset}"}


def load_state(path=None):
    try:
        with open(path or STATE) as f:
            d = json.load(f)
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def commit(state, session, level, rd, now):
    """Record a successful send. `auto` forgets the session (restored)."""
    sess = state.setdefault("sessions", {})
    if level == "auto":
        sess.pop(session, None)
    else:
        sess[session] = {"level": level, "at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
                         "account": rd["account"] if rd else "", "reset": rd["reset"] if rd else ""}
    return state


def save_state(state, path=None):
    path = path or STATE
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f, indent=1)
    os.replace(tmp, path)


def live_sessions():
    """tm-* tmux sessions (read-only list-sessions); empty on any tmux failure."""
    cmd = ["tmux"] + (["-S", os.environ["TMUX_SOCKET"]] if os.environ.get("TMUX_SOCKET") else []) \
        + ["list-sessions", "-F", "#{session_name}"]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return []
    return [s for s in out.split() if s.startswith("tm-") and s not in SKIP]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--sessions", help="comma list; default: live tm-* sessions")
    ap.add_argument("--commit", nargs=2, metavar=("SESSION", "LEVEL"))
    a = ap.parse_args(argv)
    usage = _usage()
    now = datetime.now(timezone.utc)
    rd = reading(usage.read_ledger(), now, usage)
    state = load_state()
    if a.commit:
        s, level = a.commit
        if level not in LEVELS:
            print(f"bad level {level}", file=sys.stderr)
            return 2
        save_state(commit(state, s, level, rd, now))
        return 0
    sessions = a.sessions.split(",") if a.sessions else live_sessions()
    for c in decide(sessions, rd, state):
        print(f"{c['session']} {c['level']} {c['reason']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

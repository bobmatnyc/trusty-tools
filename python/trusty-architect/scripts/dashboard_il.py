"""dashboard_il.py: read-only aggregate of every Intent Ledger (datg store) for the Architect dashboard.

Why: the owner Df7 (2026-10-21 08:14Z): "There should be no separate architect IL page. The architect IL page should be ALL
IL entries, a consolidation of individual sessions, including the architects."
What: aggregate() loads the Architect's IL, every local/il-schedule.json session, and every datg state.json under the
known project roots (legacy `.trusty-mpm/datg/state.json` and per-PM `.trusty-mpm/datg/<session>/state.json`), and returns
one row per ledger. A missing or corrupt state.json becomes a row with `error` set; nothing here raises on bad data and
nothing is ever written (files are opened for reading only). Served by dashboard-serve.py as GET /api/il (architect
mode only). Output: {generated_at, totals, sessions: [{session, code, architect, project, state_path, error, v,
written_at, written_epoch, age_s, stale, task, next, goals, items, tasks, counts}]}. Every item and task carries
`session`. Only open/answered items and open/running tasks are listed; done work lives in the ledger archive.
Test: tests/test_dashboard_il.py
"""
import json
import os
import re
import time
from datetime import datetime, timezone

STALE_S = 24 * 3600          # a ledger not written for a day is flagged stale
MAX_STATE = 2 << 20          # a state.json past 2 MB is an error row (datg refuses writes past 16 KB)
SESSION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
DEFAULT_ARCHITECT_SESSION = "tm-architect"
ANSWER_MAX = 300


def architect_session():
    """The Architect's tmux session name: ARCHITECT_SESSION, else tm-architect. Read per call, never cached."""
    # #10161: no session name is built into a shipped script; il-archive.py reads the same variable.
    return os.environ.get("ARCHITECT_SESSION") or DEFAULT_ARCHITECT_SESSION


def _read_schedule(path):
    """The `sessions` list of il-schedule.json as [{name, project, code}]; unreadable file or bad entries give fewer."""
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return []
    rows = data.get("sessions") if isinstance(data, dict) else None
    out = []
    for e in rows if isinstance(rows, list) else []:
        if isinstance(e, dict) and isinstance(e.get("name"), str) and e["name"] and isinstance(e.get("project"), str) and e["project"]:
            out.append({"name": e["name"], "project": e["project"], "code": e["code"] if isinstance(e.get("code"), str) else ""})
    return out


def _state_files(project):
    """[(subdir name or None, state.json path)] for a project: the legacy file, then each per-PM directory."""
    base = os.path.join(project, ".trusty-mpm", "datg")
    found = []
    legacy = os.path.join(base, "state.json")
    if os.path.exists(legacy):
        found.append((None, legacy))
    try:
        subs = sorted(os.listdir(base))
    except OSError:
        return found
    for s in subs:
        p = os.path.join(base, s, "state.json")
        if SESSION_RE.match(s) and os.path.isfile(p):
            found.append((s, p))
    return found


def discover(projects, schedule_path, architect_il):
    """Ordered, de-duplicated [{session, code, architect, project, state_path}] (state_path keyed by realpath).
    Order of precedence: the Architect, then schedule entries (their names win), then project roots."""
    sched = _read_schedule(schedule_path)
    out, seen = [], set()

    def add(session, code, project, path, architect=False):
        key = os.path.realpath(path)
        if key in seen:
            return
        seen.add(key)
        out.append({"session": session, "code": code, "architect": architect, "project": project, "state_path": path})

    arch_real = os.path.realpath(architect_il) if architect_il else ""
    arch_name = next((e["name"] for e in sched if os.path.realpath(e["project"]) == arch_real), architect_session())
    if architect_il:
        files = _state_files(architect_il)
        if files:
            for sub, p in files:
                add(arch_name if sub is None else sub, "", architect_il, p, architect=True)
        else:
            add(arch_name, "", architect_il, os.path.join(architect_il, ".trusty-mpm", "datg", "state.json"), architect=True)
    for e in sched:
        if os.path.realpath(e["project"]) == arch_real:
            continue
        files = _state_files(e["project"])
        if not files:   # scheduled but no ledger on disk: report it, do not hide it
            add(e["name"], e["code"], e["project"], os.path.join(e["project"], ".trusty-mpm", "datg", "state.json"))
        for sub, p in files:
            add(e["name"] if sub is None else sub, e["code"], e["project"], p)
    for name in sorted(projects or {}):
        root = projects[name]
        for sub, p in _state_files(root):
            add(name if sub is None else sub, "", root, p)
    return out


def _iso_to_epoch(s):
    try:
        return int(datetime.strptime(s, "%Y-%m-%dT%H:%MZ").replace(tzinfo=timezone.utc).timestamp())
    except (TypeError, ValueError):
        return None


def _s(v, n=None):
    v = v if isinstance(v, str) else ("" if v is None else str(v))
    return v[:n] if n else v


def _row(meta, error=None):
    return {"session": meta["session"], "code": meta["code"], "architect": meta["architect"], "project": meta["project"],
            "state_path": meta["state_path"], "error": error, "v": None, "written_at": "", "written_epoch": None,
            "age_s": None, "stale": False, "task": "", "next": "", "goals": [], "items": [], "tasks": [],
            "counts": {"goals": 0, "items_open": 0, "items_answered": 0, "tasks_open": 0, "tasks_running": 0}}


def load_one(meta, now):
    """One ledger row. Never raises: a missing, oversized, unparsable or mis-shaped state.json sets `error`."""
    row = _row(meta)
    path = meta["state_path"]
    try:
        if os.path.getsize(path) > MAX_STATE:
            return _row(meta, "state.json too large")
        with open(path, encoding="utf-8") as f:
            st = json.load(f)
    except FileNotFoundError:
        return _row(meta, "state.json missing")
    except (OSError, ValueError) as e:
        return _row(meta, f"state.json unreadable: {type(e).__name__}")
    if not isinstance(st, dict) or not all(isinstance(st.get(k), list) for k in ("goals", "items", "tasks")):
        return _row(meta, "state.json has a bad shape")
    sess = st.get("session") if isinstance(st.get("session"), dict) else {}
    name = meta["session"]
    row["code"] = _s(st.get("code")) or meta["code"]
    row["v"] = st.get("v") if isinstance(st.get("v"), int) else None
    row["written_at"] = _s(sess.get("written_at"))
    ep = sess.get("written_epoch")
    ep = ep if isinstance(ep, int) and not isinstance(ep, bool) else _iso_to_epoch(row["written_at"])
    row["written_epoch"] = ep
    if ep is not None:
        row["age_s"] = max(0, int(now - ep))
        row["stale"] = row["age_s"] > STALE_S
    row["task"], row["next"] = _s(sess.get("task")), _s(sess.get("next"))
    for g in st["goals"]:
        if isinstance(g, dict):
            row["goals"].append({"id": _s(g.get("id")), "rank": g.get("rank") if isinstance(g.get("rank"), int) else None,
                                 "title": _s(g.get("title")), "finish": _s(g.get("finish")), "status": _s(g.get("status")),
                                 "session": name})
    row["goals"].sort(key=lambda g: (g["rank"] is None, g["rank"] or 0, g["id"]))
    for it in st["items"]:
        if isinstance(it, dict) and it.get("status") in ("open", "answered"):
            row["items"].append({"id": _s(it.get("id")), "kind": _s(it.get("kind")), "status": it["status"],
                                 "goal": _s(it.get("goal")), "title": _s(it.get("title")),
                                 "answer": _s(it.get("answer"), ANSWER_MAX), "esc": it.get("esc") or None, "session": name})
    for t in st["tasks"]:
        if isinstance(t, dict) and t.get("status") in ("open", "running"):
            row["tasks"].append({"id": _s(t.get("id")), "goal": _s(t.get("goal")), "title": _s(t.get("title")),
                                 "status": t["status"], "to": _s(t.get("to")), "session": name})
    c = row["counts"]
    c["goals"] = len(row["goals"])
    c["items_open"] = sum(1 for i in row["items"] if i["status"] == "open")
    c["items_answered"] = sum(1 for i in row["items"] if i["status"] == "answered")
    c["tasks_open"] = sum(1 for t in row["tasks"] if t["status"] == "open")
    c["tasks_running"] = sum(1 for t in row["tasks"] if t["status"] == "running")
    return row


def aggregate(projects, schedule_path, architect_il, now=None):
    """The consolidated view: Architect first, then sessions by name. Read-only."""
    now = time.time() if now is None else now
    rows = [load_one(m, now) for m in discover(projects, schedule_path, architect_il)]
    rows.sort(key=lambda r: (not r["architect"], r["session"].lower()))
    totals = {"sessions": len(rows), "errors": sum(1 for r in rows if r["error"]), "items_open": 0, "items_answered": 0,
              "tasks_open": 0, "tasks_running": 0}
    for r in rows:
        for k in ("items_open", "items_answered", "tasks_open", "tasks_running"):
            totals[k] += r["counts"][k]
    return {"generated_at": datetime.fromtimestamp(now, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "totals": totals, "sessions": rows}

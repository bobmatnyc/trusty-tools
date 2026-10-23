#!/usr/bin/env python3
"""dashboard_derive.py: every value derived from the dashboard store, as pure functions (stdlib only).

Why: three scripts and the page each computed "open" and "next id" their own way (the owner 2026-10-11 04:12Z: deterministic
data). One module owns the rules; callers load a snapshot through Store and pass an explicit `now`.
What: snapshot(store, colls) -> {coll: [{id, version, data}]}; is_parked(item) (the page's isParked rule);
tally(items); used_ids / next_id / free_count / retire_candidates (ids recycle, 256 per prefix); stale_tasks(items,
now, hours) (kind == "task"); pending(snap, now) (owner events for the Architect); goal_sessions(goals, sessions);
ordered(docs) (rank, then id). No function reads the clock or the filesystem.
Test: scripts/test_dashboard_derive.py
"""
import re
from datetime import datetime, timedelta, timezone

ID_RE = re.compile(r"^([A-Z])([0-9a-f]{2})$")
SPACE = 256
ID_COLLECTIONS = ("items", "goals", "priorities", "sessions", "questions", "media")


def snapshot(store, colls):
    return {c: store.list_docs(c) for c in colls}


def _d(doc):
    return doc["data"] if isinstance(doc.get("data"), dict) else {}


def is_parked(data):
    """An open item the owner parked with a wake date, or by clarify/stale (page isParked)."""
    pk = data.get("parked")
    return data.get("status") == "open" and isinstance(pk, dict) and bool(
        pk.get("until") or pk.get("choice") in ("clarify", "stale"))


def tally(items):
    """Counts over item docs. open = status open and not parked; parked is counted apart."""
    counts = {"question": 0, "action": 0, "task": 0}
    answered = parked = 0
    for doc in items:
        d = _d(doc)
        if is_parked(d):
            parked += 1
        elif d.get("status") == "open" and d.get("kind") in counts:
            counts[d["kind"]] += 1
        elif d.get("status") == "answered":
            answered += 1
    return {"open": sum(counts.values()), "questions": counts["question"], "actions": counts["action"],
            "tasks": counts["task"], "answered_unrelayed": answered, "parked": parked}


RUN_RE = re.compile(r"^(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}):(\d{2})(?::(\d{2}))?Z)?$")


def _run_time(value, end_of_day=False):
    """UTC datetime for 'YYYY-MM-DD' or 'YYYY-MM-DDTHH:MM[:SS]Z'; None when unparseable."""
    m = RUN_RE.match(value) if isinstance(value, str) else None
    if not m:
        return None
    y, mo, d, h, mi, s = m.groups()
    try:
        if h is None:
            base = datetime(int(y), int(mo), int(d), tzinfo=timezone.utc)
            return base + timedelta(days=1) - timedelta(seconds=1) if end_of_day else base
        return datetime(int(y), int(mo), int(d), int(h), int(mi), int(s or 0), tzinfo=timezone.utc)
    except ValueError:
        return None


def run_progress(item, now):
    """Run-window state of an item, or None without a valid run_start/run_end pair.

    {day, days, ended, running, next_run}: day/days count UTC calendar dates inclusive (day 0 before the start);
    ended = now past run_end and status not done; running = status open and start <= now <= end;
    next_run = earliest run_schedule date on or after now's date with no run_log entry on that UTC date (None when none).
    """
    start, end = _run_time(item.get("run_start")), _run_time(item.get("run_end"), True)
    if not start or not end or end < start:
        return None
    days = (end.date() - start.date()).days + 1
    day = 0 if now < start else min(days, (now.date() - start.date()).days + 1)
    logged = {e["at"][:10] for e in item.get("run_log") or [] if isinstance(e, dict) and isinstance(e.get("at"), str)}
    nxt = sorted(x for x in item.get("run_schedule") or []
                 if isinstance(x, str) and x[:10] >= now.date().isoformat() and x[:10] not in logged)
    return {"day": day, "days": days, "ended": now > end and item.get("status") != "done",
            "running": item.get("status") == "open" and start <= now <= end, "next_run": nxt[0][:10] if nxt else None}


def running_count(items, now):
    """Open items whose run window contains now."""
    return sum(1 for doc in items if (run_progress(_d(doc), now) or {}).get("running"))


# ---- ids ---------------------------------------------------------------------
def used_ids(snap, prefix):
    out = set()
    for coll in ID_COLLECTIONS:
        for doc in snap.get(coll, []):
            m = ID_RE.match(doc["id"])
            if m and m.group(1) == prefix:
                out.add(int(m.group(2), 16))
    return out


def free_count(snap, prefix):
    return SPACE - len(used_ids(snap, prefix))


def next_id(snap, prefix):
    """First free id after the highest used, wrapping past ff to the lowest free; None when full."""
    u = used_ids(snap, prefix)
    top = max(u) if u else -1
    for k in range(1, SPACE + 1):
        c = (top + k) % SPACE
        if c not in u:
            return f"{prefix}{c:02x}"
    return None


def retire_candidates(snap, prefix, keep_free=64):
    """Oldest done items (by done_at) to retire so at least keep_free ids stay free."""
    need = keep_free - free_count(snap, prefix)
    if need <= 0:
        return []
    done = []
    for coll in ID_COLLECTIONS:
        for doc in snap.get(coll, []):
            m = ID_RE.match(doc["id"])
            d = _d(doc)
            if m and m.group(1) == prefix and d.get("status") == "done":
                done.append((str(d.get("done_at") or d.get("updated") or d.get("created") or ""), int(m.group(2), 16)))
    return [f"{prefix}{n:02x}" for _, n in sorted(done)[:need]]


# ---- stale tasks -------------------------------------------------------------
def _ts(value):
    try:
        t = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except ValueError:
        return None
    return t if t.tzinfo else t.replace(tzinfo=timezone.utc)


def last_activity(data):
    cands = [data.get("updated")]
    ups = data.get("updates")
    if isinstance(ups, list):
        cands += [u.get("at") for u in ups if isinstance(u, dict)]
    prog = data.get("progress")
    if isinstance(prog, dict):
        cands.append(prog.get("estimated_at"))
    times = [t for t in map(_ts, cands) if t]
    if not times:
        t = _ts(data.get("created"))
        times = [t] if t else []
    return max(times) if times else None


def stale_tasks(items, now, hours=24.0):
    """[(last_activity, id, session, title)] oldest first for open, unparked items of kind task with no recent activity."""
    cutoff = now - timedelta(hours=hours)
    out = []
    for doc in items:
        d = _d(doc)
        if d.get("kind") != "task" or d.get("status") != "open" or d.get("parked"):
            continue
        last = last_activity(d)
        if last is None or last < cutoff:
            out.append((last, doc["id"], str(d.get("session", "")), str(d.get("title", ""))))
    out.sort(key=lambda r: (r[0] is not None, r[0] or now, r[1]))
    return out


# ---- goals and sessions ------------------------------------------------------
def goal_sessions(goals, sessions):
    """{goal id: [session names that exist]} and {session name: [goal ids]} from goals.sessions; unparked goals only."""
    names = {_d(s).get("name") for s in sessions}
    by_goal, by_session = {}, {}
    for g in goals:
        d = _d(g)
        if d.get("lifecycle") in ("removed", "done", "parked"):
            continue
        linked = [n for n in d.get("sessions", []) if n in names]
        by_goal[g["id"]] = sorted(linked)
        for n in linked:
            by_session.setdefault(n, []).append(g["id"])
    return by_goal, {k: sorted(v) for k, v in by_session.items()}


def ordered(docs):
    """Server order: rank (missing last), then id."""
    def key(doc):
        r = _d(doc).get("rank")
        return (0, r, doc["id"]) if isinstance(r, int) and not isinstance(r, bool) else (1, 0, doc["id"])
    return sorted(docs, key=key)


# ---- pending owner events ----------------------------------------------------
def _done(note, prefixes):
    return isinstance(note, str) and note.lstrip().startswith(prefixes)


def _due(until, now):
    t = _ts(until)
    return bool(t) and t <= now


def _relayed(data):
    r = data.get("relayed")
    return isinstance(r, str) and bool(r.strip())


def pending_items(items, now):
    out = []
    for doc in sorted(items, key=lambda x: x["id"]):
        data, item_id = _d(doc), doc["id"]
        status, note = data.get("status"), data.get("answer_note")
        if status in ("answered", "cancelled", "wont_do") and _relayed(data):
            pass
        elif status == "answered" and not _done(note, ("Relayed",)):
            # "Architect Do" on an ask (page button; old stored text "Have Architect Do" still accepted): the Architect carries it out, then sets status done.
            if data.get("answer") in ("Architect Do", "Have Architect Do"):
                out.append({"id": item_id, "kind": "architect_do", "at": str(data.get("answered_at", "")),
                            "text": "Architect Do" + (f": {str(note).strip()}" if isinstance(note, str) and note.strip() else "")})
            else:
                out.append({"id": item_id, "kind": "answered", "at": str(data.get("answered_at", "")),
                            "text": str(data.get("answer", ""))})
        elif status == "cancelled" and not _done(note, ("Relayed", "Acknowledged")):
            out.append({"id": item_id, "kind": "cancelled", "at": str(data.get("cancelled_at", "")),
                        "text": str(data.get("cancel_note", ""))})
        elif status == "wont_do" and not _done(note, ("Relayed", "Acknowledged")):
            wd = data.get("wont_do") if isinstance(data.get("wont_do"), dict) else {}
            out.append({"id": item_id, "kind": "wont_do", "at": str(wd.get("at", "")),
                        "text": str(wd.get("note", "")).strip() or "no reason given"})
        pk = data.get("parked")
        ups = data.get("updates") if isinstance(data.get("updates"), list) else []
        if status == "open" and isinstance(pk, dict) and pk.get("until") and _due(pk["until"], now):
            n = sum(1 for u in ups if isinstance(u, dict) and str(u.get("at", "")) > str(pk.get("at", "")))
            out.append({"id": item_id, "kind": "owner_unpark_due", "at": str(pk["until"]),
                        "text": f"{n} updates since parked", "updates": n})
        if status == "open" and isinstance(pk, dict) and pk.get("choice") == "clarify" and not pk.get("relayed_at"):
            answered = any(isinstance(u, dict) and str(u.get("at", "")) > str(pk.get("at", "")) for u in ups)
            if not answered:
                cl = pk.get("clarify") if isinstance(pk.get("clarify"), dict) else {}
                dim, opt = str(cl.get("dimension") or "general"), cl.get("option")
                out.append({"id": item_id, "kind": "clarify_request", "at": str(pk.get("at", "")),
                            "text": f"{dim} ({opt})" if opt else "general"})
        for field in ("status_request", "resume_request"):
            req = data.get(field)
            if status == "open" and isinstance(req, dict) and req.get("at"):
                out.append({"id": item_id, "kind": field, "at": str(req["at"]), "text": str(data.get("title", ""))})
        req = data.get("brief_request")
        if status == "open" and isinstance(req, dict) and req.get("at") and not data.get("brief"):   # "Ask for a brief"; cleared once `brief` is set
            out.append({"id": item_id, "kind": "brief_request", "at": str(req["at"]), "text": str(data.get("title", ""))})
        sr_list = data.get("steer_requests")
        for s in sr_list if isinstance(sr_list, list) else []:
            if isinstance(s, dict) and s.get("text") and not s.get("relayed_at"):
                out.append({"id": item_id, "kind": "steer", "at": str(s.get("at", "")), "text": str(s["text"])})
        fb = data.get("feedback")
        if isinstance(fb, dict) and fb.get("status") == "new":
            out.append({"id": item_id, "kind": "feedback", "at": str(fb.get("at", "")),
                        "text": str(fb.get("note", "")), "to": str(fb.get("to", ""))})
    return out


def pending_efforts(docs):
    out = []
    for doc in sorted(docs, key=lambda x: x["id"]):
        d = _d(doc)
        if d.get("status") != "new":
            continue
        bias = d.get("bias_after")
        b = f"{bias:+d}" if isinstance(bias, int) and not isinstance(bias, bool) else "?"
        goal, direction = str(d.get("goal", "")), str(d.get("direction", ""))
        out.append({"id": doc["id"], "kind": "effort", "at": str(d.get("at", "")),
                    "text": f"{goal} effort {direction} (bias {b})", "goal": goal, "direction": direction, "bias": b})
    return out


def pending_il_steers(docs):
    """Steer requests on Intent Ledger tasks (il_steers, status "new"): kind "steer", id = the doc id, with `task` and `session`."""
    out = []
    for doc in sorted(docs, key=lambda x: x["id"]):
        d = _d(doc)
        if d.get("status") == "new" and d.get("text"):
            out.append({"id": doc["id"], "kind": "steer", "at": str(d.get("at", "")), "text": str(d["text"]),
                        "task": str(d.get("task", "")), "session": str(d.get("session", ""))})
    return out


def pending_commands(sessions):
    out = []
    for doc in sorted(sessions, key=lambda x: x["id"]):
        data = _d(doc)
        cmd = data.get("command")
        if isinstance(cmd, dict) and cmd.get("status") == "new":
            ctype = str(cmd.get("type", ""))
            text = cmd.get("text", "") if ctype == "activate" else cmd.get("note", "")
            out.append({"id": doc["id"], "kind": "owner_command", "at": str(cmd.get("at", "")),
                        "text": str(text or ""), "type": ctype, "goal": str(cmd.get("goal", "")),
                        "session": str(data.get("name", ""))})
    return out


def pending_goal_parks(goals):
    out = []
    for doc in sorted(goals, key=lambda x: x["id"]):
        d = _d(doc)
        if d.get("park_ack"):
            continue
        pk, title = d.get("parked"), str(d.get("title", ""))
        if isinstance(pk, dict):
            until = pk.get("until")
            text = f"parked until {until}" if until else f"until unparked: {title}"
            out.append({"id": doc["id"], "kind": "goal_park", "at": str(pk.get("at", "")), "text": text})
        elif d.get("unparked_at"):
            out.append({"id": doc["id"], "kind": "goal_unpark", "at": str(d["unparked_at"]), "text": title})
    return out


def pending_goal_done(goals):
    """A goal the owner marked complete on the page (lifecycle done, done_at set) until the Architect sets done_ack."""
    out = []
    for doc in sorted(goals, key=lambda x: x["id"]):
        d = _d(doc)
        if d.get("lifecycle") == "done" and d.get("done_at") and not d.get("done_ack"):
            note = str(d.get("done_note", "")).strip()
            out.append({"id": doc["id"], "kind": "goal_done", "at": str(d["done_at"]),
                        "text": str(d.get("title", "")) + (f" — {note}" if note else "")})
    return out


def pending(snap, now):
    """Owner events the Architect must act on, from a snapshot with items, sessions, effort_tunes, goals."""
    return (pending_items(snap.get("items", []), now) + pending_commands(snap.get("sessions", []))
            + pending_efforts(snap.get("effort_tunes", [])) + pending_il_steers(snap.get("il_steers", [])) + pending_goal_parks(snap.get("goals", [])) + pending_goal_done(snap.get("goals", [])))

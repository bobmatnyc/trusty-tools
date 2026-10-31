"""datg_ops.py: validate and apply datg write ops (add, update, session, close).

Why: datg.py passed the 500-SLOC cap; #10161 P3 split it by section, bodies moved verbatim.
What: id and option checks, apply_add/update/session/op, collect_closed and write_ops_ev
(one locked, all-or-nothing commit). datg.py re-exports every name.
Test: tests/test_datg.py
"""
import json
import os
import re
import time

from datg_store import (CODE_RE, DEFAULT_BLOCK_AFTER, ID_RE, PREFIXES, SESSION_MAX, STATUS, TITLE_MAX, VAGUE,
                        WHO, DatgError, Locked, commit, load_state, now_iso, paths)

def _title(t):
    t = (t or "").strip()
    if not t or len(t) > TITLE_MAX or "\n" in t:
        raise DatgError(f"title required, one line, <= {TITLE_MAX} chars")
    return t


def _lim(v, name, mx, req=False, ml=False):
    v = ("" if v is None else str(v)).strip()
    if (not ml and "\n" in v) or len(v) > mx or (req and not v):
        raise DatgError(f"{name} must be {'' if ml else 'one line, '}<= {mx} chars" + (", non-empty" if req else ""))
    return v


def _find(st, full_id):
    m = ID_RE.match(full_id or "")
    if not m or m.group(1) != st["code"]:
        raise DatgError(f"bad id {full_id!r}; expected {st['code']}-<G|D|A|T><hex2>")
    pre = m.group(2)
    bucket = {"G": "goals", "D": "items", "A": "items", "T": "tasks"}[pre]
    for o in st[bucket]:
        if o.get("id") == full_id:
            return pre, o
    raise DatgError(f"unknown id {full_id}")


def _all_ids(st):
    return {o.get("id") for k in ("goals", "items", "tasks") for o in st[k]}


def _next_id(st, pre, explicit=None):
    used = _all_ids(st)
    if explicit:
        m = ID_RE.match(explicit)
        if not m or m.group(1) != st["code"] or m.group(2) != pre:
            raise DatgError(f"bad id {explicit!r} for {pre}")
        if explicit in used:
            raise DatgError(f"id collision: {explicit} exists")
        return explicit
    # Why: ids are recycled (the owner 2026-10-24); scan from the pointer, wrap ff -> 01.
    # What: first free id in pointer..ff then 01..pointer-1; pointer = next hex after it, ff -> 01.
    # Test: test_datg.py NextIdCase
    start = int(st["next"].get(pre, "01"), 16) or 1
    order = list(range(start, 256)) + list(range(1, start))
    n = next((i for i in order if f"{st['code']}-{pre}{i:02x}" not in used), None)
    if n is None:
        raise DatgError(f"no free {pre} id (01-ff exhausted)")
    st["next"][pre] = f"{n % 255 + 1:02x}"
    return f"{st['code']}-{pre}{n:02x}"


def _check_goal(st, goal):
    m = ID_RE.match(goal or "")
    if not m or m.group(2) != "G" or goal not in {g["id"] for g in st["goals"]}:
        raise DatgError(f"goal {goal!r} is not a goal of this project")
    g = [g for g in st["goals"] if g["id"] == goal][0]
    if g["status"] != "active":
        raise DatgError(f"goal {goal} is {g['status']}, not active")


def _opts(s):
    out = []
    for p in (s or "").split(","):
        p = p.strip()
        if p:
            out.append({"l": p.rstrip("*"), "rec": p.endswith("*")})
    return out


def _check_opts(opts):
    opts = opts or []
    if len(opts) > 4 or any(len(o.get("l", "")) > 160 for o in opts):
        raise DatgError("opts: at most 4 options, each label <= 160 chars")
    return opts


def op_init(project, code):
    if not CODE_RE.match(code or ""):
        raise DatgError("code must be 2-8 chars, lowercase letters/digits, letter first")
    _, sp, _ = paths(project)
    with Locked(project):
        if os.path.exists(sp):
            raise DatgError("state.json exists; init refuses to overwrite")
        st = {"code": code, "v": 0, "config": {"block_after": DEFAULT_BLOCK_AFTER},
              "goals": [], "items": [], "tasks": [],
              "next": {p: "01" for p in PREFIXES}}
        commit(project, st, [{"id": f"{code}-init", "ev": "open", "note": "init"}])
    return f"initialised {code}"


def apply_add(st, a):
    pre = a["kind"]
    if pre not in PREFIXES:
        raise DatgError("kind must be G, D, A or T")
    title = _title(a.get("title"))
    new_id = _next_id(st, pre, a.get("id"))
    if pre == "G":
        rank = a.get("rank")
        if rank is None:
            rank = len(st["goals"]) + 1
        if not isinstance(rank, int) or rank < 1:
            raise DatgError("rank must be an integer >= 1")
        obj = {"id": new_id, "rank": rank, "title": title, "finish": _lim(a.get("finish"), "finish", 160),
               "status": "active", "maps": a.get("maps") or ""}
        st["goals"].append(obj)
    else:
        _check_goal(st, a.get("goal"))
        if pre == "T":
            obj = {"id": new_id, "goal": a["goal"], "title": title, "status": a.get("status") or "open",
                   "to": a.get("to") or "", "outcome": ""}
            if obj["status"] not in STATUS["T"][:2]:
                raise DatgError("new task status must be open or running")
            st["tasks"].append(obj)
        else:
            obj = {"id": new_id, "kind": "question" if pre == "D" else "action", "status": "open",
                   "goal": a["goal"], "title": title, "opts": _check_opts(a.get("opts")),
                   "answer": "", "outcome": "", "esc": None}
            st["items"].append(obj)
    ev = "drift" if pre == "D" and title.lower().startswith("drift:") else "open"
    return {"id": new_id, "ev": ev, "note": title}


def apply_update(st, a):
    pre, o = _find(st, a.get("id"))
    evs = []
    if a.get("title") is not None:
        o["title"] = _title(a["title"])
    if a.get("rank") is not None:
        if pre != "G" or not isinstance(a["rank"], int) or a["rank"] < 1:
            raise DatgError("rank applies to goals, integer >= 1")
        o["rank"] = a["rank"]
        evs.append("reprio")
    if a.get("esc") is not None:
        if pre not in "DA" or a["esc"] not in ("architect", ""):
            raise DatgError("esc applies to D/A, value 'architect' or ''")
        o["esc"] = a["esc"] or None
        if a["esc"]:
            evs.append("escalated")
    if a.get("answer") is not None:
        if pre not in "DA":
            raise DatgError("answer applies to D/A")
        o["answer"] = _lim(a["answer"], "answer", 300, ml=True)
        o["status"] = "answered"
        evs.append("answered")
    if a.get("to") is not None:
        if pre != "T":
            raise DatgError("to applies to tasks")
        o["to"] = a["to"]
    if a.get("outcome") is not None:
        if pre == "G":
            raise DatgError("outcome does not apply to goals")
        o["outcome"] = _lim(a["outcome"], "outcome", 300, ml=True)
    if a.get("status") is not None:
        if a["status"] not in STATUS[pre]:
            raise DatgError(f"status for {pre} must be one of {STATUS[pre]}")
        if a["status"] == "done" and pre != "G" and not o.get("outcome"):
            raise DatgError("done needs a verified one-line outcome")
        o["status"] = a["status"]
        if a["status"] == "done":
            evs.append("done")
    return [{"id": o["id"], "ev": e, "note": a.get("note") or o.get("title", "")} for e in (evs or ["update"])]


def _int(v, name, lo, hi):
    if isinstance(v, bool) or not isinstance(v, int) or not lo <= v <= hi:
        raise DatgError(f"{name} must be an integer {lo}-{hi}")
    return v


def _bad(msg):
    raise DatgError(msg)


def _rows(a, key, mx, name):
    rows = a[key]
    if not isinstance(rows, list) or len(rows) > mx or any(not isinstance(r, dict) for r in rows):
        raise DatgError(f"{name}: a list of at most {mx} objects")
    return rows


REF_KEY = re.compile(r"^[a-z0-9][a-z0-9_.-]{0,31}$")
REFS_MAX = 12


def _merge_refs(old, new):
    """Merge [{key, value}] into the stored refs: same key replaces; value None removes that key; [] clears (session set --ref none)."""
    if not new:
        return []
    refs = {r["key"]: r["value"] for r in old}
    for r in _rows({"refs": new}, "refs", REFS_MAX, "refs"):
        key = r.get("key")
        if not isinstance(key, str) or not REF_KEY.match(key):
            raise DatgError(f"refs.key must match {REF_KEY.pattern}")
        if r.get("value") is None:
            refs.pop(key, None)
            continue
        refs[key] = _lim(r.get("value"), "refs.value", 200, True)
    if len(refs) > REFS_MAX:
        raise DatgError(f"refs: at most {REFS_MAX} refs")
    return [{"key": k, "value": v} for k, v in refs.items()]


def apply_session(st, a):
    """Validate and merge a session-record op into st["session"]; raise DatgError on any violation."""
    ses = dict(st.get("session") or {})
    if a.get("task") is not None:
        ses["task"] = _lim(a["task"], "task", 160, True)
    if a.get("next") is not None:
        ses["next"] = _lim(a["next"], "next", 200, True)
    if a.get("hold") is not None:   # #10579: "" clears it
        ses["hold"] = _lim(a["hold"], "hold", 1000)   # a usage HOLD carries its reset/projection/resume context
    if a.get("merge_hold") is not None:   # #10579: a no-merge hold, never folded into the usage HOLD
        ses["merge_hold"] = _lim(a["merge_hold"], "merge_hold", 1000)
    if "waiting_on" in a:
        ses["waiting_on"] = [
            {"who": r.get("who") if r.get("who") in WHO else _bad(f"waiting_on.who must be one of {WHO}"),
             "what": _lim(r.get("what"), "waiting_on.what", 120, True),
             "since": _lim(r.get("since") or now_iso(), "waiting_on.since", 20)}
            for r in _rows(a, "waiting_on", 5, "waiting_on")]
    if "owed" in a:
        owed = []
        for r in _rows(a, "owed", 8, "owed"):
            exp = _lim(r.get("expect"), "owed.expect", 80, True)
            if exp.lower() in VAGUE:
                raise DatgError(f"owed.expect must be an exact value (SHA, run id, count), not {exp!r}")
            owed.append({"check": _lim(r.get("check"), "owed.check", 80, True), "expect": exp,
                         "ref": _lim(r.get("ref"), "owed.ref", 80)})
        ses["owed"], ses["owed_none"] = owed, not owed
    if "agents" in a:
        ses["agents"] = [{"id": _lim(r.get("id"), "agents.id", 40, True),
                          "goal": _lim(r.get("goal"), "agents.goal", 40),
                          "what": _lim(r.get("what"), "agents.what", 80)}
                         for r in _rows(a, "agents", 6, "agents")]
    if "crons" in a:
        ses["crons"] = [{"id": _lim(r.get("id"), "crons.id", 40, True),
                         "schedule": _lim(r.get("schedule"), "crons.schedule", 40, True),
                         "prompt": _lim(r.get("prompt"), "crons.prompt", 200, True)}
                        for r in _rows(a, "crons", 4, "crons")]
    if "refs" in a:
        ses["refs"] = _merge_refs(ses.get("refs") or [], a["refs"])
    if a.get("boundary"):
        ses["boundary_at"] = now_iso()
    if a.get("ctx_use") is not None:
        c = a["ctx_use"]
        if not isinstance(c, dict):
            raise DatgError("ctx_use must be an object {active, stale, ctx, keep}")
        ses["ctx_use"] = {"at": now_iso(), "active": _int(c.get("active"), "ctx_use.active", 0, 100),
                          "stale": _int(c.get("stale"), "ctx_use.stale", 0, 100),
                          "ctx": _int(c.get("ctx"), "ctx_use.ctx", 0, 10**9),
                          "keep": _lim(c.get("keep"), "ctx_use.keep", 160)}
    probe = dict(ses, written_at=now_iso(), written_epoch=int(time.time()))
    n = len(json.dumps(probe, separators=(",", ":"), ensure_ascii=False).encode())
    if n > SESSION_MAX:
        raise DatgError(f"session record would be {n} B (> {SESSION_MAX} B limit)")
    st["session"] = ses
    return {"id": f"{st['code']}-session", "ev": "update", "note": ses.get("task", "")[:120]}


def apply_op(st, a):
    verb = a.get("op")
    if verb == "add":
        return [apply_add(st, a)]
    if verb == "update":
        return apply_update(st, a)
    if verb == "session":
        return [apply_session(st, a)]
    if verb == "close":
        if not a.get("outcome"):
            raise DatgError("close needs --outcome")
        return apply_update(st, {"id": a.get("id"), "outcome": a["outcome"], "status": "done"})
    raise DatgError(f"unknown op {verb!r}")


def collect_closed(st, evs):
    """Remove items/tasks that a `done` event closed in this call; return [(kind, obj)] to archive."""
    ids = {e["id"] for e in evs if e["ev"] == "done"}
    out = []
    for field, kind in (("items", "item"), ("tasks", "task")):
        out += [(kind, o) for o in st[field] if o.get("id") in ids and o.get("status") == "done"]
        st[field] = [o for o in st[field] if not (o.get("id") in ids and o.get("status") == "done")]
    return out


def write_ops_ev(project, ops):
    """Apply ops to a copy of the state; commit once (all-or-nothing). Returns (events, state)."""
    with Locked(project):
        st = load_state(project)
        evs = []
        for a in ops:
            if not isinstance(a, dict):
                raise DatgError("each op must be an object")
            evs.extend(apply_op(st, a))
        commit(project, st, evs, collect_closed(st, evs))
    return evs, st

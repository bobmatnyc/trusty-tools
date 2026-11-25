#!/usr/bin/env python3
"""il_remind.py: reminders attached to Intent Ledger tasks (stdlib only).

Why: the owner (see #10571): "a reminder should not be a task, but we should have reminders attached to tasks."
A dated "Remind ..." task filled the task list and the WIP count; a reminder is an attribute of the task it serves.
What: a task carries `reminders`, a list of {n, at, every, note, state, fired_at, fires}. `n` is the number within the
task (never reused), `at` is a UTC `YYYY-MM-DDTHH:MMZ` (a bare date means 12:00Z), `every` is optional (`14d`, `36h`,
`2w`), `note` is what to do (160 chars), `state` is pending, fired or done. The store is the task in state.json: no new
file, no service. Pure functions over the state dict; datg.py applies them as ops under its lock and its single commit.
  add / mark_done        the `remind` and `remind-done` ops
  due / fire             the poller's step: a pending reminder whose `at` has passed fires ONCE and becomes `fired`. It
                         stays listed, and in every reply footer, until it is acked with `remind-done`
  mark_done              the ack. A one-shot becomes `done`. A repeating one that fired (or is past due) moves `at`
                         forward by whole `every` steps past now and goes back to `pending`; `end` stops it for good
  unacked / footer_line  fired reminders not yet acked, and the "Reminders (ack): ..." line the reply footer carries
  parse_ack / ack        the owner's chat ack: `ack ex-T84#2`, several ids, or `ack all` (the owner; see #10571)
  next_reminder          the one a dashboard row shows: a fired one first, else the earliest pending
  lines                  digest lines under a task
A task with a pending or fired reminder cannot be closed: move it (a new `remind` on another task, then `remind-done`).
Test: tests/test_il_remind.py
"""
import os
import re
from datetime import datetime, timedelta, timezone

STATES = ("pending", "fired", "done")
NOTE_MAX = 160
MAX_PER_TASK = 8
FMT = "%Y-%m-%dT%H:%MZ"
EVERY_RE = re.compile(r"^(\d{1,4})([hdw])$")
UNIT = {"h": timedelta(hours=1), "d": timedelta(days=1), "w": timedelta(weeks=1)}
DATE_ONLY_HOUR = 12   # a bare date fires at 12:00Z


class RemindError(ValueError):
    """Invalid reminder input; datg.py maps it to DatgError (exit 2)."""


def parse_at(s):
    """`YYYY-MM-DD` (12:00Z), `YYYY-MM-DDTHH:MM[:SS]Z` -> canonical `YYYY-MM-DDTHH:MMZ`; RemindError otherwise."""
    s = str(s or "").strip()
    for f, hour in (("%Y-%m-%d", DATE_ONLY_HOUR), ("%Y-%m-%dT%H:%MZ", None), ("%Y-%m-%dT%H:%M:%SZ", None)):
        try:
            d = datetime.strptime(s, f)
        except ValueError:
            continue
        if hour is not None:
            d = d.replace(hour=hour)
        return d.strftime(FMT)
    raise RemindError(f"at {s!r} must be YYYY-MM-DD or YYYY-MM-DDTHH:MMZ (UTC)")


def parse_every(s):
    """`14d`, `36h`, `2w` -> (canonical text, timedelta); None/empty -> (None, None)."""
    s = str(s or "").strip().lower()
    if not s:
        return None, None
    m = EVERY_RE.match(s)
    if not m or int(m.group(1)) < 1:
        raise RemindError(f"every {s!r} must look like 14d, 36h or 2w")
    step = int(m.group(1)) * UNIT[m.group(2)]
    if step < timedelta(hours=1):
        raise RemindError("every must be at least 1h")
    return s, step


def _dt(s):
    return datetime.strptime(s, FMT).replace(tzinfo=timezone.utc)


def _iso(d):
    return d.strftime(FMT)


def _note(s):
    s = " ".join(str(s or "").split())
    if not s or len(s) > NOTE_MAX:
        raise RemindError(f"note must be one line, 1-{NOTE_MAX} chars")
    return s


def of(task):
    return task.get("reminders") or []


def add(task, at, note, every=None):
    """Attach a reminder to a live task dict; returns (reminder, created). An identical live one is returned, not duplicated."""
    if task.get("status") == "done":
        raise RemindError(f"{task['id']} is done; attach the reminder to a live task")
    at, (ev, _) = parse_at(at), parse_every(every)
    note = _note(note)
    rs = task.setdefault("reminders", [])
    for r in rs:
        if r["state"] != "done" and (r["at"], r.get("every"), r["note"]) == (at, ev, note):
            return r, False
    live = [r for r in rs if r["state"] != "done"]
    if len(live) >= MAX_PER_TASK:
        raise RemindError(f"{task['id']} already has {MAX_PER_TASK} live reminders; finish one first")
    n = max((r["n"] for r in rs), default=0) + 1
    r = {"n": n, "at": at, "every": ev, "note": note, "state": "pending"}
    rs.append(r)
    while len(rs) > 2 * MAX_PER_TASK:   # history of finished ones is bounded; the oldest done goes first, so max n stays
        rs.remove(next(x for x in rs if x["state"] == "done"))
    return r, True


def find(task, n):
    for r in of(task):
        if r["n"] == n:
            return r
    raise RemindError(f"{task['id']} has no reminder #{n}")


def mark_done(task, n, now=None, end=False):
    """Ack (or cancel) reminder #n. Returns (reminder, changed). A repeating reminder that fired or is past due advances to its
    next time and stays pending; `end` (or a repeating one still in the future, i.e. a cancel) finishes it."""
    r = find(task, n)
    if r["state"] == "done":
        return r, False
    now = now or datetime.now(timezone.utc)
    _, step = parse_every(r.get("every"))
    if step is not None and not end and (r["state"] == "fired" or _dt(r["at"]) <= now):
        at = _dt(r["at"])
        r["at"] = _iso(at + step * ((now - at) // step + 1))
        r["state"] = "pending"
        r["acked_at"] = _iso(now)
        return r, True
    r["state"] = "done"
    return r, True


def live(task):
    return [r for r in of(task) if r["state"] != "done"]


def due(st, now):
    """[(task, reminder)] pending and past `at` at datetime `now`, oldest first."""
    out = [(t, r) for t in st["tasks"] for r in of(t) if r["state"] == "pending" and _dt(r["at"]) <= now]
    return sorted(out, key=lambda p: (p[1]["at"], p[0]["id"], p[1]["n"]))


def fire(r, now):
    """Mark one due reminder fired. It stays fired until mark_done acks it (a repeating one then advances `at`)."""
    r["fired_at"] = _iso(now)
    r["fires"] = r.get("fires", 0) + 1
    r["state"] = "fired"


def next_reminder(task):
    """A fired (unacknowledged) reminder first, else the earliest pending one; None when the task has neither."""
    fired = [r for r in of(task) if r["state"] == "fired"]
    pend = [r for r in of(task) if r["state"] == "pending"]
    return (min(fired, key=lambda r: r["at"]) if fired else min(pend, key=lambda r: r["at"]) if pend else None)


def label(r):
    return f"#{r['n']} {r['at']}" + (f" every {r['every']}" if r.get("every") else "") + f" [{r['state']}] {r['note']}"


def lines(task, indent="    "):
    """Digest lines for the task's live reminders, soonest first."""
    return [f"{indent}remind {label(r)}" for r in sorted(live(task), key=lambda r: r["at"])]


def dashboard_field(task):
    """The `reminder` value a dashboard task row shows, or None: {at, note, state, n, every?}."""
    r = next_reminder(task)
    if r is None:
        return None
    out = {"at": r["at"], "note": r["note"], "state": r["state"], "n": r["n"]}
    if r.get("every"):
        out["every"] = r["every"]
    return out


def _row(task, r):
    return {"task": task["id"], "title": task.get("title", ""), "n": r["n"], "at": r["at"], "every": r.get("every"),
            "note": r["note"], "state": r["state"], "fired_at": r.get("fired_at")}


def listing(st, now, due_only=False, with_done=False):
    """Rows for `datg.py reminders`, soonest first. due_only: pending past their time, plus fired ones not yet marked done."""
    rows = []
    for t in st["tasks"]:
        for r in of(t):
            if r["state"] == "done" and not with_done:
                continue
            if due_only and not (r["state"] == "fired" or (r["state"] == "pending" and _dt(r["at"]) <= now)):
                continue
            rows.append(_row(t, r))
    return sorted(rows, key=lambda x: (x["at"], x["task"], x["n"]))


def fmt_row(x):
    return f"{x['task']}#{x['n']} {x['at']} every={x['every'] or '-'} [{x['state']}] {x['note']}"


def step(project, now=None, dry_run=False):
    """The poller's step: fire every due reminder once, in one datg commit. Returns one row per firing (rows carry the task title).
    dry_run reads only. A reminder fires again only after its repeat moved `at` past now: the op re-checks `at` under datg's lock."""
    import datg   # lazy: datg imports this module
    now = now or datetime.now(timezone.utc)
    st = datg.load_state(project)
    pairs = due(st, now)
    if dry_run or not pairs:
        return [_row(t, r) for t, r in pairs]
    keys = [[t["id"], r["n"], r["at"]] for t, r in pairs]
    evs, _ = datg.write_ops_ev(project, [{"op": "remind_fire", "keys": keys, "now": _iso(now)}])
    return [dict(_row({"id": e["id"], "title": e["task"]}, e["reminder"]), at=e["reminder"]["fired_for"]) for e in evs if e.get("reminder")]


# ---- surfacing and ack (see #10571; the owner: "the reminders should be surfaced with your response and me to ack") ----

FOOTER_MAX = 5
FOOTER_NOTE = 60
ACK_RE = re.compile(r"^\s*ack\s+(.+?)\s*$", re.I)
ID_N = re.compile(r"^([a-z]{2,6}-T[0-9a-z]{2,4})#(\d{1,3})$", re.I)


def il_dir():
    """The Architect's Intent Ledger directory; ARCHITECT_IL_DIR overrides it (tests)."""
    return os.environ.get("ARCHITECT_IL_DIR") or os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "local", "il")


def unacked(st, now):
    """Rows (the `reminders` listing shape) of reminders fired and not acked, or pending and already past due (the poller has
    not fired them yet), oldest first. Done and future ones are not here."""
    return listing(st, now, due_only=True)


def key(x):
    return f"{x['task']}#{x['n']}"


def footer_line(rows):
    """`Reminders (ack): ex-T84#2 note \u00b7 ex-T6e#1 note`, or "" when there are none. At most FOOTER_MAX, then `+N more`."""
    if not rows:
        return ""
    parts = [f"{key(x)} {x['note'][:FOOTER_NOTE]}" for x in rows[:FOOTER_MAX]]
    if len(rows) > FOOTER_MAX:
        parts.append(f"+{len(rows) - FOOTER_MAX} more")
    return "Reminders (ack): " + " \u00b7 ".join(parts)


def parse_ack(text):
    """The owner's chat ack: `ack ex-T84#2`, `ack ex-T84#2 ex-T6e#1` (spaces or commas), `ack all`. Returns "all", a list of
    (task id, n) pairs, or None when the text is not an ack. Raises RemindError for an `ack` whose words are not ids."""
    m = ACK_RE.match(str(text or ""))
    if not m:
        return None
    words = [w for w in re.split(r"[\s,]+", m.group(1).strip()) if w]
    if len(words) == 1 and words[0].lower() == "all":
        return "all"
    out = []
    for w in words:
        k = ID_N.match(w)
        if not k:
            raise RemindError(f"ack {w!r}: expected <task>#<n> (e.g. ex-T84#2) or `all`")
        out.append((k.group(1), int(k.group(2))))
    return out


def ack(project, text, now=None):
    """Apply a chat ack to the ledger at `project` in one datg commit. Returns (acked keys, problems). Not an ack text: ([], []).
    An id that is not an unacked reminder is a problem and is left alone; the rest are acked."""
    import datg   # lazy: datg imports this module
    try:
        want = parse_ack(text)
    except RemindError as e:
        return [], [str(e)]
    if want is None:
        return [], []
    now = now or datetime.now(timezone.utc)
    rows = unacked(datg.load_state(project), now)
    have = {(x["task"].lower(), x["n"]): x for x in rows}
    if want == "all":
        hit, problems = rows, ([] if rows else ["ack all: no reminder is waiting for an ack"])
    else:
        hit, problems = [], []
        for tid, n in want:
            x = have.get((tid.lower(), n))
            (hit.append(x) if x else problems.append(f"ack {tid}#{n}: no such unacked reminder"))
    if hit:
        datg.write_ops_ev(project, [{"op": "remind_done", "id": x["task"], "n": x["n"], "now": _iso(now)} for x in hit])
    return [key(x) for x in hit], problems

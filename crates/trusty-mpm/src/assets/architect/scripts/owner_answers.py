"""owner_answers.py: items in the dashboard store that need the Architect to act.

Why: the owner answered 16 dashboard items and the Architect relayed none for up to 4 hours,
because nothing woke it on an answer. fleet-poll.py and pending-answers.py share this reader.
What: pending(root) returns [{id, kind, at, text}] for every item in <root>/items that is
status "answered" with an answer_note not starting "Relayed", or status "cancelled" with an
answer_note starting neither "Relayed" nor "Acknowledged". `at` is answered_at / cancelled_at.
An item with status "wont_do" (the Won't do button) and an answer_note starting neither "Relayed" nor
"Acknowledged" yields {id, kind: "wont_do", at: wont_do.at, text: the reason, or "no reason given"}.
An item whose feedback.status is "new" (the Feedback button: reclassify and/or a note) also yields
{id, kind: "feedback", at: feedback.at, text: note, to: feedback.to}, whatever the item's status.
An open item with parked.until in the past also yields {id, kind: "owner_unpark_due", at: until,
text: "<n> updates since parked", updates: n}; unparking (parked cleared) stops it.
A session doc (<root>/sessions) whose command.status is "new" (the Activate, Prioritize, Park and Restart
buttons) yields {id, kind: "owner_command", at: command.at, text, type, goal}; text is the activate text
or the note (prioritize, park, restart). The Architect sets command.status "applied" once it has relayed and recorded it.
A doc in <root>/effort_tunes with status "new" (the More/Less Effort goal tuner) yields {id, kind: "effort",
at, text: "<goal> effort <direction> (bias <+n>)", goal, direction, bias}; the Architect sets status "applied" once handled.
An open item whose status_request is a dict with `at` (the Status Update button) also yields {id, kind: "status_request",
at: status_request.at, text: title}; the Architect answers by writing status_note + status_at and setting status_request null.
Raises OSError on an unreadable store directory; an unreadable single item is skipped.
Test: scripts/test_fleet_poll_owner_answer.py
"""
import json
import os
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402


def _done(note, prefixes):
    return isinstance(note, str) and note.lstrip().startswith(prefixes)


def _due(until, now):
    try:
        t = datetime.fromisoformat(str(until).replace("Z", "+00:00"))
    except ValueError:
        return False
    if t.tzinfo is None:
        t = t.replace(tzinfo=timezone.utc)
    return t <= now


def pending(root=None, now=None):
    now = now or datetime.now(timezone.utc)
    cdir = os.path.join(root or ds.DEFAULT_ROOT, "items")
    out = []
    try:
        names = sorted(n for n in os.listdir(cdir) if n.endswith(".json") and not n.startswith("."))
    except FileNotFoundError:
        names = []
    for n in names:
        try:
            with open(os.path.join(cdir, n), encoding="utf-8") as f:
                raw = json.load(f)
        except (OSError, ValueError):
            continue
        data = raw.get("data", raw) if isinstance(raw, dict) else {}
        if not isinstance(data, dict):
            continue
        status, note = data.get("status"), data.get("answer_note")
        item_id = n[:-5]
        if status == "answered" and not _done(note, ("Relayed",)):
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
        if status == "open" and isinstance(pk, dict) and pk.get("until") and _due(pk["until"], now):
            ups = data.get("updates")
            n = sum(1 for u in ups if isinstance(u, dict) and str(u.get("at", "")) > str(pk.get("at", ""))) if isinstance(ups, list) else 0
            out.append({"id": item_id, "kind": "owner_unpark_due", "at": str(pk["until"]),
                        "text": f"{n} updates since parked", "updates": n})
        sr = data.get("status_request")
        if status == "open" and isinstance(sr, dict) and sr.get("at"):
            out.append({"id": item_id, "kind": "status_request", "at": str(sr["at"]),
                        "text": str(data.get("title", ""))})
        fb = data.get("feedback")
        if isinstance(fb, dict) and fb.get("status") == "new":
            out.append({"id": item_id, "kind": "feedback", "at": str(fb.get("at", "")),
                        "text": str(fb.get("note", "")), "to": str(fb.get("to", ""))})
    out.extend(_commands(root or ds.DEFAULT_ROOT))
    out.extend(_efforts(root or ds.DEFAULT_ROOT))
    return out


def _efforts(root):
    cdir = os.path.join(root, "effort_tunes")
    try:
        names = sorted(n for n in os.listdir(cdir) if n.endswith(".json") and not n.startswith("."))
    except FileNotFoundError:
        return []
    out = []
    for n in names:
        try:
            with open(os.path.join(cdir, n), encoding="utf-8") as f:
                raw = json.load(f)
        except (OSError, ValueError):
            continue
        d = raw.get("data", raw) if isinstance(raw, dict) else {}
        if not isinstance(d, dict) or d.get("status") != "new":
            continue
        bias = d.get("bias_after")
        b = f"{bias:+d}" if isinstance(bias, int) and not isinstance(bias, bool) else "?"
        goal, direction = str(d.get("goal", "")), str(d.get("direction", ""))
        out.append({"id": n[:-5], "kind": "effort", "at": str(d.get("at", "")),
                    "text": f"{goal} effort {direction} (bias {b})", "goal": goal,
                    "direction": direction, "bias": b})
    return out


def _commands(root):
    cdir = os.path.join(root, "sessions")
    try:
        names = sorted(n for n in os.listdir(cdir) if n.endswith(".json") and not n.startswith("."))
    except FileNotFoundError:
        return []
    out = []
    for n in names:
        try:
            with open(os.path.join(cdir, n), encoding="utf-8") as f:
                raw = json.load(f)
        except (OSError, ValueError):
            continue
        data = raw.get("data", raw) if isinstance(raw, dict) else {}
        cmd = data.get("command") if isinstance(data, dict) else None
        if isinstance(cmd, dict) and cmd.get("status") == "new":
            ctype = str(cmd.get("type", ""))
            text = cmd.get("text", "") if ctype == "activate" else cmd.get("note", "")
            out.append({"id": n[:-5], "kind": "owner_command", "at": str(cmd.get("at", "")),
                        "text": str(text or ""), "type": ctype, "goal": str(cmd.get("goal", "")),
                        "session": str(data.get("name", ""))})
    return out

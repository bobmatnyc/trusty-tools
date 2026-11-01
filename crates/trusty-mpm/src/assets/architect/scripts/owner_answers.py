"""owner_answers.py: items in the dashboard store that need the Architect to act.

Why: the owner answered 16 dashboard items and the Architect relayed none for up to 4 hours,
because nothing woke it on an answer. fleet-poll.py and pending-answers.py share this reader.
What: pending(root) returns [{id, kind, at, text}] for every item in <root>/items that is
status "answered" with an answer_note not starting "Relayed", or status "cancelled" with an
answer_note starting neither "Relayed" nor "Acknowledged". `at` is answered_at / cancelled_at.
An item (answered, cancelled or wont_do) with a non-empty `relayed` field ("<iso> <session>", written by the Architect)
counts as relayed, the same as a note starting "Relayed"; it is not listed.
An item with status "wont_do" (the Won't do button) and an answer_note starting neither "Relayed" nor
"Acknowledged" yields {id, kind: "wont_do", at: wont_do.at, text: the reason, or "no reason given"}.
An item whose feedback.status is "new" (the Feedback button: reclassify and/or a note) also yields
{id, kind: "feedback", at: feedback.at, text: note, to: feedback.to}, whatever the item's status.
An open item with parked.until in the past also yields {id, kind: "owner_unpark_due", at: until,
text: "<n> updates since parked", updates: n}; unparking (parked cleared) stops it.
An open item whose resume_request is a dict with `at` (the Resume button on a parked item) yields
{id, kind: "resume_request", at: resume_request.at, text: title}; the Architect unparks it (parked null,
resume_request null) and relays to the PM.
An open item whose brief_request is a dict with `at` and that has no `brief` (the "Ask for a brief" button on a decision or ask)
yields {id, kind: "brief_request", at: brief_request.at, text: title}; the Architect writes the brief, sets `brief` to
"<project>/docs/briefs/<file>.md" on the item, and the request stops being reported. A goal doc with brief_request {at} and no `brief`
yields the same kind with the goal id. "Refresh briefing" (item or goal that already has `brief`) writes brief_request {at, by, refresh: true}
and is reported until the Architect regenerates the brief and sets brief_request to null.
Each `steer_requests` entry {text, at, by} on a task without `relayed_at` (the Steer button) yields
{id, kind: "steer", at: entry.at, text: entry.text}, whatever the item's status. The Architect relays it to the
owning PM as `User instruction (the owner): <text>`, then sets that entry's relayed_at (ISO time) with
`dashboard-db.py update items <id> --json '{"steer_requests": [...all entries, this one with relayed_at...]}' --if-version <v>`.
A doc in <root>/il_steers with status "new" (the Steer button on an Intent Ledger task row in the Architect IL pane) yields
{id: doc id, kind: "steer", at, text, task: the IL task id, session}. The Architect relays it to that session's PM as
`User instruction (the owner): <text>` and sets status "relayed" (and relayed_at) with `dashboard-db.py update il_steers <id> --json '{...}'`.
A session doc (<root>/sessions) whose command.status is "new" (the Activate, Prioritize, Park and Restart
buttons) yields {id, kind: "owner_command", at: command.at, text, type, goal}; text is the activate text
or the note (prioritize, park, restart). The Architect sets command.status "applied" once it has relayed and recorded it.
A doc in <root>/effort_tunes with status "new" (the More/Less Effort goal tuner) yields {id, kind: "effort",
at, text: "<goal> effort <direction> (bias <+n>)", goal, direction, bias}; the Architect sets status "applied" once handled.
An open item whose status_request is a dict with `at` (the Status Update button) also yields {id, kind: "status_request",
at: status_request.at, text: title}; the Architect answers by writing status_note + status_at and setting status_request null.
An open item whose parked.choice is "clarify" (a marker chip or Explain on the dashboard; parked.until is null)
yields {id, kind: "clarify_request", at: parked.at, text: "<dimension> (<option>)", or "general" when option is null
and dimension is general}, until a `updates` entry is dated after parked.at or parked.relayed_at is set. The
Architect, once it has relayed the question to the PM, sets parked.relayed_at (ISO time) with
`dashboard-db.py update items <id> --json '{"parked": {...existing parked..., "relayed_at": "<iso>"}}' --if-version <v>`;
a new park builds a new parked object, so it alerts again.
A doc in <root>/goals with park_ack null after a park or unpark write yields {id, kind: "goal_park", at: parked.at,
text: "parked until <date>", or "until unparked: <title>" when parked.until is null} (parked is a dict), or
{id, kind: "goal_unpark", at: unparked_at, text: title} (parked null, unparked_at set). The Architect sets
park_ack to an ISO time once handled; a new park or unpark write sets it null again.
A goal the owner marked complete (lifecycle "done", done_at set, done_ack unset) yields {id, kind: "goal_done", at: done_at,
text: "<title>" or "<title> — <done_note>"}; the Architect sets done_ack to an ISO time once handled.
Reads only through dashboard_store (either format); the rules live in dashboard_derive. Raises StoreError on an unreadable collection.
Qualified ids (ar-T44): resolve_id(root, ref) maps "S0c:D01" or a bare id to the local id (NotFound when the S id is not this store's own session); qualified(root, entries) adds a derived `qid` ("<S id>:<id>", bare when unknown) to pending entries, so a relay or log line can name the
entry fleet-wide. Stored ids stay bare.
Test: tests/test_dashboard_derive.py, tests/test_dashboard_qualified.py
"""
import os
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_derive as derive  # noqa: E402
import dashboard_qual as dq  # noqa: E402
import dashboard_store as ds  # noqa: E402

COLLECTIONS = ("items", "sessions", "effort_tunes", "il_steers", "goals")


def pending(root=None, now=None, colls=COLLECTIONS):
    """colls limits which collections are read: a caller that wants only item answers (#10670) is not stopped by a bad document elsewhere."""
    now = now or datetime.now(timezone.utc)
    store = ds.Store(root or ds.DEFAULT_ROOT)
    return derive.pending(derive.snapshot(store, colls), now)


def resolve_id(root, ref):
    return dq.resolve(ds.Store(root or ds.DEFAULT_ROOT), "items", ref)


def qualified(root, entries):
    sid = dq.store_sid(ds.Store(root or ds.DEFAULT_ROOT))
    return [dict(e, qid=e["id"] if e.get("kind") == "owner_command" else dq.qualify(sid, e["id"])) for e in entries]

#!/usr/bin/env python3
"""dashboard_updates.py: deterministic ingester of task update snippets into dashboard items (stdlib only, no model call).

Why: owner 2026-11-12 17:00Z: "capture update snippets directly from tagged tasks in the live chat ... deterministic better".
What: reads four sources and appends {at, by, text} entries to the `updates` array of the dashboard item whose `task` field
names the tagged IL task.
  - inbox/relays.jsonl: {ts, session, kind, text}; a relay belongs to a task when its text carries `Task: <id>[@MMDD]`, or
    else a bare IL id with the `ar-` prefix (`ar-T24`). A bare `T24` never matches (dashboard and IL ids differ).
  - inbox/task-notes.jsonl: {ts, task, by, text}, the Architect's own work notes; append with
    `python3 scripts/dashboard_updates.py note <task-id> "<text>" [--by <who>]` (appends one line, then ingests).
  - inbox/mcp-messages.jsonl: a PM handback ends with `Task: <id>@MMDD | done: <x> | est: <x> | remaining: <x> | pct: <n>`;
    the entry is `done: <x> (pct <n>)`.
  - the dashboard items themselves (item_entries): every non-task item with a `task` field writes onto that task's item an
    owner answer ("<owner> answered <id>: <answer>[: <note>]" at answered_at, by owner; <owner> is owner_name in
    local/architect.json, else "Owner"), a relay ("<id> relayed to <session>" at
    relayed_at, by architect) and a done outcome ("<id> done: <outcome>" at done_at). A changed answer has a new answered_at,
    so it adds an entry and keeps the old one. stale_notes()/annotate_stale() compute `note_stale` and `note_stale_by` at read time.
Only the item of kind "task" for an IL task receives entries; with several, the one not done, then the oldest `created`.
Idempotent with no cursor: an entry's `at` is the source line's timestamp and its text is a pure function of the line, so an
entry already present on the item (same at and text) is never added again. Each text is cut to its first paragraph and
MAX_CHARS. Untagged lines, unknown task ids and lines older than `since_hours` are skipped silently. Writes go through
dashboard_store.Store.update with if_version, so versioning and the change log hold; a version conflict is retried next
call. fleet-poll.py calls ingest() once per cycle (no new daemon, cron or launchd job).
Test: tests/test_dashboard_updates.py.
"""
import json
import os
import threading
import re
import sys
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import architect_config  # noqa: E402
import dashboard_store as ds  # noqa: E402

MAX_CHARS = 400
SINCE_HOURS = 72.0
TAG = re.compile(r"\bTask:\s*((?:ar-)?T[0-9a-km-np-z]+)(?:@\d{4})?(?![0-9a-zA-Z])")
BARE = re.compile(r"(?<![0-9a-zA-Z-])ar-(T[0-9a-km-np-z]+)(?![0-9a-zA-Z])")
HANDBACK = re.compile(
    r"^Task:\s*((?:ar-)?T[0-9a-km-np-z]+)(?:@\d{4})?\s*\|\s*done:\s*(.*?)\s*\|\s*est:.*?\|\s*remaining:.*?\|\s*pct:\s*(\d{1,3})\s*$")


def _norm(task_id):
    t = str(task_id or "").strip().lower()
    return t[3:] if t.startswith("ar-") else t


def one_paragraph(text, limit=MAX_CHARS):
    """First paragraph, whitespace collapsed, cut to `limit` characters with an ellipsis."""
    first = re.split(r"\n\s*\n", str(text).strip())[0]
    s = " ".join(first.split())
    return s if len(s) <= limit else s[: limit - 1].rstrip() + "…"


def _lines(path):
    try:
        f = open(path, encoding="utf-8")
    except OSError:
        return
    with f:
        for raw in f:
            try:
                row = json.loads(raw)
            except ValueError:
                continue
            if isinstance(row, dict):
                yield row


def parse_handback(text):
    """(task id, done text, pct) from the last non-empty line of `text`, or None."""
    rows = [x for x in str(text).splitlines() if x.strip()]
    m = HANDBACK.match(rows[-1].strip()) if rows else None
    return (m.group(1), m.group(2), int(m.group(3))) if m else None


def relay_entry(row):
    """One relays.jsonl row to (task id, at, by, text), or None when untagged."""
    text, ts = row.get("text"), row.get("ts")
    m = (TAG.search(text) or BARE.search(text)) if isinstance(text, str) else None
    if not (m and ts):
        return None
    return m.group(1), ts, "architect relay to %s" % (row.get("session") or "session"), one_paragraph(text)


def message_entry(row):
    """One mcp-messages.jsonl row to (task id, at, by, text), or None when it is not a handback.
    A structured-block parser is added beside parse_handback here; the store and merge logic stay unchanged."""
    hb = parse_handback(row.get("text")) if isinstance(row.get("text"), str) else None
    if not (hb and row.get("ts")):
        return None
    return hb[0], row["ts"], str(row.get("sender") or "pm"), one_paragraph("done: %s (pct %d)" % (hb[1], hb[2]))


def note_entry(row):
    """One task-notes.jsonl row to (task id, at, by, text), or None when a field is missing."""
    task, ts, text = row.get("task"), row.get("ts"), row.get("text")
    if not (task and ts and isinstance(text, str) and text.strip()):
        return None
    return str(task).strip(), ts, str(row.get("by") or "architect"), one_paragraph(text)


def candidates(inbox):
    """Yield (task id, at, by, text) from relays, handbacks and task notes, in file order."""
    for name, parse in (("relays.jsonl", relay_entry), ("mcp-messages.jsonl", message_entry),
                        ("task-notes.jsonl", note_entry)):
        for r in _lines(os.path.join(inbox, name)):
            e = parse(r)
            if e:
                yield e


def owner_label():
    """owner_name from local/architect.json, else "Owner" (#10571 S2: the name is instance data, never a constant here).
    An unreadable config is reported on stderr and the label is "Owner"; the ingest itself still runs."""
    try:
        return architect_config.load().owner_name or "Owner"
    except architect_config.ConfigError as e:
        print("dashboard_updates: %s; answers are labelled Owner" % e, file=sys.stderr)
        return "Owner"


def item_entries(docs, owner=None):
    """Yield (task id, at, by, text) from the answer, relay and done state of every non-task item that names an IL task."""
    owner = owner or owner_label()
    for d in docs:
        x = d["data"]
        if x.get("kind") == "task" or not x.get("task"):
            continue
        i, tid = d["id"], x["task"]
        if x.get("answered_at"):
            text = "%s answered %s: %s" % (owner, i, x.get("answer") or "")
            if x.get("answer_note"):
                text += ": %s" % x["answer_note"]
            yield tid, x["answered_at"], "owner", one_paragraph(text)
        if x.get("relayed_at"):
            yield tid, x["relayed_at"], "architect", one_paragraph("%s relayed to %s" % (i, x.get("session") or "session"))
        if x.get("status") == "done" and x.get("outcome") and x.get("done_at"):
            yield tid, x["done_at"], "architect", one_paragraph("%s done: %s" % (i, x["outcome"]))


def stale_notes(docs):
    """{task item id: [decision ids]}: non-task items answered after the task's status note (status_at, else updated).
    `updated` alone would be bumped by every ingest, so the note time is status_at when the task has one."""
    tasks = pick_task_items(docs)
    out = {}
    for d in docs:
        x = d["data"]
        t = tasks.get(_norm(x.get("task"))) if x.get("task") and x.get("kind") != "task" else None
        if t is None or not x.get("answered_at"):
            continue
        note_at = str(t["data"].get("status_at") or t["data"].get("updated") or "")
        if str(x["answered_at"]) > note_at:
            out.setdefault(t["id"], []).append(d["id"])
    return out


def annotate_stale(docs):
    """Copies of `docs` where a task with a stale note carries data.note_stale = True and data.note_stale_by = [ids]."""
    stale = stale_notes(docs)
    return [dict(d, data=dict(d["data"], note_stale=True, note_stale_by=sorted(stale[d["id"]]))) if d["id"] in stale else d
            for d in docs]


def pick_task_items(docs):
    """{normalised IL task id: the one kind=task item that receives its entries}: not done first, then oldest created."""
    best = {}
    for d in docs:
        data = d["data"]
        if data.get("kind") != "task" or not data.get("task"):
            continue
        key = (data.get("status") == "done", str(data.get("created") or "~"))
        tid = _norm(data["task"])
        if tid not in best or key < best[tid][0]:
            best[tid] = (key, d)
    return {t: v[1] for t, v in best.items()}


def add_note(task, text, by="architect", inbox=None, now=None):
    """Append one line to inbox/task-notes.jsonl and return the line."""
    inbox = inbox or os.path.join(os.path.dirname(HERE), "inbox")
    now = now or datetime.now(timezone.utc)
    row = {"ts": now.strftime("%Y-%m-%dT%H:%M:%SZ"), "task": task, "by": by, "text": text}
    os.makedirs(inbox, exist_ok=True)
    with open(os.path.join(inbox, "task-notes.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps(row) + "\n")
    return row


def ingest(store_root=ds.DEFAULT_ROOT, inbox=None, now=None, since_hours=SINCE_HOURS):
    """Append new update entries. Returns {added: {item id: n}, conflicts: [item id], errors: [text]}."""
    inbox = inbox or os.path.join(os.path.dirname(HERE), "inbox")
    now = now or datetime.now(timezone.utc)
    floor = (now - timedelta(hours=since_hours)).strftime("%Y-%m-%dT%H:%M:%SZ")
    out = {"added": {}, "conflicts": [], "errors": []}
    st = ds.Store(store_root, actor="poller")
    try:
        docs = st.list_docs("items")
    except ds.StoreError as e:
        out["errors"].append(str(e))
        return out
    by_task = pick_task_items(docs)
    new = {}
    for tid, at, by, text in list(candidates(inbox)) + list(item_entries(docs)):
        d = by_task.get(_norm(tid))
        if d is None or str(at) < floor:
            continue
        have = {(u.get("at"), u.get("text")) for u in d["data"].get("updates") or [] if isinstance(u, dict)}
        mine = new.get(d["id"], [])
        if (at, text) in have or any((u["at"], u["text"]) == (at, text) for u in mine):
            continue
        new[d["id"]] = mine
        mine.append({"at": at, "by": by, "text": text})
    for item_id, adds in new.items():
        d = next(x for x in docs if x["id"] == item_id)
        merged = list(d["data"].get("updates") or []) + adds
        try:
            st.update("items", item_id, {"updates": merged}, if_version=d["version"])
            out["added"][item_id] = len(adds)
        except (ds.Conflict, ds.NotFound):
            out["conflicts"].append(item_id)
        except (ds.StoreError, OSError) as e:
            out["errors"].append("items/%s: %s" % (item_id, e))
    return out


_CHECK_LOCK = threading.Lock()


def check_item(store_root, item_id, inbox=None, now=None, timeout=4.0):
    """One ingest pass for the page's "Check for updates" button. Returns {added: n new updates on item_id, pending: bool, errors: [...]}.

    Idempotent (ingest skips an update already on the item). One pass runs at a time: a second caller while one runs waits for it, so
    two clicks never race. Gives up waiting after `timeout` seconds and answers pending=True; the pass still finishes in the background.
    """
    box = {}

    def run():
        with _CHECK_LOCK:
            box["out"] = ingest(store_root, inbox, now)

    t = threading.Thread(target=run, daemon=True)
    t.start()
    t.join(timeout)
    out = box.get("out")
    if out is None:
        return {"added": 0, "pending": True, "errors": []}
    return {"added": int(out["added"].get(item_id, 0)), "pending": False, "errors": out["errors"]}


def main(argv):
    root = os.environ.get("DASHBOARD_ROOT") or ds.DEFAULT_ROOT
    if argv and argv[0] == "note":
        rest, by = argv[1:], "architect"
        if "--by" in rest:
            i = rest.index("--by")
            by = rest[i + 1] if i + 1 < len(rest) else ""
            rest = rest[:i] + rest[i + 2:]
        if len(rest) != 2 or not by:
            print('usage: dashboard_updates.py note <task-id> "<text>" [--by <who>]', file=sys.stderr)
            return 2
        add_note(rest[0], rest[1], by, inbox=os.environ.get("DASHBOARD_INBOX"))
    print(json.dumps(ingest(root, os.environ.get("DASHBOARD_INBOX"))))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

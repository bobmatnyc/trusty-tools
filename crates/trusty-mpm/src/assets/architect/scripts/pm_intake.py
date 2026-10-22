#!/usr/bin/env python3
"""PM-message intake: PMs queue messages, the poller wakes the Architect only on some.

Why: every SendMessage from a PM starts a full Architect turn, including acks and
progress notes (the owner D10 2026-10-12: the filter and wake digest are an Architect
feature). A PM that appends to inbox/pm-messages.jsonl instead lets the poller decide.
What: `notify` appends one JSON line (used by pm-notify.sh). `classify` is a pure rule
function returning (wake|hold, reason): kinds decision, blocker, head, done always wake;
`urgent` wakes any kind; an unknown or missing kind wakes; a head without a 7+ hex SHA
wakes as unknown; ack/progress/fyi are held, unless the text asks for a ruling
(`?` or approve|choose|which), which raises it to a wake. `read_new` reads by byte
offset and drops a repeat of (session, kind, pr, head, text) within 30 min.
Cursor commands (D39): `unread [--wake-only]`, `ack <id>... --outcome T`, `ack-through <id> --outcome T`,
`push-due [--mark]` (logic in pm_cursor.py; acks go to inbox/pm-acks.jsonl).
Test: scripts/test_pm_intake.py, scripts/test_pm_cursor.py
"""
import argparse
import hashlib
import json
import os
import re
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
WAKE_MSG_KINDS = {"decision", "blocker", "head", "done"}
HOLD_MSG_KINDS = {"ack", "progress", "fyi"}
ASKS = re.compile(r"\?|\b(approve|choose|which)\b", re.I)
SNAPSHOT = re.compile(r"\.trusty-mpm/sessions/\S*session-\d{8}-\d{6}\.md")
SHA = re.compile(r"^[0-9a-f]{7,40}$", re.I)
DEDUP_SECONDS = 30 * 60


def inbox():
    return os.environ.get("ARCHITECT_INBOX_DIR") or os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")


def messages_path():
    return os.path.join(inbox(), "pm-messages.jsonl")


def iso(ts=None):
    return datetime.fromtimestamp(time.time() if ts is None else ts, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def msg_id(line):
    return hashlib.sha1(line.encode()).hexdigest()[:12]


def classify(msg):
    """(wake|hold, reason) for one message dict. Pure."""
    kind = str(msg.get("kind") or "")
    if msg.get("urgent"):
        return "wake", f"urgent {kind or 'unknown'}"
    if kind == "head":
        if SHA.match(str(msg.get("head") or "")):
            return "wake", "head ready for clearance"
        return "wake", "head without a valid SHA (unknown)"
    if kind in WAKE_MSG_KINDS:
        return "wake", kind
    if kind in HOLD_MSG_KINDS:
        if ASKS.search(str(msg.get("text") or "")):
            return "wake", f"{kind} text asks for a ruling"
        if SNAPSHOT.search(str(msg.get("text") or "")):
            return "wake", "snapshot ready for refresh"
        return "hold", kind
    return "wake", f"unknown kind {kind!r}"


def read_new(state, path=None):
    """[(msg, line)] added since state['pm_offset'], repeats within 30 min marked dup.

    A first run reads from byte 0: the file is new, so nothing is backlog. A smaller
    file than the offset was rotated and is read from 0.
    """
    path = path or messages_path()
    size = os.path.getsize(path) if os.path.exists(path) else 0
    off = state.get("pm_offset", 0)
    if size < off:
        off = 0
    out = []
    if size > off:
        with open(path, "rb") as f:
            f.seek(off)
            chunk = f.read()
        end = chunk.rfind(b"\n") + 1  # leave a partial last line for the next cycle
        off += end
        seen = state.setdefault("pm_seen", {})
        now = time.time()
        for line in chunk[:end].decode("utf-8", "replace").splitlines():
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            if not isinstance(msg, dict):
                continue
            key = "|".join(str(msg.get(k) or "") for k in ("session", "kind", "pr", "head", "text"))
            if now - seen.get(key, 0) < DEDUP_SECONDS:
                msg["dup"] = True
            else:
                seen[key] = now
            out.append((msg, line))
        for k in [k for k, t in seen.items() if now - t >= DEDUP_SECONDS]:
            del seen[k]
    state["pm_offset"] = off
    return out


def step(state, alert_fn, now):
    """(wake alerts, triage records for every message). Reads the queue once."""
    alerts, recs = [], []
    for msg, line in read_new(state):
        decision, reason = classify(msg)
        if msg.get("dup"):
            decision, reason = "hold", "duplicate within 30 min"
        session = str(msg.get("session") or "")
        mid = msg_id(line)
        recs.append({"ts": iso(now), "id": mid, "kind": "pm_message", "session": session,
                     "decision": "wake" if decision == "wake" else "log",
                     "path": "rule:pm_intake", "reason": reason})
        if decision == "wake":
            bits = [str(msg.get("kind") or "?")]
            if msg.get("pr"):
                bits.append(f"PR#{msg['pr']}")
            if msg.get("head"):
                bits.append(str(msg["head"])[:10])
            alerts.append(alert_fn("pm_message", mid, f"{' '.join(bits)}: {msg.get('text') or ''}",
                                   session, "", now))
    return alerts, recs


def poll_step(state, alert_fn, now, triage_path, dry_run=False):
    """Wake alerts (kind pm_message) for the poller; held messages go to triage.jsonl as log.

    Why: fleet-poll.py takes one call here and no other change. Wake alerts are logged
    by the poller's own triage step (pm_message is in alert_triage.WAKE_KINDS).
    Test: test_poll_step_wakes_and_logs_held
    """
    alerts, recs = step(state, alert_fn, now)
    held = [r for r in recs if r["decision"] == "log"]
    if dry_run:
        for r in held:
            print(f"  pm-message held ({r['session']}): {r['reason']}")
    elif held:
        try:
            with open(triage_path, "a") as f:
                for r in held:
                    f.write(json.dumps(r, ensure_ascii=False) + "\n")
        except OSError as e:  # the offset already advanced: the wake alerts must still return
            print(f"pm_intake: triage write failed {type(e).__name__}: {e}", file=sys.stderr)
    return alerts


def notify(argv):
    ap = argparse.ArgumentParser(prog="pm-notify.sh", description="Queue a message for the Architect.")
    ap.add_argument("--kind", required=True, help="decision|blocker|head|done|ack|progress|fyi")
    ap.add_argument("--session", required=True, help="the sending PM's tmux session")
    ap.add_argument("--pr", type=int)
    ap.add_argument("--head", help="head SHA (7+ hex); required for kind head")
    ap.add_argument("--urgent", nargs="?", const="yes", help="wake whatever the kind (e.g. --urgent prod)")
    ap.add_argument("text", nargs="+")
    a = ap.parse_args(argv)
    msg = {"ts": iso(), "session": a.session, "kind": a.kind, "pr": a.pr, "head": a.head,
           "urgent": a.urgent, "text": " ".join(a.text)}
    line = (json.dumps({k: v for k, v in msg.items() if v is not None}, ensure_ascii=False) + "\n").encode()
    os.makedirs(inbox(), exist_ok=True)
    fd = os.open(messages_path(), os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, line)  # one O_APPEND write: concurrent PMs do not interleave
    finally:
        os.close(fd)
    print("queued")


def _ack_summary(ids):
    """One line: count, first and last id (never one line per id)."""
    return f"acked {len(ids)}" + (f" first={ids[0]} last={ids[-1]}" if ids else "")


def cursor_cli(cmd, argv):
    import pm_cursor
    ap = argparse.ArgumentParser(prog=f"pm_intake.py {cmd}")
    if cmd == "unread":
        ap.add_argument("--wake-only", action="store_true")
    elif cmd == "push-due":
        ap.add_argument("--mark", action="store_true")
    else:
        ap.add_argument("ids", nargs="+" if cmd == "ack" else 1)
        ap.add_argument("--outcome", required=True)
    a = ap.parse_args(argv)
    try:
        if cmd == "unread":
            for m in pm_cursor.unread(wake_only=a.wake_only):
                print(f"{pm_cursor._line(m)} [{m['cls']}]")
        elif cmd == "push-due":
            print("\n".join(pm_cursor.push_due(a.mark)))
        elif cmd == "ack":
            print(_ack_summary(pm_cursor.ack(a.ids, a.outcome)))
        else:
            print(_ack_summary(pm_cursor.ack_through(a.ids[0], a.outcome)))
    except ValueError as e:
        sys.exit(f"pm_intake: {e}")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "notify":
        notify(sys.argv[2:])
    elif len(sys.argv) > 1 and sys.argv[1] in ("unread", "ack", "ack-through", "push-due"):
        cursor_cli(sys.argv[1], sys.argv[2:])
    else:
        sys.exit("usage: pm_intake.py notify --kind K --session S [--pr N --head SHA --urgent] text"
                 " | unread [--wake-only] | ack <id>... --outcome T | ack-through <id> --outcome T | push-due [--mark]")

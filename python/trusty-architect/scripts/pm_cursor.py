#!/usr/bin/env python3
"""pm_cursor.py: unread/ack cursor over PM messages, the hook injection block, wake-failure alert, escalation.

Why: the poller's typed wake failed 383 of 383 cycles on 2026-10-20 and nothing surfaced it (the owner D39,
Model A: the trusty-events bus is the queue, a hook injects unread events, the ack is a cursor). The
bus is not on this host until the 1.9.0 install, so this reads inbox/pm-messages.jsonl behind a source
interface a later EventsBusSource (since_seq/since_id) can replace.
What: `JsonlSource.messages()` yields [(id, msg)] in order (id = pm_intake.msg_id of the line).
Acks append {ts, id, outcome} to inbox/pm-acks.jsonl; unread = every message not acked. `unread()`
lists them (wake_only keeps classify()=="wake"); `ack`/`ack_through` write acks. `inject_block()` is the
hook text: "PM MESSAGES UNREAD" plus one line per unacked wake message (cap 10 + "+N more"), a
"WAKE FAILING since <ts> (<n> attempts)" line while pm-wake-alert.json exists, and a PUSH DUE line.
It repeats on every call (no dedupe). MCP-channel messages (source "mcp" / session "mcp:*", from
architect-mcp.py) with no row in mcp-replies.jsonl are listed first under their own "MCP MESSAGES UNREAD"
header, tagged `[MCP]`, with the `architect-mcp.py reply` command; `ack` and `ack_through` refuse or skip
them until a reply row exists (`no_reply_needed=True` is the explicit escape). `record_wake_result` writes/clears the alert file for fleet-poll.
`escalate` keeps ONE dashboard summary action for wake messages unacked 30+ min (since the first run, under 24 h) and records "push due" at 60+
min in pm-escalation.json; it never sends a push (the Architect does, `pm_intake.py push-due --mark`).
Test: scripts/test_pm_cursor.py
"""
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import pm_intake  # noqa: E402

MAX_LINES = 10
TEXT_CHARS = 120
ESCALATE_SECS = 30 * 60
PUSH_SECS = 60 * 60
RETRY_SECS = 300


def _p(name):
    return os.path.join(pm_intake.inbox(), name)


def acks_path():
    return _p("pm-acks.jsonl")


def replies_path():
    return _p("mcp-replies.jsonl")


def wake_alert_path():
    return _p("pm-wake-alert.json")


def escalation_path():
    return _p("pm-escalation.json")


class JsonlSource:
    """Messages from pm-messages.jsonl. A bus source implements the same messages() call."""

    def __init__(self, path=None):
        self.path = path or pm_intake.messages_path()

    def messages(self):
        out, seen = [], set()
        try:
            with open(self.path, encoding="utf-8", errors="replace") as f:
                lines = f.read().splitlines()
        except OSError:
            return out
        for line in lines:
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            mid = pm_intake.msg_id(line)
            if isinstance(msg, dict) and mid not in seen:
                seen.add(mid)
                out.append((mid, msg))
        return out


def _load(path):
    try:
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def _save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f)
    os.replace(tmp, path)


def acked_ids(path=None):
    ids = set()
    try:
        with open(path or acks_path(), encoding="utf-8") as f:
            for line in f:
                try:
                    ids.add(json.loads(line)["id"])
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    return ids


def unread(source=None, wake_only=False, acks=None):
    """Unacked messages in order, each msg dict plus id, cls (wake|hold) and reason."""
    done = acked_ids(acks)
    out = []
    for mid, msg in (source or JsonlSource()).messages():
        if mid in done:
            continue
        cls, reason = pm_intake.classify(msg)
        if wake_only and cls != "wake":
            continue
        out.append(dict(msg, id=mid, cls=cls, reason=reason))
    return out


def _write_acks(ids, outcome, acks, now):
    path = acks or acks_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        for i in ids:
            f.write(json.dumps({"ts": pm_intake.iso(now), "id": i, "outcome": outcome}, ensure_ascii=False) + "\n")


def is_mcp(msg):
    """True for a message queued by architect-mcp.py (an outside agent waiting on mcp-replies.jsonl)."""
    return msg.get("source") == "mcp" or str(msg.get("session") or "").startswith("mcp:")


def replied_ids(path=None):
    ids = set()
    try:
        with open(path or replies_path(), encoding="utf-8", errors="replace") as f:
            for line in f:
                try:
                    ids.add(json.loads(line)["id"])
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    return ids


def needs_reply(msg, mid, replied):
    """An MCP-channel message with no persisted reply row: acking it would leave the sender 'pending'."""
    return is_mcp(msg) and mid not in replied


def reply_command(mid):
    return f'python3 scripts/architect-mcp.py reply {mid} "<text>"'


class ReplyRequired(ValueError):
    """ack refused: an MCP-channel message has no row in mcp-replies.jsonl."""


NO_REPLY_PREFIX = "no-reply-needed: "


def ack(ids, outcome, source=None, acks=None, now=None, no_reply_needed=False, replies=None):
    """Ack each id; ValueError for an id not in the source. Already-acked ids are skipped. Returns acked ids.

    An MCP-channel id with no row in mcp-replies.jsonl raises ReplyRequired (nothing is written) unless
    no_reply_needed=True, which acks it with the outcome prefixed "no-reply-needed: ".
    """
    msgs = dict((source or JsonlSource()).messages())
    bad = [i for i in ids if i not in msgs]
    if bad:
        raise ValueError("unknown message id: " + " ".join(bad))
    done = acked_ids(acks)
    todo = [i for i in dict.fromkeys(ids) if i not in done]
    replied = replied_ids(replies)
    stranded = [i for i in todo if needs_reply(msgs[i], i, replied)]
    if stranded and not no_reply_needed:
        raise ReplyRequired("MCP message has no reply; the sender would stay pending. Reply with: "
                            + "; ".join(reply_command(i) for i in stranded)
                            + " (it also acks), or ack with --no-reply-needed")
    plain = [i for i in todo if i not in stranded]
    _write_acks(plain, outcome, acks, now)
    _write_acks(stranded, NO_REPLY_PREFIX + outcome, acks, now)
    return todo


def ack_through(mid, outcome, source=None, acks=None, now=None, no_reply_needed=False, replies=None, skipped=None):
    """Ack every unacked message up to and including `mid` in source order.

    An unreplied MCP-channel message in the range is SKIPPED (left unread, id appended to `skipped`) so a
    sweep never strands an outside agent; no_reply_needed=True acks it with the "no-reply-needed: " prefix.
    """
    pairs = (source or JsonlSource()).messages()
    order = [m for m, _ in pairs]
    if mid not in order:
        raise ValueError(f"unknown message id: {mid}")
    kind = dict(pairs)
    done = acked_ids(acks)
    replied = replied_ids(replies)
    todo = [m for m in order[:order.index(mid) + 1] if m not in done]
    stranded = [m for m in todo if needs_reply(kind[m], m, replied)]
    if stranded and not no_reply_needed:
        if skipped is not None:
            skipped.extend(stranded)
        todo = [m for m in todo if m not in stranded]
        stranded = []
    _write_acks([m for m in todo if m not in stranded], outcome, acks, now)
    _write_acks(stranded, NO_REPLY_PREFIX + outcome, acks, now)
    return todo


def _line(m):
    bits = [m["id"], str(m.get("ts") or "?"), str(m.get("session") or "?"), str(m.get("kind") or "?")]
    if m.get("pr"):
        bits.append(f"PR#{m['pr']}")
    if m.get("head"):
        bits.append(str(m["head"])[:10])
    text = re.sub(r"\s+", " ", str(m.get("text") or "")).strip()[:TEXT_CHARS]
    return " ".join(bits) + " " + text


def record_wake_result(result, now=None, path=None):
    """fleet-poll calls this with wake()'s log phrase: 'failed...' counts, 'sent...' clears; skips change nothing."""
    path = path or wake_alert_path()
    now = time.time() if now is None else now
    if result.startswith("failed"):
        st = _load(path)
        st.setdefault("first_ts", pm_intake.iso(now))
        st["attempts"] = int(st.get("attempts", 0)) + 1
        st["last_ts"] = pm_intake.iso(now)
        _save(path, st)
    elif result.startswith("sent"):
        try:
            os.remove(path)
        except OSError:
            pass


def _age(m, now):
    try:
        return now - datetime.strptime(m["ts"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()
    except (KeyError, ValueError, TypeError):
        return 0.0


MIN_FREE_IDS = 16
MAX_AGE_SECS = 24 * 3600


def _run_db(*args):
    return subprocess.run([sys.executable, os.path.join(HERE, "dashboard-db.py"), *args],
                          capture_output=True, text=True, timeout=20)


def _dashboard_sync(item_id, summary):
    """Create (item_id None) or update the one summary action; count 0 closes it. Returns the id or None.

    Refuses to create when fewer than MIN_FREE_IDS A ids are free, so it can never use up the prefix.
    """
    now = pm_intake.iso()
    if summary["count"]:
        o = summary["oldest"]
        data = {"kind": "action", "status": "open", "rank": 1, "urgent": True, "goal": None,
                "session": str(o.get("session") or os.environ.get("ARCHITECT_SESSION") or "tm-architect"), "updated": now,
                "title": f"{summary['count']} PM messages unacked 30+ min: oldest {o['id']} {o.get('session')}",
                "detail": _line(o) + "\nList: python3 scripts/pm_intake.py unread --wake-only\n"
                          "Ack: python3 scripts/pm_intake.py ack <id> --outcome '<text>'"}
    else:
        data = {"status": "done", "done_at": now, "updated": now, "outcome": "all escalated PM messages acked"}
    if item_id:
        return item_id if _run_db("update", "items", item_id, "--json", json.dumps(data)).returncode == 0 else None
    if not summary["count"]:
        return None
    import dashboard_ids
    if dashboard_ids.free_count("A") < MIN_FREE_IDS:
        print(f"pm_cursor: fewer than {MIN_FREE_IDS} free A ids; no escalation item created", file=sys.stderr)
        return None
    r = _run_db("create", "items", "A", "--json", json.dumps(dict(data, created=now)))
    try:
        return json.loads(r.stdout)["id"] if r.returncode == 0 else None
    except (ValueError, KeyError):
        return None


def _eligible(m, since, now):
    """A message after the first escalation run and under 24 h old."""
    age = _age(m, now)
    try:
        ts = datetime.strptime(m["ts"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()
    except (KeyError, ValueError, TypeError):
        return False
    return ts >= since and age <= MAX_AGE_SECS


def escalate(wake_unread, now=None, sync_fn=None, path=None):
    """30+ min unacked: ONE dashboard summary action, updated in place (never one item per message).
    60+ min: record push due. Never pushes.

    Only messages timestamped at or after the first run (`since`) and under 24 h old count, so a backlog
    never escalates. State {since, summary_id, summary_key, push_due: {id: {at, pushed}}, retry_at};
    keys from the earlier per-message format are dropped on the next save. A failed sync retries after 5 min.
    """
    now = time.time() if now is None else now
    path = path or escalation_path()
    st = _load(path)
    since = float(st.get("since") or now)
    elig = [m for m in wake_unread if _eligible(m, since, now)]
    late = [m for m in elig if _age(m, now) >= ESCALATE_SECS]
    due = {k: v for k, v in st.get("push_due", {}).items() if k in {m["id"] for m in elig}}
    for m in elig:
        if _age(m, now) >= PUSH_SECS and m["id"] not in due:
            due[m["id"]] = {"at": pm_intake.iso(now), "pushed": None}
    sid, key, retry_at = st.get("summary_id"), st.get("summary_key"), float(st.get("retry_at", 0))
    want = [m["id"] for m in late]
    if (want or sid) and f"{len(want)}:{want[:1]}" != key and now >= retry_at:
        sync_fn = sync_fn or _dashboard_sync
        try:
            res = sync_fn(sid, {"count": len(late), "oldest": late[0] if late else None})
        except (OSError, subprocess.SubprocessError):
            res = None
        if res:
            sid, key = (res if want else None), (f"{len(want)}:{want[:1]}" if want else None)
        else:
            retry_at = now + RETRY_SECS
    new = {"since": since, "summary_id": sid, "summary_key": key, "push_due": due, "retry_at": retry_at}
    if new != {k: st.get(k) for k in new}:
        _save(path, new)
    return new


def push_due(mark=False, path=None, now=None):
    """Ids with push due and not yet pushed; mark=True records them as pushed."""
    path = path or escalation_path()
    st = _load(path)
    ids = [k for k, v in st.get("push_due", {}).items() if not v.get("pushed")]
    if mark and ids:
        for k in ids:
            st["push_due"][k]["pushed"] = pm_intake.iso(now)
        _save(path, st)
    return ids


def inject_block(source=None, acks=None, now=None, alert_path=None, esc_path=None, sync_fn=None):
    """Hook text for the unread wake messages, or None when there are none. Repeats every call."""
    now = time.time() if now is None else now
    wake = unread(source, wake_only=True, acks=acks)
    esc = escalate(wake, now, sync_fn, esc_path)
    if not wake:
        return None
    lines = []
    alert = _load(alert_path or wake_alert_path())
    if alert:
        lines.append(f"WAKE FAILING since {alert.get('first_ts')} ({alert.get('attempts', 0)} attempts)")
    replied = replied_ids()
    mcp = [m for m in wake if needs_reply(m, m["id"], replied)]  # first: an outside agent is waiting
    shown = (mcp + [m for m in wake if m not in mcp])[:MAX_LINES]
    shown_mcp = [m for m in shown if m in mcp]
    shown_pm = [m for m in shown if m not in mcp]
    if shown_mcp:
        lines.append("MCP MESSAGES UNREAD (outside agent waits on a reply; answer with: python3 "
                     'scripts/architect-mcp.py reply <id> "<text>", which also acks; pm_intake.py ack is refused)')
        lines += ["[MCP] " + _line(m) for m in shown_mcp]
    if shown_pm:
        lines.append("PM MESSAGES UNREAD (ack with pm_intake.py ack <id> --outcome ...)")
        lines += [_line(m) for m in shown_pm]
    if len(wake) > MAX_LINES:
        lines.append(f"+{len(wake) - MAX_LINES} more")
    ids = [k for k, v in esc["push_due"].items() if not v.get("pushed")]
    if ids:
        lines.append("PUSH DUE (60+ min unacked; PushNotification unless user state Sleeping, then "
                     "`pm_intake.py push-due --mark`): " + " ".join(ids))
    return "\n".join(lines)

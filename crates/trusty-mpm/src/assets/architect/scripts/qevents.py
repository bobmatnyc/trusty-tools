"""qevents.py — the owner-question event log and its folded view.

Why: open questions must survive supervisor restarts and account switches, and
the design must carry over to pub/sub on the trusty event bus (ADR-0005) and to
more answer channels (Slack, Telegram). The bus is broadcast-only with no
persistence, so the durable record is an append-only event log and the open
queue is a view folded from it. The Rust rebuild swaps this file for a bus
publish plus a store; the event names and fields stay the same.
What: `append(event, **fields)` writes one event line under an exclusive lock;
`fold()` replays the log into {id: question-state}; `open_questions()` returns
the ones not yet retired. Events:
  question.opened   id, session, question, options, recommendation, urgent,
                    dialog, reason
  question.answered id, channel (chat|pane|slack|telegram|...), answer, by
  question.relayed  id, target (tmux session), text
  question.retired  id, disposition (asked-user | answered-direct |
                    already-answered | resolved-in-pane | not-a-question),
                    reason
Test: `python3 scripts/questions.py list` after a collector sweep; folding is
deterministic, so replaying the same log always gives the same queue.
"""
import fcntl
import json
import os
from datetime import datetime, timezone

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOG = os.path.join(ROOT, "local", "question-events.jsonl")
LOCK = LOG + ".lock"
EVENTS = ("question.opened", "question.answered", "question.relayed", "question.retired")


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


class _Lock:
    def __enter__(self):
        self.f = open(LOCK, "w")
        fcntl.flock(self.f, fcntl.LOCK_EX)
        return self

    def __exit__(self, *a):
        fcntl.flock(self.f, fcntl.LOCK_UN)
        self.f.close()


def lock():
    """Exclusive lock for a read-decide-append sequence (e.g. dedupe then open)."""
    return _Lock()


def append(event, _locked=False, **fields):
    if event not in EVENTS:
        raise ValueError(f"unknown event {event}")
    line = json.dumps({"event": event, "ts": fields.pop("ts", None) or now(), **fields})
    if _locked:
        with open(LOG, "a") as f:
            f.write(line + "\n")
        return
    with _Lock():
        with open(LOG, "a") as f:
            f.write(line + "\n")


def read():
    try:
        with open(LOG) as f:
            return [json.loads(l) for l in f if l.strip()]
    except OSError:
        return []


def fold(events=None):
    """Replay events into {id: state}. State keeps the opened fields plus
    answers, relays, and the retirement (None while open)."""
    qs = {}
    for e in events if events is not None else read():
        i = e.get("id")
        if e["event"] == "question.opened":
            qs[i] = {k: v for k, v in e.items() if k != "event"} | {
                "found": e["ts"], "answers": [], "relays": [], "retired": None}
        elif i in qs:
            if e["event"] == "question.answered":
                qs[i]["answers"].append(e)
            elif e["event"] == "question.relayed":
                qs[i]["relays"].append(e)
            elif e["event"] == "question.retired":
                qs[i]["retired"] = e
    return qs


def open_questions():
    return [q for q in fold().values() if q["retired"] is None]

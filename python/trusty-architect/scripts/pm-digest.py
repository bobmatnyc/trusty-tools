#!/usr/bin/env python3
"""pm-digest.py - what the PMs sent since the last wake, in at most 20 lines (D10).

Why: held messages (ack, progress, fyi) never wake the Architect; it reads them here.
What: groups inbox/pm-messages.jsonl lines newer than inbox/digest-state.json `last_wake`
(or --since ISO) by session; WAKE lines first, then one line per (session, kind) with a
count. Default advances last_wake to now; --no-advance only reads. Lines are 110 chars
max; overflow collapses to `+N more, run with --all`.
`wake_digest` builds the one-line digest the poller appends to its wake line (Db8
2026-10-14): one `<session> <kind>: <text>` item per woken alert and per held PM message,
capped, joined with ` | `. Pure, no model call.
Test: scripts/test_pm_digest.py
"""
import argparse
import calendar
import importlib.util
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("pm_intake", os.path.join(HERE, "pm_intake.py"))
pm_intake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pm_intake)
MAX_LINES, MAX_WIDTH = 20, 110


def parse_ts(text):
    return calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ"))


def build(msgs, since_label, show_all=False):
    """Digest lines for `msgs` (dicts, oldest first)."""
    wakes, holds, held_n = [], {}, 0
    for m in msgs:
        decision, reason = pm_intake.classify(m)
        s = str(m.get("session") or "?")
        if decision == "wake":
            bits = [str(m.get("kind") or "?")]
            if m.get("pr"):
                bits.append(f"PR#{m['pr']}")
            if m.get("head"):
                bits.append(str(m["head"])[:10])
            wakes.append(f"{s:<13} WAKE {' '.join(bits)}; {m.get('text') or ''}")
        else:
            held_n += 1
            h = holds.setdefault((s, str(m.get("kind"))), [0, ""])
            h[0] += 1
            h[1] = str(m.get("text") or "")
    held = [f"{s:<13} held {n} {k} (last: \"{t}\")" for (s, k), (n, t) in holds.items()]
    head = f"DIGEST since {since_label} ({held_n} held, {len(wakes)} woke)"
    body = [ln[:MAX_WIDTH] for ln in wakes + held]
    room = MAX_LINES - 1
    if not show_all and len(body) > room:
        extra = len(body) - (room - 1)
        body = body[:room - 1] + [f"+{extra} more, run with --all"]
    return [head] + body


def _flat(text, width):
    """One line, control characters and runs of whitespace collapsed, cut to `width`."""
    return " ".join(str(text or "").split())[:width]


def wake_digest(alerts, msgs, path, cap=8, width=140):
    """Single-line digest ('' when nothing): `<session> <kind>: <text>` per alert dict
    (session/kind/detail) then per held PM message (session/kind/text), at most `cap`
    items, then `+N more -> path`. Single line because multi-line send-keys submits early."""
    items = []
    for a in alerts:
        items.append((str(a.get("session") or "owner"), a.get("kind"), a.get("detail")))
    for m in msgs:
        items.append((str(m.get("session") or "?"), m.get("kind"), m.get("text")))
    out = [f"{s} {k or '?'}: {_flat(t, width)}" for s, k, t in items[:cap]]
    if len(items) > cap:
        out.append(f"+{len(items) - cap} more -> {path}")
    return " | ".join(out)


def held_since(cutoff, path=None):
    """Held (non-wake) PM messages with ts > `cutoff` (epoch seconds), oldest first."""
    out = []
    try:
        with open(path or pm_intake.messages_path()) as f:
            for line in f:
                try:
                    m = json.loads(line)
                    if parse_ts(m["ts"]) > cutoff and pm_intake.classify(m)[0] == "hold":
                        out.append(m)
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description="Digest of PM messages since the last wake.")
    ap.add_argument("--since", default="last", help="last (default) or an ISO UTC time")
    ap.add_argument("--no-advance", action="store_true", help="read only; keep last_wake")
    ap.add_argument("--all", action="store_true", help="no 20-line cap")
    a = ap.parse_args(argv)
    state_path = os.path.join(pm_intake.inbox(), "digest-state.json")
    try:
        with open(state_path) as f:
            last = json.load(f).get("last_wake")
    except (OSError, ValueError):
        last = None
    since = last if a.since == "last" else a.since
    cutoff = parse_ts(since) if since else 0
    msgs = []
    try:
        with open(pm_intake.messages_path()) as f:
            for line in f:
                try:
                    m = json.loads(line)
                    if parse_ts(m["ts"]) > cutoff:
                        msgs.append(m)
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    print("\n".join(build(msgs, (since or "start")[11:16] + "Z" if since else "start", a.all)))
    if not a.no_advance:
        os.makedirs(pm_intake.inbox(), exist_ok=True)
        with open(state_path, "w") as f:
            json.dump({"last_wake": pm_intake.iso()}, f)


if __name__ == "__main__":
    sys.exit(main())

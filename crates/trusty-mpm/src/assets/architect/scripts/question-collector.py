#!/usr/bin/env python3
"""RETIRED 2026-10-04 (f4): running this script exits 2; see RETIRED_MSG.

question-collector.py — collect owner questions from PM panes into a queue.

Why: the supervisor session cannot watch the fleet while one of its own
questions to the user is open, and questions posted as chat text scroll away.
This collector runs in its own tmux session, independent of the supervisor,
and keeps a durable queue in files so it also survives account switches.
What: follows inbox/events.jsonl; on each PM event (and on a periodic sweep of
every tm-* session) it captures the pane, asks collector-agent (.claude/agents/
collector-agent.md, via `claude -p --agent`) whether the latest PM turn holds
a question for the owner, and appends each
new question as a `question.opened` event on local/question-events.jsonl
(scripts/qevents.py). It never sends keys to any pane.
The supervisor folds the log into the open queue (scripts/questions.py),
adds context, answers or asks the user, and records answered/relayed/retired
events on the same log.
Usage: scripts/question-collector.py [--once]   (normally run via
       scripts/start-question-collector.sh)
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qevents as qe  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EVENTS = os.path.join(ROOT, "inbox", "events.jsonl")
STATE = os.path.join(ROOT, "local", "collector-state.json")
LOG = os.path.join(ROOT, "inbox", "collector.log")
SWEEP_SECONDS = 600      # full sweep of all tm-* panes, catches missed events
DEBOUNCE_SECONDS = 20    # coalesce bursts of events from one pane
# Supervisors, not PMs: tm-employer-cto only observes and summarizes PMs' questions.
SKIP_SESSIONS = {"tm-supervisor", "tm-supervisor-collector", "tm-employer-cto"}

PROMPT = """SESSION: {session}

KNOWN (this session's ledger; do not re-report these):
{known}

PANE:
{pane}
"""


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def log(msg):
    with open(LOG, "a") as f:
        f.write(f"{now()} {msg}\n")


def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def save_state(state):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


def sessions():
    out = subprocess.run(["tmux", "ls", "-F", "#{session_name}"],
                         capture_output=True, text=True).stdout.split()
    return [s for s in out if s.startswith("tm-") and s not in SKIP_SESSIONS]


def capture(session):
    r = subprocess.run(["tmux", "capture-pane", "-t", f"={session}:", "-p", "-S", "-120"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None
    # rstrip: wide panes pad lines with spaces, which only inflate the prompt.
    # Test: `test_capture_strips_trailing_whitespace`.
    lines = [l.rstrip() for l in r.stdout.splitlines() if l.strip()]
    # Drop the status footer and borders; they change every second.
    lines = [l for l in lines if not re.match(r"^[─━]+$", l)
             and "TM 1." not in l and "bypass permissions" not in l
             and not l.strip().endswith("/rc") and "/effort" not in l]
    # The input box sits after the last "❯" line: a dim next-prompt suggestion
    # there reads like a user reply, and the agent once retired a question
    # from it (2026-09-24, tm-tga-01 "the owner picks A"). Cut it in code, unless a
    # selection dialog is open (its cursor also starts with "❯").
    if not any("Enter to select" in l for l in lines[-15:]):
        for k in range(len(lines) - 1, -1, -1):
            if lines[k].lstrip().startswith("❯"):
                lines = lines[:k]
                break
    return "\n".join(lines[-90:])


def turn_hash(text):
    # Ignore spinners/timers so an unchanged turn is not re-read.
    stable = re.sub(r"\d+m \d+s|\d+s\b|↓ [\d.]+k tokens|[✻✽✢✳✶·*] \S+…", "", text)
    return hashlib.sha1(stable.encode()).hexdigest()


def ledger(session):
    """This session's questions so far from the event log: open ones plus the
    last 15 retired, so collector-agent reports only new ones."""
    rows = [q for q in qe.fold().values() if q["session"] == session]
    open_ = [q for q in rows if q["retired"] is None]
    done = [q for q in rows if q["retired"] is not None][-15:]
    if not open_ and not done:
        return "(none)"
    out = [f'- id={q["id"]} [open] {q["question"]}' for q in open_]
    for q in done:
        ans = q["answers"][-1]["answer"] if q["answers"] else q["retired"].get("reason", "")
        out.append(f'- id={q["id"]} [answered: {ans[:120]}] {q["question"]}')
    return "\n".join(out)


def pane_size(pane):
    """`bytes=B lines=L` of a capture, for the extract log lines."""
    return f"bytes={len(pane.encode())} lines={len(pane.splitlines())}"


# (pane lines kept, timeout s) per attempt; None keeps the whole capture.
# 2026-09-25: tm-webapp-companion hit the 120 s timeout three times in ten
# minutes while every other pane extracted fine, so one retry runs on a
# shorter capture with a longer budget.
ATTEMPTS = ((None, 120), (60, 180))


def extract(session, pane):
    """Why: a pane whose capture makes collector-agent slow must not lose its
    questions until the next idle event.
    What: runs collector-agent on the capture; on TimeoutExpired retries once
    on the last 60 lines with a 180 s timeout. Every log line carries the
    size of the capture that attempt used. Returns (questions, resolved,
    size) where size is the pane_size() string of the capture that answered,
    with ` attempt=2` appended after a retry; None on failure.
    Test: `test_retry_once_on_timeout`, `test_no_retry_on_success`,
    `test_log_lines_carry_size`.
    """
    known = ledger(session)
    for n, (keep, timeout) in enumerate(ATTEMPTS, 1):
        text = pane if keep is None else "\n".join(pane.splitlines()[-keep:])
        size = pane_size(text)
        prompt = (PROMPT.replace("{session}", session).replace("{known}", known)
                  .replace("{pane}", text))
        try:
            # The rules live in .claude/agents/collector-agent.md (versioned here).
            r = subprocess.run(["claude", "-p", "--agent", "collector-agent", "--tools", "",
                                "--disable-slash-commands", "--strict-mcp-config"],
                               input=prompt, capture_output=True, text=True,
                               timeout=timeout, cwd=ROOT)
        except subprocess.TimeoutExpired:
            log(f"{session} extract TIMEOUT attempt={n} {size}")
            continue
        if n > 1:
            size += f" attempt={n}"
        m = re.search(r"\{.*\}", r.stdout, re.S)
        if not m:
            log(f"{session} extract NOJSON rc={r.returncode} {size} "
                f"{r.stdout[:200]!r} {r.stderr[:200]!r}")
            return None
        try:
            d = json.loads(m.group(0))
            return d.get("questions", []), d.get("resolved", []), size
        except ValueError:
            log(f"{session} extract BADJSON {size} {m.group(0)[:200]!r}")
            return None
    return None


def qid(session, question):
    norm = re.sub(r"[^a-z0-9#]+", " ", question.lower()).strip()
    return hashlib.sha1(f"{session}|{norm}".encode()).hexdigest()[:12]


def process(session, state, reason):
    pane = capture(session)
    if pane is None:
        return
    h = turn_hash(pane)
    if state["hashes"].get(session) == h:
        return
    res = extract(session, pane)
    if res is None:
        return  # retry on the next event or sweep
    qs, resolved, size = res
    state["hashes"][session] = h
    added = retired = 0
    with qe.lock():
        state_q = qe.fold()
        for i in resolved:
            q = state_q.get(i)
            if q and q["session"] == session and q["retired"] is None:
                qe.append("question.retired", _locked=True, id=i,
                          disposition="resolved-in-pane",
                          reason="PM pane reports it answered or done")
                retired += 1
        for q in qs:
            text = (q.get("question") or "").strip()
            if not text:
                continue
            i = qid(session, text)
            if i in state_q:
                continue
            state_q[i] = True
            qe.append("question.opened", _locked=True, id=i, session=session,
                      question=text, options=q.get("options", []),
                      recommendation=q.get("recommendation", ""),
                      urgent=bool(q.get("urgent")), dialog=bool(q.get("dialog")),
                      reason=reason)
            added += 1
            # One log line per new question: the supervisor's wake signal.
            log(f"NEW {i} [{session}]{' URGENT' if q.get('urgent') else ''} {text}")
    log(f"{session} {reason} questions={len(qs)} new={added} retired={retired} {size}")


RETIRED_MSG = ("question-collector.py: retired 2026-10-04 (f4); history in "
               "local/question-events.jsonl; scripts/questions.py list still reads it")


def main():
    # f4: PMs SendMessage the Architect and fleet-poll.py catches the rest; the
    # collector-agent it needs no longer exists, so refuse instead of failing obscurely.
    print(RETIRED_MSG, file=sys.stderr)
    sys.exit(2)
    once = "--once" in sys.argv
    state = load_json(STATE, {})
    state.setdefault("hashes", {})
    if "offset" not in state or not os.path.exists(EVENTS):
        state["offset"] = os.path.getsize(EVENTS) if os.path.exists(EVENTS) else 0
    pending = {}
    last_sweep = 0.0
    log("collector start")
    while True:
        try:
            size = os.path.getsize(EVENTS) if os.path.exists(EVENTS) else 0
            if size < state["offset"]:
                state["offset"] = 0  # file rotated
            if size > state["offset"]:
                with open(EVENTS) as f:
                    f.seek(state["offset"])
                    chunk = f.read()
                    state["offset"] = f.tell()
                for line in chunk.splitlines():
                    try:
                        ev = json.loads(line)
                    except ValueError:
                        continue
                    s = ev.get("tmux_session")
                    if s and s not in SKIP_SESSIONS:
                        pending[s] = (time.time(), ev.get("type", "event"))
            t = time.time()
            for s, (seen, typ) in list(pending.items()):
                if t - seen >= DEBOUNCE_SECONDS:
                    del pending[s]
                    process(s, state, typ)
            if t - last_sweep >= SWEEP_SECONDS:
                last_sweep = t
                for s in sessions():
                    if s not in pending:
                        process(s, state, "sweep")
            save_state(state)
            if once:
                return
        except Exception as e:  # keep collecting; one bad pane must not stop it
            log(f"ERROR {type(e).__name__}: {e}")
        time.sleep(3)


if __name__ == "__main__":
    main()

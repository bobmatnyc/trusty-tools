#!/usr/bin/env python3
"""pending_hook_dedupe.py: decide whether the PostToolUse pending-answers hook speaks.

Why: the hook injected the same "OWNER ANSWERS PENDING" line (~100 tokens) after every tool call,
about 130 calls per session (T09 round 1, change 1).
What: decide(items, state_path, session, now, user_state) returns the context text (first line "USER STATE: <state>"
when user_state is given, then the pending line), or None when the pending set and state
(id, kind, at) equal the last emission for this session and under 15 minutes passed. owner_unpark_due
items are open asks, not answers to relay, and are dropped. main() reads the hook JSON on stdin and
prints the hookSpecificOutput JSON when decide() returns a line. Never raises; a state-file error
means "emit".
Test: scripts/test_pending_answers_hook.py, scripts/test_user_state.py
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

REMIND_SECS = 900
MAX_SESSIONS = 20
ROOT = os.path.dirname(HERE)
STATE = os.path.join(ROOT, "inbox", "pending-hook-state.json")


def _load(path):
    try:
        with open(path, encoding="utf-8") as f:
            d = json.load(f)
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def _save(path, data):
    try:
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(data, f)
        os.replace(tmp, path)
    except OSError:
        pass


def decide(items, state_path, session, now=None, user_state=None):
    now = time.time() if now is None else now
    items = [i for i in items if i.get("kind") != "owner_unpark_due"]
    state = _load(state_path)
    prev = state.get(session) or {}
    ifp = json.dumps(sorted((i["id"], i["kind"], i["at"]) for i in items)) if items else ""
    say_items = bool(ifp) and not (ifp == prev.get("fp") and now - float(prev.get("last", 0)) < REMIND_SECS)
    say_state = bool(user_state) and not (user_state == prev.get("us") and now - float(prev.get("us_last", 0)) < REMIND_SECS)
    new = dict(prev)
    if not ifp:
        new["fp"] = ""
    if say_items:
        new["fp"], new["last"] = ifp, now
    if say_state:
        new["us"], new["us_last"] = user_state, now
    if new != prev:
        state[session] = new
        if len(state) > MAX_SESSIONS:
            keep = sorted(state, key=lambda k: max(state[k].get("last", 0), state[k].get("us_last", 0)))[-MAX_SESSIONS:]
            state = {k: state[k] for k in keep}
        _save(state_path, state)
    lines = [f"USER STATE: {user_state}"] if say_state else []
    if say_items:
        lines.append("OWNER ANSWERS PENDING (relay now, r-183): " + "; ".join(f"{i['id']} ({i['kind']}): {i['text'][:60]}" for i in items))
    return "\n".join(lines) or None


def main():
    try:
        d = json.load(sys.stdin)
    except ValueError:
        d = {}
    if not isinstance(d, dict) or d.get("agent_id"):
        return 0
    import owner_answers
    try:
        items = owner_answers.pending(os.path.join(ROOT, "local", "owner-queue"))
    except OSError:
        return 0
    import user_state
    line = decide(items, STATE, str(d.get("session_id", "")),
                  user_state=user_state.read(os.path.join(ROOT, "local", "owner-queue"))["state"])
    if line:
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": line}}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

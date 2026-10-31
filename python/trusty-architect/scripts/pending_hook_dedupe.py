#!/usr/bin/env python3
"""pending_hook_dedupe.py: decide whether the PostToolUse pending-answers hook speaks.

Why: the hook injected the same "OWNER ANSWERS PENDING" line (~100 tokens) after every tool call,
about 130 calls per session (T09 round 1, change 1).
What: the user state printed is the effective one (user_state.effective: active for 15 min after any owner signal, else the
manual setting) with its reason; an owner prompt (owner_prompt) records a signal first. decide(items, state_path, session, now, user_state, state_reason) returns the context text (first line "USER STATE: <state>"
when user_state is given, then the pending line), or None when the pending set and state
(id, kind, at) equal the last emission for this session and under 15 minutes passed. owner_unpark_due
items are open asks, not answers to relay, and are dropped. main() reads the hook JSON on stdin and
prints the hookSpecificOutput JSON when decide() returns a line. Never raises; a state-file error
means "emit".
main() also appends pm_cursor.inject_block() (unread PM wake messages, repeated every call, D39) to the same
additionalContext and echoes the hook's own hook_event_name (PostToolUse or UserPromptSubmit).
Test: scripts/test_pending_answers_hook.py, scripts/test_user_state.py, scripts/test_pm_cursor.py
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


def decide(items, state_path, session, now=None, user_state=None, state_reason=None):
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
    lines = [f"USER STATE: {user_state}" + (f" ({state_reason})" if state_reason else "")] if say_state else []
    if say_items:
        lines.append("OWNER ANSWERS PENDING (relay now, r-183): " + "; ".join(
            f"INTERACTIVE: {i['id']} {i['text'][:60]} (ask the owner in chat, multiple choice)" if i["kind"] == "interactive"
            else f"{i['id']} ({i['kind']}): {i['text'][:60]}" for i in items))
    return "\n".join(lines) or None


def owner_prompt(d):
    """True for a UserPromptSubmit turn the owner typed: not a poller wake ("[poll "), a task notification ("<") or a
    slash command (self-clear, resume). The voice relay ("[the owner via Codex") counts as the owner."""
    if d.get("hook_event_name") != "UserPromptSubmit":
        return False
    p = str(d.get("prompt") or "").lstrip()
    return bool(p) and not p.startswith(("[poll ", "<", "/"))


def build_output(d, now=None):
    """The hook's JSON-able output for hook input `d`, or None. Subagent calls (agent_id) are skipped."""
    if not isinstance(d, dict) or d.get("agent_id"):
        return None
    parts = []
    import owner_answers
    try:
        import user_state
    except ImportError:  # #10571: the owner-state module ships with the poller set; without it the state is unknown
        user_state = None
    try:
        qroot = os.path.join(ROOT, "local", "owner-queue")
        try:  # a signal write must never block a tool call or hide the state line
            if user_state is None:
                raise LookupError("user_state not installed")
            if owner_prompt(d):
                user_state.record_signal(qroot, f"turn in {os.environ.get('ARCHITECT_SESSION') or 'tm-architect'}", now)
            user_state.sync_signal(qroot, now)
        except LookupError:
            pass
        except Exception as e:  # noqa: BLE001 - StoreError (also the live-store refusal under pytest), lock or schema errors
            sys.stderr.write(f"owner signal not recorded: {e!r}\n")
        items = owner_answers.pending(qroot)
        eff = user_state.effective(qroot, now) if user_state else {"state": None, "reason": ""}
        line = decide(items, STATE, str(d.get("session_id", "")), now,
                      user_state=eff["state"], state_reason=eff["reason"])
        if line:
            parts.append(line)
    except OSError:
        pass
    try:
        import pm_cursor
        block = pm_cursor.inject_block(now=now)
        if block:
            parts.append(block)
    except Exception as e:  # never block a tool call; say so in the context instead of failing silently
        parts.append(f"PM MESSAGES: unread check failed ({type(e).__name__}: {e})")
    if not parts:
        return None
    return {"hookSpecificOutput": {"hookEventName": str(d.get("hook_event_name") or "PostToolUse"),
                                   "additionalContext": "\n".join(parts)}}


def main():
    try:
        d = json.load(sys.stdin)
    except ValueError:
        d = {}
    out = build_output(d)
    if out:
        print(json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())

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
#10780: an owner prompt (owner_prompt, minus the machine-sent NOT_OWNER_TURN shapes) is also appended, verbatim, to the owner-turn record ({at, session, text} per line in
inbox/owner-turns.jsonl, or $ARCHITECT_OWNER_TURNS), which relay_provenance.py checks a `User ruling` quote
against; the record keeps its newest half once it passes OWNER_TURNS_MAX bytes.
main() also appends pm_cursor.inject_block() (unread PM wake messages, repeated every call, D39) to the same
additionalContext and echoes the hook's own hook_event_name (PostToolUse or UserPromptSubmit).
#11064: build_output also appends level_line() ("TECH LEVEL: <level> (<rule>)", technical_level.line) after the user
state, with the same timing: on the first call after the level changes, then once per REMIND_SECS. The level is the
owner's setting, or a secondary user's own (turn_speaker: the `[SECONDARY USER <name> via` prompt marker, kept for
that turn's tool calls); such a prompt is not an owner turn.
Test: tests/test_pending_answers_hook.py, tests/test_technical_level.py, tests/test_pm_cursor.py
"""
import json
import os
import re
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


def level_line(state_path, session, level, now=None, who=None):
    """#11064: the technical-level line (technical_level.line), throttled like the user state: the same level for the
    same reader in the same session, under REMIND_SECS since the last emission -> None. A change of level or of reader
    (owner or secondary user `who`) speaks at once. Shares the session's state entry (keys tl, tl_last)."""
    import technical_level
    now = time.time() if now is None else now
    level = technical_level.normalize(level)
    key = f"{who}:{level}" if who else level
    state = _load(state_path)
    prev = state.get(session) or {}
    if key == prev.get("tl") and now - float(prev.get("tl_last", 0)) < REMIND_SECS:
        return None
    state[session] = dict(prev, tl=key, tl_last=now)
    _save(state_path, state)
    return technical_level.line(level, who)


def turn_speaker(state_path, session, d):
    """#11064 (TL-8): the secondary user the current turn answers, or None for the owner. Each UserPromptSubmit sets it
    from its `[SECONDARY USER <name> via` marker; the turn's later tool calls reuse it (session key tl_who)."""
    import technical_level
    state = _load(state_path)
    prev = state.get(session) or {}
    if d.get("hook_event_name") != "UserPromptSubmit":
        return prev.get("tl_who") or None
    who = technical_level.speaker(d.get("prompt")) or ""
    if who != prev.get("tl_who", ""):
        state[session] = dict(prev, tl_who=who)
        _save(state_path, state)
    return who or None


def owner_prompt(d):
    """True for a UserPromptSubmit turn the owner typed: not a poller wake ("[poll "), a task notification ("<"), a
    slash command (self-clear, resume) or a secondary user's message ("[SECONDARY USER ", #11064). The voice relay
    ("[the owner via Codex") counts as the owner."""
    if d.get("hook_event_name") != "UserPromptSubmit":
        return False
    p = str(d.get("prompt") or "").lstrip()
    return bool(p) and not p.startswith(("[poll ", "<", "/", "[SECONDARY USER "))


OWNER_TURNS_MAX = 1024 * 1024
OWNER_TURN_TEXT_MAX = 8000


# #10780 critic: prompts that reach UserPromptSubmit but are not the owner typing. A PM's SendMessage
# ("Another Claude session sent a message: <cross-session-message ...>"), the self-clear resume prompt,
# hook feedback, a skill body, an interrupt or image note, a spawned session's brief, and a scheduled
# heartbeat ("<id> heartbeat: ..."). Recording any of them would make another party's words citable as
# the owner's. The architect MCP's architect_send never arrives as a prompt: it queues to
# inbox/pm-messages.jsonl and reaches the Architect through a "[poll ...]" wake and pm_cursor context.
NOT_OWNER_TURN = re.compile(
    r"^(?:Another Claude session sent a message|<cross-session-message|Resume from the Architect IL|"
    r"Stop hook feedback:|Base directory for this skill:|\[Request interrupted|\[Image:|You are tm-|"
    r"\S{1,12} heartbeat:)")


def owner_turn_text(d):
    """The prompt text when `d` is a turn the owner typed and none of NOT_OWNER_TURN, else None."""
    if not owner_prompt(d):
        return None
    p = str(d.get("prompt") or "").lstrip()
    return None if NOT_OWNER_TURN.match(p) else p


def record_owner_turn(d, now=None):
    """#10780: append the owner's prompt to the owner-turn record; never raises (a write error goes to stderr).

    Test: tests/test_pending_answers_hook.py
    """
    text = owner_turn_text(d)
    if text is None:
        return
    path = os.environ.get("ARCHITECT_OWNER_TURNS") or os.path.join(ROOT, "inbox", "owner-turns.jsonl")
    at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() if now is None else now))
    rec = {"at": at, "session": os.environ.get("ARCHITECT_SESSION") or "tm-architect",
           "text": text[:OWNER_TURN_TEXT_MAX]}
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        if os.path.getsize(path) > OWNER_TURNS_MAX:
            with open(path, encoding="utf-8") as f:
                lines = f.readlines()
            tmp = path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                f.writelines(lines[len(lines) // 2:])
            os.replace(tmp, path)
    except (OSError, ValueError) as e:   # #10780 critic M: also a lone surrogate or a non-UTF-8 byte
        sys.stderr.write(f"owner turn not recorded: {e!r}\n")


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
        record_owner_turn(d, now)   # #10780: whether or not user_state is installed; owner turns only
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
        try:  # #11064: the technical level rides next to the user state; a read or write error never blocks a tool call
            import technical_level
            sess = str(d.get("session_id", ""))
            who = turn_speaker(STATE, sess, d)
            level = technical_level.read_user(who) if who else technical_level.read(qroot)
            tline = level_line(STATE, sess, level, now, who)
            if tline:
                parts.append(tline)
        except Exception as e:  # noqa: BLE001 - StoreError, schema or file errors must not block a tool call
            sys.stderr.write(f"technical level line failed: {e!r}\n")
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

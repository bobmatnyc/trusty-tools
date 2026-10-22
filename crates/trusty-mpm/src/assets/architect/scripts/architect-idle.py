#!/usr/bin/env python3
"""Exit 0 when the Architect's Claude transcript says no turn is running.

Why: the pane-based turn_active test reads False mid-turn when the line above
the input box is not a spinner; self-clear.sh could type /clear into a running
turn. What: layout-independent second signal. Newest *.jsonl in the Architect's
project dir must be >= MIN_AGE s old and its last user/assistant record must be
an assistant message with stop_reason "end_turn" (no pending tool_use). Reads
only the last 64 KB. Exit 0 idle, 1 busy, 2 unknown (treated as busy by callers).
Test: test_architect_idle.py
"""
import importlib.util
import json
import os
import sys
import time

_spec = importlib.util.spec_from_file_location(
    "self_ctx", os.path.join(os.path.dirname(os.path.abspath(__file__)), "self-ctx.py"))
self_ctx = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(self_ctx)
PROJ = self_ctx.DEFAULT_DIR
MIN_AGE = 20
TAIL = 64 * 1024


def newest_transcript(d=PROJ):
    best, best_m = None, -1.0
    with os.scandir(d) as it:
        for e in it:
            if e.name.endswith(".jsonl") and e.is_file():
                m = e.stat().st_mtime
                if m > best_m:
                    best, best_m = e.path, m
    return best


def tail_idle(data):
    """True/False from raw tail bytes; None when no user/assistant record is found."""
    for line in reversed(data.decode("utf-8", "replace").splitlines()):
        try:
            rec = json.loads(line)
        except ValueError:
            continue  # partial first line of the tail, or a torn write
        msg = rec.get("message")
        if rec.get("type") not in ("assistant", "user") or not isinstance(msg, dict):
            continue
        if msg.get("role") != "assistant":
            return False  # a user prompt or tool_result: the model owes a reply
        content = msg.get("content")
        if isinstance(content, list) and any(
                isinstance(c, dict) and c.get("type") == "tool_use" for c in content):
            return False
        return msg.get("stop_reason") == "end_turn"
    return None


def check(path, now=None):
    now = time.time() if now is None else now
    try:
        st = os.stat(path)
        if now - st.st_mtime < MIN_AGE:
            return 1, f"busy: transcript written {now - st.st_mtime:.0f}s ago"
        with open(path, "rb") as f:
            f.seek(max(0, st.st_size - TAIL))
            verdict = tail_idle(f.read())
    except OSError as e:
        return 2, f"unknown: {e}"
    if verdict is None:
        return 2, "unknown: no assistant/user record in tail"
    return (0, "idle") if verdict else (1, "busy: last record is not end_turn")


if __name__ == "__main__":
    p = sys.argv[1] if len(sys.argv) > 1 else newest_transcript()
    if not p:
        print("unknown: no transcript"); sys.exit(2)
    code, why = check(p)
    print(f"{why} ({os.path.basename(p)})")
    sys.exit(code)

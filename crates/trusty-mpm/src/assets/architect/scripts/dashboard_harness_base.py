"""dashboard_harness_base.py: the Live view's window constants and bounded-read helpers (split from
dashboard_harness.py, #10571; bodies moved verbatim).

Why: dashboard_harness.py and dashboard_fleet.py both read transcripts and inbox files through the same bounded
helpers; one module keeps them in one place and keeps both files under the 500-SLOC cap.
What: WINDOW_S, ACTIVITY_S, IDLE_PARK_MIN, SLOW_TOKENS_1H, TEXT_MAX, TAIL_BYTES; _iso, one_line, tail_lines,
tail_json and read_json. dashboard_harness.py re-exports every name.
Test: tests/test_dashboard_harness.py
"""
import json
import os
import re
import time
from datetime import datetime, timezone

_SECRET = re.compile(r"(?i)(secret|token|password|passwd|api[_-]?key|bearer)\s*[:=]\s*\S+")
WINDOW_S = 30 * 60
ACTIVITY_S = 10 * 60               # a message or Architect transcript write this recent means the node is active (not park)
IDLE_PARK_MIN = 15
SLOW_TOKENS_1H = 20000
TEXT_MAX = 160
TAIL_BYTES = 256 * 1024
def _iso(ts):
    try:
        return datetime.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    except (ValueError, TypeError):
        return None


def one_line(text, n=TEXT_MAX):
    s = _SECRET.sub(lambda m: m.group(1) + "=[redacted]", " ".join(str(text or "").split()))
    return s if len(s) <= n else s[: n - 1] + "…"


def tail_lines(path, nbytes=TAIL_BYTES):
    """Last nbytes of a file as complete lines; [] when absent. Bounded read."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            data = f.read()
    except OSError:
        return None
    lines = data.decode("utf-8", "replace").splitlines()
    return lines[1:] if size > nbytes else lines


def tail_json(path, nbytes=TAIL_BYTES):
    """Parsed JSON-object lines from the tail; None when the file is absent."""
    lines = tail_lines(path, nbytes)
    if lines is None:
        return None
    out = []
    for ln in lines:
        try:
            r = json.loads(ln)
        except ValueError:
            continue
        if isinstance(r, dict):
            out.append(r)
    return out


def read_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            r = json.load(f)
        return r if isinstance(r, dict) else None
    except (OSError, ValueError):
        return None



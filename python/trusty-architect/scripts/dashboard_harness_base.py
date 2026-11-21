"""dashboard_harness_base.py: the Live view's window constants and bounded-read helpers (split from
dashboard_harness.py, #10571; bodies moved verbatim).

Why: dashboard_harness.py and dashboard_fleet.py both read transcripts and inbox files through the same bounded
helpers; one module keeps them in one place and keeps both files under the 500-SLOC cap.
What: WINDOW_S, ACTIVITY_S, IDLE_PARK_MIN, SLOW_TOKENS_1H, TEXT_MAX, TAIL_BYTES; _iso, one_line, tail_lines,
tail_json and read_json; CACHE_TTL_S and HarnessCache (moved here for #10698). dashboard_harness.py re-exports every name.
Test: tests/test_dashboard_harness.py, tests/test_dashboard_concurrency.py
"""
import json
import os
import re
import threading
import time
from datetime import datetime, timezone

_SECRET = re.compile(r"(?i)(secret|token|password|passwd|api[_-]?key|bearer)\s*[:=]\s*\S+")
WINDOW_S = 30 * 60
ACTIVITY_S = 10 * 60               # a message or Architect transcript write this recent means the node is active (not park)
IDLE_PARK_MIN = 15
SLOW_TOKENS_1H = 20000
TEXT_MAX = 160
TAIL_BYTES = 256 * 1024
CACHE_TTL_S = 4.0
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


class HarnessCache:
    """Serve one build() result for CACHE_TTL_S so concurrent pages and polls share one scan; a failed build's error is shared for CACHE_TTL_S.
    swr=True (stale-while-revalidate): once a value exists, an expired get() returns it at once and rebuilds on one background
    thread, so a slow build under host load never holds a request (Live "timed out"). The first get() still builds inline."""

    def __init__(self, ttl=CACHE_TTL_S, clock=time.time, swr=False):
        self.ttl, self.clock, self.swr, self._at, self._val, self._err = ttl, clock, swr, 0.0, None, None
        self._lock = threading.Lock()   # #10161: concurrent requests share one build() instead of racing two
        self._refreshing = False

    def _refresh(self, fn):
        try:
            val = fn()
            with self._lock:
                self._val, self._at = val, self.clock()
        except Exception as e:  # noqa: BLE001 - keep serving the stale value; the next expired get() retries
            import sys
            sys.stderr.write(f"harness refresh failed: {e!r}\n")
        finally:
            with self._lock:
                self._refreshing = False

    def get(self, fn):
        with self._lock:
            t = self.clock()
            if self.swr and self._val is not None:
                if t - self._at >= self.ttl and not self._refreshing:
                    self._refreshing = True
                    threading.Thread(target=self._refresh, args=(fn,), daemon=True).start()
                return self._val
            if self._val is None or t - self._at >= self.ttl:
                if self._err and t - self._err[1] < self.ttl:
                    raise self._err[0]   # #10698: requests queued behind a failed build share its error, not N re-runs
                try:
                    self._val, self._at, self._err = fn(), t, None
                except Exception as e:
                    self._err = (e, self.clock())
                    raise
            return self._val

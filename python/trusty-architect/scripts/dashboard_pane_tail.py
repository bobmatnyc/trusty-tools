"""dashboard_pane_tail.py: session_pane_tail, the ephemeral provider of a session's masked pane tail (#11065 phase 2).

Why: a Decision shows the session's most recent pane lines (#11065). The text is read at request time and may hold
secrets, so it is masked before it leaves this module and it is never written anywhere: no store document, no inbox
file, no log line (issue constraint 2: in memory only, with a TTL).
What: PaneTailProvider.session_pane_tail(name, n) finds the tmux pane of the session named exactly `name` through the
poller's list_panes(), captures it through the poller's capture_pane() (fleet_poll_io.py, the one tmux path) with
tmux soft wraps joined (-J), masks it with dashboard_mask.mask_pane_text(hard_wraps=True), drops trailing blank screen
rows and keeps the last n lines, each cut to LINE_MAX characters. It returns a PaneTail, or a PaneTailFailure
("not_running" or "capture_failed"): a failed capture is never an empty tail, which would read as an idle pane.
Successes are cached in a process-local dict for TTL seconds.
n: None gives DEFAULT_LINES; an int is clamped into [MIN_LINES, MAX_LINES], so the cap holds for every caller and an
over-large request still gets an answer; a bool or non-int raises TypeError (a caller bug, not a request to clamp).
The module-level session_pane_tail(name, n) uses one shared provider that loads fleet-poll.py on its first call.
Test: tests/test_dashboard_pane_tail.py
"""
import importlib.util
import os
import threading
import time
from dataclasses import dataclass

import dashboard_mask

DEFAULT_LINES = 40
MIN_LINES = 10
MAX_LINES = 200
LINE_MAX = 300      # characters per line before the "…" cut: wider than a usual pane, bounds 200 lines to ~60 KB
TTL = 3.0           # seconds a capture is served from memory (#11065 phase 3: 3 s cache)
NOT_RUNNING = "not_running"
CAPTURE_FAILED = "capture_failed"


@dataclass(frozen=True)
class PaneTail:
    """The masked tail: `lines` oldest first; `truncated` is True when older pane lines exist beyond the n kept."""
    session: str
    lines: tuple
    captured_at: float
    truncated: bool


@dataclass(frozen=True)
class PaneTailFailure:
    """No tail: `kind` is NOT_RUNNING (no pane of that exact session) or CAPTURE_FAILED (tmux did not answer)."""
    session: str
    kind: str
    detail: str


def clamp_lines(n):
    """n as the provider uses it: None gives DEFAULT_LINES, an int is clamped to [MIN_LINES, MAX_LINES]."""
    if n is None:
        return DEFAULT_LINES
    if isinstance(n, bool) or not isinstance(n, int):
        raise TypeError(f"n must be an int or None, not {type(n).__name__}")
    return max(MIN_LINES, min(MAX_LINES, n))


def _pick_pane(panes, name):
    """The active pane of the session named exactly `name`, else its first pane, else None."""
    mine = [p for p in panes if p.get("session") == name]
    for p in mine:
        if p.get("active") == "11":
            return p
    return mine[0] if mine else None


def _tail(raw, n):
    """(lines, truncated): `raw` masked whole, trailing blank rows dropped, the last n lines kept.

    #11065: hard_wraps=True also masks a secret that an application wrapped onto the next row.
    """
    masked = dashboard_mask.mask_pane_text(raw, max_lines=raw.count("\n") + 1, max_line_len=LINE_MAX,
                                          hard_wraps=True)
    lines = masked.split("\n") if masked else []
    while lines and not lines[-1].strip():
        lines.pop()
    return tuple(lines[-n:]), len(lines) > n


class PaneTailProvider:
    """Captures, masks and caches pane tails. `poller` exposes list_panes, capture_pane and TmuxError (fleet-poll.py).

    The cache maps (name, n) to (expires_at, PaneTail) in this object only; it is never written to disk.
    """

    def __init__(self, poller, *, ttl=TTL, clock=time.time):
        self._poller = poller
        self._ttl = ttl
        self._clock = clock
        self._cache = {}
        self._lock = threading.Lock()

    def session_pane_tail(self, name, n=DEFAULT_LINES):
        """PaneTail | PaneTailFailure for the session named exactly `name`; see the module docstring for n."""
        if not isinstance(name, str):
            raise TypeError(f"name must be a str, not {type(name).__name__}")
        n = clamp_lines(n)
        now = self._clock()
        with self._lock:
            for key in [k for k, (exp, _) in self._cache.items() if exp <= now]:
                del self._cache[key]
            hit = self._cache.get((name, n))
        if hit:
            return hit[1]
        result = self._capture(name, n, now)
        if isinstance(result, PaneTail):
            with self._lock:
                self._cache[(name, n)] = (now + self._ttl, result)
        return result

    def _capture(self, name, n, now):
        err = self._poller.TmuxError
        try:
            pane = _pick_pane(self._poller.list_panes(), name)
        except err as e:
            return PaneTailFailure(name, CAPTURE_FAILED, f"list-panes: {e}")
        if pane is None:
            return PaneTailFailure(name, NOT_RUNNING, f"no tmux session named {name!r}")
        try:
            raw = self._poller.capture_pane(pane, history=n, join=True)
        except err as e:
            return PaneTailFailure(name, CAPTURE_FAILED, str(e))
        if not isinstance(raw, str):
            return PaneTailFailure(name, CAPTURE_FAILED, f"capture returned {type(raw).__name__}")
        lines, truncated = _tail(raw, n)
        return PaneTail(name, lines, now, truncated)


_DEFAULT = []
_DEFAULT_LOCK = threading.Lock()


def _load_poller():
    # fleet-poll.py is hyphenated, so load it by path; loading it binds fleet_poll_io to its settings.
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location("fleet_poll_for_pane_tail", os.path.join(here, "fleet-poll.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def session_pane_tail(name, n=DEFAULT_LINES):
    """The shared provider's session_pane_tail; fleet-poll.py loads on the first call, not at import."""
    with _DEFAULT_LOCK:
        if not _DEFAULT:
            _DEFAULT.append(PaneTailProvider(_load_poller()))
    return _DEFAULT[0].session_pane_tail(name, n)

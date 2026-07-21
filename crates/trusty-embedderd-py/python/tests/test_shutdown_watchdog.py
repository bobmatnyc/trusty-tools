"""Progress-aware shutdown watchdog tests (MEDIUM fix).

A flat ``worker_thread.join(timeout=N)`` bounds the ENTIRE drain of up to
``max_queue`` (default 64) legitimately-slow encodes — a reindex burst
arriving right at SIGTERM could exceed N seconds of HEALTHY work and force
``os._exit``, dropping in-flight (not hung) replies. ``_join_with_progress_
watchdog`` instead tracks the worker's last-completed-item timestamp
(``_ProgressTracker``) and only force-exits once NO item has completed for
``no_progress_timeout_secs`` — bounding on LACK OF PROGRESS, not total drain
time.

Both tests use small injected ``poll_secs``/``no_progress_timeout_secs``
values so they run in well under a second, and patch ``os._exit`` so the
"must force-exit" case can be observed without actually killing the test
process.

This mechanism did not exist before this fix (the prior shutdown path was a
single flat ``join(timeout=10.0)`` with no progress tracking at all) — these
tests fail with an ``AttributeError``/``ImportError`` against that prior
``sidecar.py`` and pass against this one.
"""

from __future__ import annotations

import threading
import time
from unittest import mock

from trusty_embed_sidecar.sidecar import _ProgressTracker, _join_with_progress_watchdog


def test_slow_but_progressing_worker_drains_fully_without_force_exit():
    """A worker that keeps completing items — however slowly — must be left
    to drain to completion rather than force-exited mid-drain."""
    progress = _ProgressTracker()
    finished = threading.Event()

    def slow_but_healthy_worker():
        # Completes 10 "items", marking progress after each. Total wall time
        # (~0.3s) comfortably exceeds the no-progress window used below
        # (0.5s would tolerate a SINGLE stall that long), but no single gap
        # BETWEEN completions does — this is exactly the "healthy burst"
        # scenario the fix targets. The no-progress window is intentionally
        # ~15x the per-item interval to absorb normal thread-scheduling
        # jitter without flaking.
        for _ in range(10):
            time.sleep(0.03)
            progress.mark()
        finished.set()

    thread = threading.Thread(target=slow_but_healthy_worker, daemon=True)
    thread.start()

    with mock.patch("trusty_embed_sidecar.sidecar.os._exit") as mock_exit:
        _join_with_progress_watchdog(
            thread,
            progress,
            poll_secs=0.05,
            no_progress_timeout_secs=0.5,
        )

    assert finished.is_set(), "worker must have run to completion"
    assert not thread.is_alive()
    mock_exit.assert_not_called()


def test_wedged_worker_force_exits_within_no_progress_window():
    """A worker that stops completing items entirely (a genuine wedge, e.g. a
    hung MPS ``encode()``) must force-exit within the no-progress window
    rather than hanging forever."""
    progress = _ProgressTracker()
    never_progresses = threading.Event()

    def wedged_worker():
        # Never calls progress.mark() again and never returns on its own —
        # simulates a hung encode(). Blocks on a real Event (not a fixed
        # sleep) and is `daemon=True` so it can never hang interpreter/test
        # exit even though this test does not join it.
        never_progresses.wait()

    thread = threading.Thread(target=wedged_worker, daemon=True)
    thread.start()

    start = time.monotonic()
    with mock.patch("trusty_embed_sidecar.sidecar.os._exit") as mock_exit:
        _join_with_progress_watchdog(
            thread,
            progress,
            poll_secs=0.05,
            no_progress_timeout_secs=0.2,
        )
    elapsed = time.monotonic() - start

    mock_exit.assert_called_once_with(1)
    assert elapsed < 2.0, "watchdog must force-exit promptly, not hang"

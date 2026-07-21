"""Stdio serve loop for the embedding sidecar.

Why: the Rust client (`StdioEmbedderClient`) is *multi-flight* — with
``TRUSTY_EMBED_INFLIGHT`` (default 2) it writes several requests before reading
their replies, correlating responses by JSON-RPC ``id``. The read loop here
must therefore never block on encoding: a reader thread drains stdin frames
into a bounded queue and a single worker thread does the (GPU-serialized)
encode and writes replies. One worker keeps MPS access serialized (concurrent
encode from multiple threads gives no speedup and risks Metal contention)
while still decoupling reading from compute so a slow/large batch can never
wedge the stdin drain.

stdout carries ONLY newline-terminated JSON frames; every log line goes to
stderr (see ``log``). We flush stdout after every frame so the client's
``read_line`` returns promptly.

Clean shutdown: EOF on stdin (or SIGTERM handled by the caller) stops the
reader, drains any queued work, and joins the worker under a PROGRESS-AWARE
watchdog (see ``_ProgressTracker`` / ``_join_with_progress_watchdog``) rather
than a flat timeout, and returns.
"""

from __future__ import annotations

import os
import queue
import sys
import threading
import time
from typing import Optional, TextIO

from .protocol import Encoder, handle_frame


def log(msg: str) -> None:
    """Write a diagnostic line to stderr (never stdout)."""
    print(f"[trusty-embed-sidecar] {msg}", file=sys.stderr, flush=True)


# Sentinel enqueued to tell the worker to stop after draining.
_STOP = object()

# How often the shutdown watchdog re-checks the worker during drain.
_SHUTDOWN_JOIN_POLL_SECS = 1.0

# How long the worker may go with ZERO completed items during shutdown before
# the watchdog concludes it is genuinely wedged (not just slow) and
# force-exits.
#
# Why NOT a flat total-drain timeout: `max_queue` (default 64) legitimately
# slow-but-healthy encodes — e.g. a reindex burst queued right as SIGTERM
# arrives — can easily take longer than a flat ~10s bound to drain in full,
# even though every item IS completing. A flat timeout would force-exit that
# healthy drain and silently drop in-flight (not hung) replies. Bounding on
# LACK OF PROGRESS instead (no item has finished for this many seconds) only
# fires for a genuine wedge (e.g. an MPS driver stall inside `encode()`),
# while a burst that keeps completing items — however slowly — is allowed to
# drain to completion.
_SHUTDOWN_NO_PROGRESS_TIMEOUT_SECS = 20.0


class _ProgressTracker:
    """Thread-safe holder for "when did the worker last complete an item".

    Why: the shutdown watchdog (main thread) needs to distinguish "still
    legitimately draining" from "wedged" by reading a timestamp the worker
    thread writes after each completed item. A small lock-guarded wrapper
    avoids relying on any assumption about `float` write atomicity across
    threads.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._last = time.monotonic()

    def mark(self) -> None:
        """Record that an item was just completed (called by the worker)."""
        with self._lock:
            self._last = time.monotonic()

    def seconds_since_progress(self) -> float:
        """Seconds since the last completed item (called by the watchdog)."""
        with self._lock:
            return time.monotonic() - self._last


def _join_with_progress_watchdog(
    thread: threading.Thread,
    progress: _ProgressTracker,
    poll_secs: float = _SHUTDOWN_JOIN_POLL_SECS,
    no_progress_timeout_secs: float = _SHUTDOWN_NO_PROGRESS_TIMEOUT_SECS,
) -> None:
    """Join ``thread`` in short polls, force-exiting only on genuine no-progress.

    Loops ``thread.join(timeout=poll_secs)`` — bounding total drain time is
    deliberately NOT the goal here (see ``_SHUTDOWN_NO_PROGRESS_TIMEOUT_SECS``'s
    doc comment); each poll instead checks ``progress.seconds_since_progress()``
    and only force-exits (``os._exit``, bypassing further Python-level cleanup
    by design — there is nothing left to clean up safely) once that has
    exceeded ``no_progress_timeout_secs`` with the worker still alive. A
    worker that keeps completing items, however slowly, is left to drain to
    completion; a worker that stops completing items entirely (a genuine
    wedge, e.g. a hung MPS ``encode()``) is force-exited within the
    no-progress window instead of hanging this process forever.
    """
    while thread.is_alive():
        thread.join(timeout=poll_secs)
        if not thread.is_alive():
            return
        stalled_for = progress.seconds_since_progress()
        if stalled_for >= no_progress_timeout_secs:
            log(
                f"worker made no progress for {stalled_for:.1f}s during shutdown "
                "(hung encode?) — forcing process exit"
            )
            os._exit(1)
            return  # pragma: no cover — unreachable once os._exit runs for
            # real (the process is gone); only reached when a test patches
            # `os._exit` to a no-op, where it stops the watchdog loop from
            # calling it again on every subsequent poll.


def serve(
    encoder: Encoder,
    stdin: Optional[TextIO] = None,
    stdout: Optional[TextIO] = None,
    max_queue: int = 64,
) -> None:
    """Run the newline-JSON-RPC serve loop until stdin EOF.

    ``encoder`` maps a list of texts to a list of 384-dim unit-norm float
    lists. It is injected so this loop is exercised by the protocol
    conformance tests with a torch-free stub.
    """
    stdin = stdin if stdin is not None else sys.stdin
    stdout = stdout if stdout is not None else sys.stdout

    work: "queue.Queue[object]" = queue.Queue(maxsize=max_queue)
    write_lock = threading.Lock()
    progress = _ProgressTracker()

    def worker() -> None:
        while True:
            item = work.get()
            try:
                if item is _STOP:
                    return
                # ``item`` is a raw stdin line; handle_frame never raises.
                frame = handle_frame(item, encoder)  # type: ignore[arg-type]
                if frame is not None:
                    with write_lock:
                        stdout.write(frame)
                        stdout.flush()
            finally:
                work.task_done()
                # Record completion AFTER task_done so a shutdown-time watcher
                # reading `progress` never observes "done" before the queue
                # itself reflects it.
                progress.mark()

    worker_thread = threading.Thread(target=worker, name="embed-worker", daemon=True)
    worker_thread.start()

    try:
        for line in stdin:  # blocks; ends on EOF -> clean shutdown
            # Enqueue the raw line; the worker parses + encodes + writes so the
            # read loop is never blocked by a slow/large encode (multi-flight).
            work.put(line)
    finally:
        # Drain outstanding work, then stop the worker and join it under the
        # progress-aware watchdog — see `_join_with_progress_watchdog`'s doc
        # comment for why this bounds on LACK OF PROGRESS rather than total
        # drain time.
        work.put(_STOP)
        _join_with_progress_watchdog(worker_thread, progress)

    log("stdin EOF — exiting cleanly")

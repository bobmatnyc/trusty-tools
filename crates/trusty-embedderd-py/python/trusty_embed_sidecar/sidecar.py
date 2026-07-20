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
reader, drains any queued work, joins the worker, and returns.
"""

from __future__ import annotations

import queue
import sys
import threading
from typing import Optional, TextIO

from .protocol import Encoder, handle_frame


def log(msg: str) -> None:
    """Write a diagnostic line to stderr (never stdout)."""
    print(f"[trusty-embed-sidecar] {msg}", file=sys.stderr, flush=True)


# Sentinel enqueued to tell the worker to stop after draining.
_STOP = object()


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

    worker_thread = threading.Thread(target=worker, name="embed-worker", daemon=True)
    worker_thread.start()

    try:
        for line in stdin:  # blocks; ends on EOF -> clean shutdown
            # Enqueue the raw line; the worker parses + encodes + writes so the
            # read loop is never blocked by a slow/large encode (multi-flight).
            work.put(line)
    finally:
        # Drain outstanding work, then stop the worker and join it so all
        # queued replies are flushed before we exit.
        work.put(_STOP)
        worker_thread.join()

    log("stdin EOF — exiting cleanly")

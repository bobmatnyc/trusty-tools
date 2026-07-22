"""CLI entrypoint: ``python -m trusty_embed_sidecar --stdio``.

Launched by the ``trusty-embedderd-py`` Rust launcher, which the trusty-search
``EmbedderSupervisor`` spawns exactly like the reference ``trusty-embedderd``:
piped stdin/stdout, inherited stderr, and the ``--stdio`` argument.

Startup order matters: the model is loaded and warmed up (``build_encoder``)
BEFORE the serve loop begins reading stdin, so the supervisor's real-embed
readiness probe never races a cold torch import / first-MPS compile.
"""

from __future__ import annotations

import signal
import sys

from .model import build_encoder
from .sidecar import log, serve


class _ShutdownRequested(BaseException):
    """Raised from the SIGTERM handler to unwind a blocked stdin read.

    Why a dedicated ``BaseException`` (not touching stdin, and not deriving
    from ``Exception``): the main thread is typically blocked inside
    ``sidecar.serve``'s ``for line in stdin:`` read when SIGTERM arrives. A
    prior version of this handler called ``sys.stdin.close()`` from inside
    the signal handler — a reentrant call into the same (non-reentrant)
    ``BufferedReader`` that was blocked mid-read, which raised a
    ``RuntimeError`` *inside the handler* that was then swallowed by a bare
    ``except Exception`` and left the read (and the process) hung, needing
    SIGKILL.

    Instead, the handler only raises. Per PEP 475, a signal handler that
    raises propagates that exception out of the interrupted blocking syscall
    (rather than Python silently retrying it) — so this exception surfaces
    cleanly from ``for line in stdin:`` and unwinds through ``serve``'s
    ``finally`` (which still drains the queue and joins the worker) up to
    ``main``'s ``except _ShutdownRequested`` below. Subclassing
    ``BaseException`` rather than ``Exception`` also means a broad
    ``except Exception`` anywhere else in the call stack can never
    accidentally swallow it.
    """


def main() -> int:
    # ``--stdio`` is the launch-contract arg from the supervisor; accept and
    # ignore (there is no other transport in this sidecar).
    _stdio = "--stdio" in sys.argv[1:]

    # Translate SIGTERM (the supervisor's cooperative-shutdown signal) into a
    # raised exception — see ``_ShutdownRequested`` — rather than touching
    # stdin from the handler.
    def _on_sigterm(_signum, _frame):
        raise _ShutdownRequested()

    try:
        signal.signal(signal.SIGTERM, _on_sigterm)
    except (ValueError, OSError):
        # Not on the main thread / unsupported platform — EOF handling still
        # gives a clean exit.
        pass

    try:
        encoder = build_encoder(log=log)
    except _ShutdownRequested:
        log("SIGTERM received during startup — shutting down")
        return 0
    except Exception as exc:  # noqa: BLE001
        # A model-load failure must be a NON-zero exit so the Rust launcher's
        # supervisor sees a failed startup probe and trusty-search falls back
        # to the ort path rather than hanging.
        log(f"FATAL: model load failed: {exc}")
        return 1

    try:
        serve(encoder)
    except _ShutdownRequested:
        # ``serve``'s own ``finally`` already drained the queue and joined the
        # worker (bounded — see ``sidecar.py``) before this propagated here.
        log("SIGTERM received — shut down cleanly")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

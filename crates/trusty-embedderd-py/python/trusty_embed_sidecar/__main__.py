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


def main() -> int:
    # ``--stdio`` is the launch-contract arg from the supervisor; accept and
    # ignore (there is no other transport in this sidecar).
    _stdio = "--stdio" in sys.argv[1:]

    # Translate SIGTERM (the supervisor's cooperative-shutdown signal) into a
    # clean stdin close so ``serve`` drains and exits with code 0 rather than
    # dying on the default SIGTERM disposition.
    def _on_sigterm(_signum, _frame):
        log("SIGTERM received — closing stdin for clean shutdown")
        try:
            sys.stdin.close()
        except Exception:
            pass

    try:
        signal.signal(signal.SIGTERM, _on_sigterm)
    except (ValueError, OSError):
        # Not on the main thread / unsupported platform — EOF handling still
        # gives a clean exit.
        pass

    try:
        encoder = build_encoder(log=log)
    except Exception as exc:  # noqa: BLE001
        # A model-load failure must be a NON-zero exit so the Rust launcher's
        # supervisor sees a failed startup probe and trusty-search falls back
        # to the ort path rather than hanging.
        log(f"FATAL: model load failed: {exc}")
        return 1

    serve(encoder)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

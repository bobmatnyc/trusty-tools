"""Real-signal shutdown regression test (code-critic HIGH fix).

``test_serve_loop_end_to_end_over_pipes`` (in ``test_protocol_conformance.py``)
only exercises clean shutdown via stdin EOF (an in-memory ``io.StringIO``) — it
never sends a real ``SIGTERM`` to a real process. That left a genuine hang
uncaught: the previous SIGTERM handler called ``sys.stdin.close()`` from
*inside* the handler while the main thread was blocked in ``sidecar.serve``'s
``for line in stdin:`` read. That is a reentrant call into the same
(non-reentrant) ``BufferedReader`` that was mid-read, which raised a
``RuntimeError`` *inside the handler* — swallowed there by a bare
``except Exception`` — leaving the read (and the process) hung forever
(reproduced: the process needed ``SIGKILL``).

This test spawns the sidecar's real ``main()`` as a subprocess with stdin held
OPEN (never closed, never EOF'd — the only way out is the signal), sends a
real ``SIGTERM``, and asserts the process exits promptly. ``build_encoder`` is
monkeypatched to a torch-free stub inside the subprocess so this test has no
model/network/torch dependency and runs in every environment, exactly like the
rest of this conformance suite.

Must fail before the fix (the process hangs and this test times out / has to
kill it) and pass after (clean, prompt exit 0).
"""

from __future__ import annotations

import signal
import subprocess
import sys
import time

# Subprocess script: stub out the (slow, torch-dependent) model load with an
# instant fake encoder, then run the real `main()` — including the real
# signal-handling and `serve()` shutdown path under test.
_SUBPROCESS_SCRIPT = """
import sys
from unittest import mock

from trusty_embed_sidecar import __main__ as m


def _stub_build_encoder(log=None):
    def _encode(texts):
        return [[0.0] * 384 for _ in texts]

    return _encode


with mock.patch.object(m, "build_encoder", _stub_build_encoder):
    raise SystemExit(m.main())
"""

_WAIT_FOR_EXIT_SECS = 5.0


def test_sigterm_with_stdin_open_exits_promptly(tmp_path):
    script = tmp_path / "run_sidecar.py"
    script.write_text(_SUBPROCESS_SCRIPT)

    proc = subprocess.Popen(
        [sys.executable, str(script)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        # Let the subprocess reach `serve`'s blocking `for line in stdin:`
        # read. stdin is never closed/written-to by this test, so the only
        # way the process can exit is via the signal sent below — this is
        # exactly the state (blocked mid-read) that reproduced the hang.
        time.sleep(0.3)
        assert proc.poll() is None, "subprocess exited before SIGTERM was sent"

        proc.send_signal(signal.SIGTERM)

        try:
            returncode = proc.wait(timeout=_WAIT_FOR_EXIT_SECS)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=_WAIT_FOR_EXIT_SECS)
            raise AssertionError(
                f"sidecar did not exit within {_WAIT_FOR_EXIT_SECS}s of a real "
                "SIGTERM while stdin was held open — this is the reentrant "
                "stdin.close() hang regressing (needs the _ShutdownRequested fix "
                "in __main__.py)"
            ) from None

        assert returncode == 0, (
            f"expected a clean exit 0 on SIGTERM, got {returncode}; stderr:\\n"
            f"{proc.stderr.read()}"
        )
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()

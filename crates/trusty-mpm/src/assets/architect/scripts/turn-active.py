#!/usr/bin/env python3
"""Exit 0 when the pane capture on stdin shows a running turn, 10 when it shows none, 3 on any error.

Why: self-clear.sh must not type /clear mid-turn; its own grep missed spinners without "esc to interrupt". A crash
used to exit 1, the same code as "not running", so a broken check let a clear through (#10579). Only 10 means idle;
every other code, 3 included, means busy.
What: fleet-poll's parse_pane "turn_active" (status line has "…"/"esc to interrupt", or a current-turn WAITING
line); background agent rows are ignored, since the Architect always has monitors. Any exception (fleet-poll missing,
no `turn_active` field, unreadable stdin) exits 3, never 10.
Test: tests/test_turn_active.py
"""
import importlib.util
import os
import sys

RUNNING, IDLE, ERROR = 0, 10, 3


def main(stdin_text):
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(here, "fleet-poll.py"))
    fp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fp)
    return RUNNING if fp.parse_pane(stdin_text)["turn_active"] else IDLE


if __name__ == "__main__":
    try:
        code = main(sys.stdin.read())
    except BaseException as e:  # noqa: BLE001 - any failure is "busy", never "idle"
        sys.stderr.write(f"turn-active: cannot tell ({type(e).__name__}: {e}); treat as busy\n")
        code = ERROR
    sys.exit(code)

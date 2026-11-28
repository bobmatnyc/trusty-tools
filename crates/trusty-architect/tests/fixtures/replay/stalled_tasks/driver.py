"""Replay driver for stalled_tasks.py (#12028): the script's command line, on the working directory.

Why: the script's command line reads the ledger and the dashboard items beside its own checkout, which is
outside a recording sandbox. This file is copied beside the frozen module at record time; it points both
paths at the working directory (the sandbox root) and then does what the script's __main__ block does.
examples/replay_port.rs mirrors it as `replay_port stalled_tasks`.
A load_state error, a traceback in the script, is printed as the traceback's last line (exit 1).
Usage: driver.py [hook]    (hook: unasked_line with its default cap, as the prompt hook calls it)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import datg  # noqa: E402
import stalled_tasks as s  # noqa: E402

ITEMS = os.path.join("local", "owner-queue", "items")
IL = os.path.join(os.path.dirname(ITEMS), "..", "il")
s.load_open_asks.__defaults__ = (ITEMS,)


def main(argv):
    if argv not in ([], ["hook"]):
        print("usage: driver.py [hook]", file=sys.stderr)
        return 2
    try:
        st = datg.load_state(IL)
    except Exception as e:  # noqa: BLE001 - the script's traceback, last line
        print(f"{type(e).__module__}.{type(e).__name__}: {e}", file=sys.stderr)
        return 1
    unasked = s.unasked_line(st) if argv else s.unasked_line(st, cap=None)
    for ln in (s.line(st), unasked):
        print(ln or "none")
    return 0


sys.exit(main(sys.argv[1:]))

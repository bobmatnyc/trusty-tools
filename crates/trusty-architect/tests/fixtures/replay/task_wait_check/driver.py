"""Replay driver for task_wait_check.py (#12028): the script's main(), on the working directory.

Why: main() reads the ledger and the dashboard items beside the script's own checkout, which is outside a
recording sandbox. This file is copied beside the frozen module at record time; it points both paths at the
working directory (the sandbox root) and runs main() unchanged. examples/replay_port.rs mirrors it as
`replay_port task_wait_check`.
Usage: driver.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import task_wait_check as w  # noqa: E402

w.IL_DIR = os.path.join("local", "il")
w.load_items.__defaults__ = (os.path.join("local", "owner-queue", "items"),)
sys.exit(w.main())

#!/usr/bin/env python3
"""Exit 0 when the pane capture on stdin shows a running turn, else 1.

Why: self-clear.sh must not type /clear mid-turn; its own grep missed spinners
without "esc to interrupt". What: fleet-poll's parse_pane "turn_active" (status
line has "…"/"esc to interrupt", or a current-turn WAITING line); background
agent rows are ignored, since the Architect always has monitors.
Test: test_fleet_poll_idle.py
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fp)
sys.exit(0 if fp.parse_pane(sys.stdin.read())["turn_active"] else 1)

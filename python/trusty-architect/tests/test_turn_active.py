"""turn-active.py exit contract (#10579): 0 turn running, 10 idle, 3 on any internal error (never 10).

Covers: a fleet-poll whose parse_pane reports a running turn exits 0; one reporting none exits 10; a fleet-poll
that raises, or lacks the `turn_active` field, exits 3. Each case runs a copy of turn-active.py beside a stub
fleet-poll.py in a temp dir, so the contract is tested apart from the poller's pane rules.
Test: tests/test_turn_active.py
"""
import os
import shutil
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def run(tmp_path, fleet_poll_body):
    shutil.copy(os.path.join(SCRIPTS, "turn-active.py"), tmp_path / "turn-active.py")
    (tmp_path / "fleet-poll.py").write_text(fleet_poll_body)
    return subprocess.run([sys.executable, str(tmp_path / "turn-active.py")], input="pane text\n",
                          capture_output=True, text=True).returncode


def test_running_turn_exits_0(tmp_path):
    assert run(tmp_path, "def parse_pane(t):\n    return {'turn_active': True}\n") == 0


def test_idle_exits_10(tmp_path):
    assert run(tmp_path, "def parse_pane(t):\n    return {'turn_active': False}\n") == 10


def test_an_internal_error_exits_3_never_idle(tmp_path):
    assert run(tmp_path, "def parse_pane(t):\n    raise RuntimeError('boom')\n") == 3
    assert run(tmp_path, "def parse_pane(t):\n    return {}\n") == 3   # no turn_active field
    assert run(tmp_path, "raise SystemExit(10)\n") == 3                 # even a stray 10 from the loader is busy

"""turn-active.py exit contract (#10579): 0 turn running, 10 idle, 3 on any internal error (never 10).

Covers: a fleet-classify whose parse_pane reports a running turn exits 0; one reporting none exits 10; a
fleet-classify that raises, or lacks the `turn_active` field, exits 3. Those cases run a copy of turn-active.py
beside a stub fleet-classify.py in a temp dir, so the contract is tested apart from the pane rules. The
regression case pipes a real Architect pane (the capture that exited 3 with KeyError 'turn_active' before
#10579) into the shipped script and checks it now answers, and agrees with parse_pane.
Test: tests/test_turn_active.py
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "turn_active")
PANE = os.path.join(FIXTURES, "architect-pane-10579.txt")


def run(tmp_path, classify_body):
    shutil.copy(os.path.join(SCRIPTS, "turn-active.py"), tmp_path / "turn-active.py")
    (tmp_path / "fleet-classify.py").write_text(classify_body)
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


def test_real_architect_pane_answers_and_agrees_with_parse_pane():
    with open(PANE, encoding="utf-8") as f:
        pane = f.read()
    with open(os.path.join(FIXTURES, "actual-turn-active-result.json"), encoding="utf-8") as f:
        before = json.load(f)
    assert before["exit"] == 3 and "turn_active" in before["stderr"]   # the recorded failure this fixes
    r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "turn-active.py")], input=pane,
                       capture_output=True, text=True)
    assert r.returncode in (0, 10), r.stderr
    spec = importlib.util.spec_from_file_location("fleet_classify", os.path.join(SCRIPTS, "fleet-classify.py"))
    fc = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fc)
    assert r.returncode == (0 if fc.parse_pane(pane)["turn_active"] else 10)
    assert r.returncode == 10   # the capture ends on a finished turn ("Cooked for 41s · done") at an empty box

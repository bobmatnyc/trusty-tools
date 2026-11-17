"""HOLD is a typed Intent Ledger session field (#10579, Architect ruling 2026-10-31).

Covers: `session set --hold TEXT` stores it and the digest SESSION block shows it first; `--hold none` clears it and
the digest stops showing it; other session fields are untouched by a hold change.
Test: tests/test_datg_hold.py
"""
import json
import os
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
DATG = os.path.join(SCRIPTS, "datg.py")


def run(project, *args):
    return subprocess.run([sys.executable, DATG, "--project", str(project), *args], capture_output=True, text=True, check=True)


def state(project):
    with open(os.path.join(project, ".trusty-mpm", "datg", "state.json"), encoding="utf-8") as f:
        return json.load(f)


def test_hold_is_set_shown_first_and_cleared(tmp_path):
    run(tmp_path, "init", "--code", "ar")
    run(tmp_path, "session", "set", "--next", "ship the dashboard")
    run(tmp_path, "session", "set", "--hold", "no merges until the owner says so")
    ses = state(tmp_path)["session"]
    assert (ses["hold"], ses["next"]) == ("no merges until the owner says so", "ship the dashboard")
    digest = run(tmp_path, "digest").stdout
    block = digest[digest.index("SESSION"):]
    assert block.index("hold: no merges until the owner says so") < block.index("next: ship the dashboard")
    run(tmp_path, "session", "set", "--hold", "none")
    assert state(tmp_path)["session"]["hold"] == ""
    assert "hold:" not in run(tmp_path, "digest").stdout
    assert state(tmp_path)["session"]["next"] == "ship the dashboard"

"""End-to-end tests for the `cli.py` subprocess entrypoint.

Why: `crates/trusty-agents/src/tools/python_skill.rs` invokes this module as
a subprocess, not as a Python import — these tests drive it the same way
(argv + stdin + stdout), which is the actual contract the Rust bridge
depends on, not just the Python-level `db.dispatch` behaviour already
covered by `test_db.py`.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

PYTHON_DIR = Path(__file__).resolve().parent.parent
FIXTURE_DB = PYTHON_DIR / "fixtures" / "cto_fixture.db"


def _run(tool_name: str, args: dict, env_overrides: dict | None = None) -> subprocess.CompletedProcess:
    import os

    env = os.environ.copy()
    env["PYTHONPATH"] = str(PYTHON_DIR / "src") + os.pathsep + env.get("PYTHONPATH", "")
    # #4860: the fixture is opt-in, so these fixture-backed cases must ask for
    # it by name. A developer's real `CTO_DB_PATH` is dropped so the suite
    # queries the committed fixture rather than whatever is on their machine.
    env.pop("CTO_DB_PATH", None)
    env["CTO_DB_USE_FIXTURE"] = "1"
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [sys.executable, "-m", "cto_db_skill.cli", tool_name],
        input=json.dumps(args),
        capture_output=True,
        text=True,
        env=env,
        timeout=10,
        check=False,
    )


def test_cli_query_headcount_against_fixture() -> None:
    proc = _run("query_headcount", {"filter_by": "team"})
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["filter_by"] == "team"
    assert isinstance(payload["groups"], list)
    assert len(payload["groups"]) > 0


# #4860: fixture output must be identifiable downstream, not indistinguishable
# from a real answer.
def test_cli_stamps_the_resolved_source_on_every_response() -> None:
    proc = _run("query_headcount", {"filter_by": "team"})
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["db_path"] == str(FIXTURE_DB)
    assert payload["is_fixture"] is True


def test_cli_refuses_when_no_database_is_configured() -> None:
    import os

    env = os.environ.copy()
    env["PYTHONPATH"] = str(PYTHON_DIR / "src") + os.pathsep + env.get("PYTHONPATH", "")
    env.pop("CTO_DB_PATH", None)
    env.pop("CTO_DB_USE_FIXTURE", None)
    proc = subprocess.run(
        [sys.executable, "-m", "cto_db_skill.cli", "query_headcount"],
        input=json.dumps({"filter_by": "team"}),
        capture_output=True,
        text=True,
        env=env,
        timeout=10,
        check=False,
    )
    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "CTO_DB_PATH" in payload["error"]
    assert "groups" not in payload


def test_cli_query_budget_no_args() -> None:
    proc = _run("query_budget", {})
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert "rows" in payload


def test_cli_query_risks_with_severity() -> None:
    proc = _run("query_risks", {"severity": "low"})
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["severity"] == "low"


def test_cli_query_work_classification() -> None:
    proc = _run("query_work_classification", {})
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert "rows" in payload


def test_cli_unknown_tool_exits_nonzero_with_json_error() -> None:
    proc = _run("query_nonexistent", {})
    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "error" in payload


def test_cli_unknown_filter_exits_nonzero_with_json_error() -> None:
    proc = _run("query_headcount", {"filter_by": "bogus"})
    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "error" in payload


def test_cli_missing_db_is_recoverable_json_error() -> None:
    proc = _run(
        "query_headcount",
        {},
        env_overrides={"CTO_DB_PATH": "/tmp/definitely-not-a-real-cto-db.sqlite"},
    )
    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "error" in payload


def test_cli_malformed_stdin_json_is_recoverable_error() -> None:
    import os

    env = os.environ.copy()
    env["PYTHONPATH"] = str(PYTHON_DIR / "src") + os.pathsep + env.get("PYTHONPATH", "")
    proc = subprocess.run(
        [sys.executable, "-m", "cto_db_skill.cli", "query_headcount"],
        input="not valid json{{{",
        capture_output=True,
        text=True,
        env=env,
        timeout=10,
        check=False,
    )
    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "error" in payload

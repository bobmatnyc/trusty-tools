"""`datg.py ready` (#10579): exit 0 ready to clear, 1 not ready with plain reasons, 2 missing or unreadable.

Covers: a HOLD-only ledger is not ready and the digest still shows the missing-session sentinel; a placeholder
next ("x") is not ready; a fully recorded ledger (task, next, an active goal, owed and crons stated, a fresh write)
is ready, also in --json; a missing ledger exits 2.
Test: tests/test_datg_ready.py
"""
import json
import os
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
DATG = os.path.join(SCRIPTS, "datg.py")


def run(project, *args, check=True):
    return subprocess.run([sys.executable, DATG, "--project", str(project), *args], capture_output=True, text=True, check=check)


def test_a_hold_only_ledger_is_not_ready_and_the_digest_says_so(tmp_path):
    run(tmp_path, "init", "--code", "ar")
    run(tmp_path, "session", "set", "--hold", "HOLD since 01:49Z")
    r = run(tmp_path, "ready", check=False)
    assert r.returncode == 1
    assert "no task recorded" in r.stdout and "no next recorded" in r.stdout
    assert "no open goal or task item" in r.stdout
    digest = run(tmp_path, "digest").stdout
    session = digest[digest.index("SESSION"):]
    assert "none recorded" in session and "hold: HOLD since 01:49Z" in session


def test_task_and_next_without_an_open_item_is_not_ready(tmp_path):
    # the partial session #10599 hit: task and next recorded, nothing open to resume
    run(tmp_path, "init", "--code", "ar")
    run(tmp_path, "session", "set", "--task", "ship the dashboard", "--next", "run the sandbox check",
        "--owe", "none", "--cron", "none")
    r = run(tmp_path, "ready", check=False)
    assert r.returncode == 1
    assert r.stdout.splitlines() == ["no open goal or task item: there is no saved work to resume"]


def test_a_placeholder_next_is_not_ready(tmp_path):
    run(tmp_path, "init", "--code", "ar")
    run(tmp_path, "session", "set", "--task", "ship the dashboard", "--next", "x")
    r = run(tmp_path, "ready", "--json", check=False)
    out = json.loads(r.stdout)
    assert (r.returncode, out["ready"]) == (1, False)
    assert any("no next recorded" in x for x in out["reasons"])


def test_a_fully_recorded_ledger_is_ready(tmp_path):
    run(tmp_path, "init", "--code", "ar")
    run(tmp_path, "add", "G", "--title", "Ship P1", "--rank", "1", "--finish", "the dashboard runs")
    run(tmp_path, "session", "set", "--task", "ship the dashboard", "--next", "run the sandbox check",
        "--owe", "none", "--cron", "none", "--hold", "HOLD until the reset")
    r = run(tmp_path, "ready", check=False)
    assert r.returncode == 0, r.stdout
    out = json.loads(run(tmp_path, "ready", "--json").stdout)
    assert out["ready"] is True and out["reasons"] == [] and out["hold"] == "HOLD until the reset" and out["written_at"]


def test_a_missing_ledger_exits_2(tmp_path):
    r = run(tmp_path / "none", "ready", check=False)
    assert r.returncode == 2 and "missing" in r.stdout


def _complete(project):
    run(project, "init", "--code", "ar")
    run(project, "add", "G", "--title", "Ship P1", "--rank", "1", "--finish", "the dashboard runs")


def test_a_stale_write_is_not_ready(tmp_path):
    _complete(tmp_path)
    run(tmp_path, "session", "set", "--task", "ship the dashboard", "--next", "run the sandbox check",
        "--owe", "none", "--cron", "none")
    r = run(tmp_path, "ready", "--since", "2999-01-01T00:00Z", check=False)
    assert r.returncode == 1
    assert r.stdout.splitlines()[-1].startswith("the ledger was not written after the latest change")


def test_missing_owed_and_crons_are_named(tmp_path):
    _complete(tmp_path)
    run(tmp_path, "session", "set", "--task", "ship the dashboard", "--next", "run the sandbox check")
    r = run(tmp_path, "ready", check=False)
    assert r.returncode == 1
    assert r.stdout.splitlines() == [
        "what is owed is not recorded; set it with datg.py session set --owe CHECK=EXPECT@REF or --owe none",
        "the scheduled jobs are not recorded; set them with datg.py session set --cron ... or --cron none",
    ]


def test_the_migration_placeholders_are_not_ready(tmp_path):
    _complete(tmp_path)
    run(tmp_path, "session", "set", "--task", "needs owner input: what is the Architect working on?",
        "--next", "needs owner input: what is the next step?", "--owe", "none", "--cron", "none")
    r = run(tmp_path, "ready", check=False)
    assert r.returncode == 1
    lines = r.stdout.splitlines()
    assert [line.split(" (")[0] for line in lines] == ["no task recorded", "no next recorded"]
    assert all("Set the current task and next step: run python3 scripts/datg.py --project" in line for line in lines)

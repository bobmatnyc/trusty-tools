"""Task link on D and Blocker items (the owner 2026-10-28): a question or action without `task` warns on stderr, never refuses.

Why: all Architect work is a Task under a Session under a Project; decisions and blockers must name their task.
What: dashboard-db.py warns on set/create/update when kind is question|action and `task` is missing; schema accepts `task`.
Test: tests/test_dashboard_task_link.py
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")


def cli(root, *args):
    p = subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", str(root), *args],
                       capture_output=True, text=True)
    return p.returncode, p.stdout, p.stderr


def item(kind, **kw):
    return json.dumps({"kind": kind, "status": "open", "title": "t", "created": "2026-10-28T00:00:00Z", "updated": "2026-10-28T00:00:00Z", **kw})


def test_warns_without_task_but_writes(tmp_path):
    root = tmp_path / "owner-queue"
    for kind in ("question", "action"):
        rc, _, err = cli(root, "create", "items", "D" if kind == "question" else "B", "--json", item(kind))
        assert rc == 0 and "no `task`" in err
    rc, _, err = cli(root, "set", "items", "D90", "--json", item("question"))
    assert rc == 0 and "item D90" in err


def test_no_warning_with_task_or_for_tasks(tmp_path):
    root = tmp_path / "owner-queue"
    assert cli(root, "create", "items", "D", "--json", item("question", task="T44"))[2] == ""
    assert cli(root, "create", "items", "T", "--json", item("task", steps=[{"id": "1", "title": "Next: t"}]))[2] == ""


def test_update_checks_merged_doc(tmp_path):
    root = tmp_path / "owner-queue"
    cli(root, "set", "items", "D91", "--json", item("question", task="T44"))
    rc, _, err = cli(root, "update", "items", "D91", "--json", '{"title": "x"}', "--if-version", "1")
    assert rc == 0 and err == ""

"""Every open task is progress tracked (ar-T33, the owner 2026-10-30): dashboard-db refuses an open task with no steps and no progress; validate lists any.

Test: tests/test_dashboard_task_progress.py
"""
import importlib.util
import json
import os

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)

STEP = [{"id": "1", "title": "Next: x"}]


def put(root, doc_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def run(root, *argv):
    return db.main(["--root", str(root), *argv])


@pytest.mark.parametrize("data,ok", [
    ({"kind": "task", "status": "open", "title": "x"}, False),
    ({"kind": "task", "status": "open", "title": "x", "steps": []}, False),
    ({"kind": "task", "status": "running", "title": "x"}, False),
    ({"kind": "task", "status": "open", "title": "x", "steps": STEP}, True),
    ({"kind": "task", "status": "open", "title": "x", "progress": {"pct": 0}}, True),
    ({"kind": "task", "status": "done", "title": "x"}, True),
    ({"kind": "question", "status": "open", "title": "x"}, True),
])
def test_create_refuses_untracked_open_task(tmp_path, data, ok):
    assert (run(tmp_path, "create", "items", "T", "--json", json.dumps(data)) == 0) is ok


def test_update_checks_merged_doc(tmp_path, capsys):
    put(tmp_path, "T01", kind="task", status="open", title="x", steps=STEP)
    assert run(tmp_path, "update", "items", "T01", "--if-version", "1", "--json", '{"title": "y"}') == 0   # steps survive the merge
    put(tmp_path, "T02", kind="task", status="open", title="x")
    assert run(tmp_path, "update", "items", "T02", "--if-version", "1", "--json", '{"title": "y"}') == 2   # still untracked
    assert run(tmp_path, "update", "items", "T02", "--if-version", "1", "--json", '{"status": "done"}') == 0  # closing is allowed
    assert "every open task needs progress" in capsys.readouterr().err


def test_validate_lists_untracked_open_tasks(tmp_path, capsys):
    put(tmp_path, "T01", kind="task", status="open", title="x")
    put(tmp_path, "T02", kind="task", status="open", title="x", progress={"pct": 10})
    assert run(tmp_path, "validate") == 1
    out = capsys.readouterr().out
    assert "items/T01: open task has no steps and no progress" in out
    assert "items/T02: open task has no steps" not in out

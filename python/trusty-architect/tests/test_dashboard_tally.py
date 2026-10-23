"""dashboard-db tally: deterministic open counts by kind, singular/plural wording, --json shape.

Test: scripts/test_dashboard_tally.py
"""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)


def put(root, doc_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def test_tally_line_and_json(tmp_path, capsys):
    put(tmp_path, "D01", kind="question", status="open")
    put(tmp_path, "D02", kind="question", status="open")
    put(tmp_path, "A01", kind="action", status="open")
    put(tmp_path, "T01", kind="action", status="open")   # T id, kind action: counts by kind
    put(tmp_path, "T02", kind="task", status="open")
    put(tmp_path, "D03", kind="question", status="answered")
    put(tmp_path, "D04", kind="question", status="done")
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "5 open dashboard items: 2 questions, 2 actions, 1 task\n"
    assert db.main(["--root", str(tmp_path), "tally", "--json"]) == 0
    assert json.loads(capsys.readouterr().out) == {
        "open": 5, "questions": 2, "actions": 2, "tasks": 1, "answered_unrelayed": 1, "parked": 0, "running": 0}


def test_tally_empty_store_and_singular(tmp_path, capsys):
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "0 open dashboard items: 0 questions, 0 actions, 0 tasks\n"
    put(tmp_path, "D01", kind="question", status="open")
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "1 open dashboard item: 1 question, 0 actions, 0 tasks\n"

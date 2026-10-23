"""dashboard-db summary: read-only resume lines (user_state, open counts, answers to relay, open questions).

Test: scripts/test_dashboard_summary.py
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


def put(root, coll, doc_id, **data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def store(root):
    put(root, "meta", "settings", user_state="remote", user_state_at="2026-10-19T20:06Z")
    put(root, "items", "D01", kind="question", status="open")
    put(root, "items", "A01", kind="action", status="open")
    put(root, "items", "T01", kind="task", status="open")
    put(root, "items", "D02", kind="question", status="answered", answer="x" * 90, answered_at="2026-10-19T10:00:00Z",
        session="tm-apex")
    put(root, "items", "D03", kind="question", status="answered", answer="done", answered_at="2026-10-19T10:00:00Z",
        relayed="2026-10-19T11:00Z tm-apex")
    put(root, "questions", "Q00", status="cleared", cleared_at="2026-10-14T04:46:09Z")
    put(root, "questions", "Q01", status="open")


def test_summary_lines_and_exit_code(tmp_path, capsys):
    store(tmp_path)
    rc = db.main(["--root", str(tmp_path), "summary"])
    out = capsys.readouterr().out.splitlines()
    assert rc == 0
    assert out[0] == "user_state: remote since 2026-10-19T20:06Z"
    assert out[1] == "open: 3 (D 1, A 1, T 1)"
    assert out[2] == "answers to relay: 1"
    assert out[3].startswith("  D02 answered tm-apex 2026-10-19T10:00:00Z x")
    assert len(out[3].strip().split(" ", 4)[4]) == 60
    assert out[-1] == "questions open: 1"
    assert len(out) == 5


def test_summary_is_read_only_and_defaults_without_settings(tmp_path, capsys):
    put(tmp_path, "items", "D01", kind="question", status="open")
    before = sorted((p.name, p.read_text()) for p in tmp_path.rglob("*.json"))
    assert db.main(["--root", str(tmp_path), "summary"]) == 0
    assert capsys.readouterr().out.startswith("user_state: active since unknown\nopen: 1")
    assert before == sorted((p.name, p.read_text()) for p in tmp_path.rglob("*.json"))


def test_have_architect_do_answer_is_listed_as_architect_do(tmp_path, capsys):
    put(tmp_path, "items", "A01", kind="action", status="answered", answer="Architect Do", answer_note="check the log",
        answered_at="2026-10-23T10:00:00Z")
    put(tmp_path, "items", "A02", kind="action", status="answered", answer="Have Architect Do", answer_note="Relayed 10:05Z",
        answered_at="2026-10-23T10:00:00Z")
    assert db.main(["--root", str(tmp_path), "summary"]) == 0
    out = capsys.readouterr().out.splitlines()
    assert "answers to relay: 1" in out
    assert any(" architect_do " in l and l.endswith("Architect Do: check the log") for l in out)


def test_old_have_architect_do_answer_still_listed(tmp_path, capsys):
    put(tmp_path, "items", "A03", kind="action", status="answered", answer="Have Architect Do", answer_note="old",
        answered_at="2026-10-23T10:00:00Z")
    assert db.main(["--root", str(tmp_path), "summary"]) == 0
    assert any(" architect_do " in l for l in capsys.readouterr().out.splitlines())

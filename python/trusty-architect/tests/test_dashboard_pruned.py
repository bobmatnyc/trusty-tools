"""Prune log: every dashboard delete is appended to <root>/../owner-queue-archive/pruned.jsonl first.

Why: owner (2026-10-12 03:35Z): log all dash items after pruning, for error decision tracing.
Covers: single delete and batch delete log the full document, fail-closed on a log error, both store
formats, and the `pruned` read verb filters. Test: scripts/test_dashboard_pruned.py
"""
import json
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402

DOC = {"status": "answered", "options": [{"label": "A"}], "answer": "A", "answer_note": "n",
       "outcome": "done ok", "created": "2026-10-01T00:00:00Z", "updated": "2026-10-02T00:00:00Z"}


def cli(root, *args, env=None):
    p = subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", str(root), *args],
                       capture_output=True, text=True, env={**os.environ, **(env or {})})
    return p.returncode, p.stdout, p.stderr


def log_path(root):
    return os.path.join(os.path.dirname(str(root)), "owner-queue-archive", "pruned.jsonl")


def lines(root):
    with open(log_path(root)) as f:
        return [json.loads(x) for x in f.read().splitlines()]


@pytest.fixture(params=["dir", "jsonl"])
def env(request, tmp_path):
    root = tmp_path / "owner-queue"
    return root, {"DASHBOARD_FORMAT": request.param}


def test_delete_logs_full_doc(env):
    root, e = env
    assert cli(root, "set", "items", "D01", "--json", json.dumps(DOC), env=e)[0] == 0
    assert cli(root, "--actor", "tester", "delete", "items", "D01", env=e)[0] == 0
    (rec,) = lines(root)
    assert (rec["collection"], rec["id"], rec["version"], rec["actor"]) == ("items", "D01", 1, "tester")
    assert rec["data"] == DOC
    assert rec["pruned_at"].endswith("Z")
    assert cli(root, "get", "items", "D01", env=e)[0] == 1


def test_batch_delete_logs_each(env, tmp_path):
    root, e = env
    for i in ("D01", "D02"):
        cli(root, "set", "items", i, "--json", json.dumps({**DOC, "n": i}), env=e)
    ops = tmp_path / "ops.json"
    ops.write_text(json.dumps([{"op": "delete", "collection": "items", "id": "D01"},
                               {"op": "delete", "collection": "items", "id": "D02"}]))
    assert cli(root, "batch", str(ops), env=e)[0] == 0
    assert [(r["id"], r["data"]["n"]) for r in lines(root)] == [("D01", "D01"), ("D02", "D02")]


def test_missing_doc_delete_logs_nothing(env):
    root, e = env
    assert cli(root, "delete", "items", "nope", env=e)[0] == 1
    assert not os.path.exists(log_path(root))


def test_log_failure_blocks_delete(env, tmp_path):
    root, e = env
    cli(root, "set", "items", "D01", "--json", json.dumps(DOC), env=e)
    (tmp_path / "owner-queue-archive").write_text("a file where the log dir must be")
    rc, _, err = cli(root, "delete", "items", "D01", env=e)
    assert rc != 0 and "prune log" in err
    assert cli(root, "get", "items", "D01", env=e)[0] == 0


def test_pruned_verb_filters(env):
    root, e = env
    for coll, i in (("items", "D01"), ("items", "D02"), ("goals", "D01")):
        cli(root, "set", coll, i, "--json", json.dumps(DOC), env=e)
        cli(root, "delete", coll, i, env=e)
    rc, out, _ = cli(root, "pruned", env=e)
    assert [(r["collection"], r["id"]) for r in map(json.loads, out.splitlines())] == \
        [("items", "D01"), ("items", "D02"), ("goals", "D01")]
    out = cli(root, "pruned", "--id", "D01", env=e)[1]
    assert [r["collection"] for r in map(json.loads, out.splitlines())] == ["items", "goals"]
    out = cli(root, "pruned", "--id", "D01", "--collection", "goals", env=e)[1]
    assert len(out.splitlines()) == 1
    assert cli(root, "pruned", "--id", "zz", env=e)[1] == ""


def test_pruned_verb_without_log_is_empty(env):
    root, e = env
    assert cli(root, "pruned", env=e)[:2] == (0, "")


def test_server_does_not_read_archive_path():
    # #10161: the server is split into four modules; none of them reads the archive path
    for name in ("dashboard-serve.py", "dashboard_http.py", "dashboard_page.py", "dashboard_docs.py"):
        assert "owner-queue-archive" not in open(os.path.join(SCRIPTS, name)).read(), name

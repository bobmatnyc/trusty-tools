"""Closing an answered item keeps the owner's `answer` and `answer_note` byte-equal (#10711).

Why: the r-183 close wrote "Relayed HH:MMZ" over the text the owner typed for an "Other" answer, and the items
schema refused the `relayed` field the readers already honour, so the Architect had no allowed place for the marker.
Covers: a close that records the relay in `relayed` keeps both owner fields and passes the schema; a close (update,
set or batch) that would change `answer_note` or `answer` is refused (exit 2) and leaves the item answered; a reopen
(status open) and an Architect marker note are not owner words and stay writable.
Test: tests/test_dashboard_answer_close.py
"""
import json
import os
import subprocess
import sys

import pytest

import dashboard_schema as dsch
import dashboard_store as ds
import owner_answers as oa

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
T = "2026-11-02T01:00:00Z"
TYPED = "Ship it Friday, but keep the old flag on for a week.\n"
ANSWERED = {"kind": "question", "status": "answered", "title": "When?", "session": "tm-a", "goal": "G01", "rank": 1,
            "task": "T01", "created": T, "updated": T, "answer": "Other", "answer_note": TYPED, "answered_at": T}
CLOSE = {"status": "done", "done_at": "2026-11-02T01:43:00Z", "outcome": "Relayed to tm-a; PM scheduled it.",
         "relayed": "2026-11-02T01:42:00Z tm-a"}


def db(root, *args):
    return subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", str(root), *args],
                          capture_output=True, text=True)


def seed(tmp_path, data=ANSWERED):
    root = tmp_path / "store"
    ds.Store(str(root)).set("items", "D34", dict(data), 0)
    return root


def item(root):
    return ds.Store(str(root)).get("items", "D34")


def test_close_keeps_the_owners_answer_note_10711(tmp_path):
    root = seed(tmp_path)
    r = db(root, "update", "items", "D34", "--json", json.dumps(CLOSE), "--if-version", "1")
    assert r.returncode == 0, r.stderr
    data = item(root)["data"]
    assert (data["answer"], data["answer_note"]) == ("Other", TYPED)
    assert data["status"] == "done" and data["relayed"] == CLOSE["relayed"]
    assert dsch.validate("items", "D34", data) == []
    assert oa.pending(str(root)) == []


@pytest.mark.parametrize("field,value", [("answer_note", "Relayed 01:42Z"), ("answer", "Relayed")])
def test_close_that_overwrites_an_owner_answer_field_is_refused_10711(tmp_path, field, value):
    root = seed(tmp_path)
    r = db(root, "update", "items", "D34", "--json", json.dumps({**CLOSE, field: value}), "--if-version", "1")
    assert r.returncode == 2, r.stdout + r.stderr
    assert field in r.stderr and "relayed" in r.stderr
    doc = item(root)
    assert doc["version"] == 1 and doc["data"]["status"] == "answered" and doc["data"][field] == ANSWERED[field]


def test_set_and_batch_that_drop_the_answer_note_are_refused_10711(tmp_path):
    root = seed(tmp_path)
    closed = {k: v for k, v in {**ANSWERED, **CLOSE}.items() if k != "answer_note"}
    r = db(root, "set", "items", "D34", "--json", json.dumps(closed), "--if-version", "1")
    assert r.returncode == 2 and "answer_note" in r.stderr, r.stdout + r.stderr
    ops = tmp_path / "ops.json"
    ops.write_text(json.dumps([{"action": "update", "collection": "items", "id": "D34", "data": {**CLOSE, "answer_note": ""}}]))
    r = db(root, "batch", str(ops))
    assert r.returncode == 2 and "answer_note" in r.stderr, r.stdout + r.stderr
    assert item(root)["data"] == ANSWERED


def test_reopen_and_architect_marker_notes_stay_writable_10711(tmp_path):
    root = seed(tmp_path)
    r = db(root, "update", "items", "D34", "--json", json.dumps({"status": "open", "answer": "", "answer_note": ""}))
    assert r.returncode == 0, r.stderr
    legacy = seed(tmp_path / "legacy", {**ANSWERED, "answer_note": "Relayed 01:42Z"})
    r = db(legacy, "update", "items", "D34", "--json", json.dumps({"status": "done", "answer_note": "Relayed 01:42Z; done"}))
    assert r.returncode == 0, r.stderr

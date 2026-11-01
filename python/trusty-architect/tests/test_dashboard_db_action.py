"""dashboard-db.py batch takes `action` as a batch entry's verb, beside the older key (#10571).

Covers: an all-`action` file (set, update, delete) is applied by store.batch; a mixed file (some entries on the older
key, some on `action`) is applied in order; an entry naming both keys is refused with a plain error and nothing is
written.
Test: tests/test_dashboard_db_action.py
"""
import json
import os
import subprocess
import sys

import dashboard_store as ds

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
OLD = ds.BATCH_VERB_KEY


def batch(root, entries, tmp_path):
    f = tmp_path / "ops.json"
    f.write_text(json.dumps(entries))
    return subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", str(root), "batch", str(f)],
                          capture_output=True, text=True)


def test_action_keyed_set_update_delete_are_applied(tmp_path):
    root = tmp_path / "store"
    r = batch(root, [
        {"action": "set", "collection": "goals", "id": "G01", "data": {"title": "one"}},
        {"action": "set", "collection": "goals", "id": "G02", "data": {"title": "two"}},
        {"action": "update", "collection": "goals", "id": "G01", "data": {"title": "uno"}},
        {"action": "delete", "collection": "goals", "id": "G02"},
    ], tmp_path)
    assert r.returncode == 0, r.stderr
    store = ds.Store(str(root))
    assert store.get("goals", "G01")["data"]["title"] == "uno"
    assert [d["id"] for d in store.list_docs("goals")] == ["G01"]


def test_a_mixed_file_is_applied_in_order(tmp_path):
    root = tmp_path / "store"
    r = batch(root, [
        {OLD: "set", "collection": "goals", "id": "G01", "data": {"title": "old key"}},
        {"action": "update", "collection": "goals", "id": "G01", "data": {"title": "new key"}},
    ], tmp_path)
    assert r.returncode == 0, r.stderr
    assert ds.Store(str(root)).get("goals", "G01")["data"]["title"] == "new key"


def test_an_entry_with_both_keys_is_refused(tmp_path):
    root = tmp_path / "store"
    r = batch(root, [{"action": "set", OLD: "delete", "collection": "goals", "id": "G01", "data": {"title": "x"}}], tmp_path)
    assert r.returncode == 2
    assert "names its verb twice" in r.stderr
    assert ds.Store(str(root)).list_docs("goals") == []

"""dashboard-db.py caller-key look-up and items field check (#10766).

Covers: `create items` with a `detail_ref` an open item already carries updates that item (no duplicate);
`list --detail-ref` finds it; a closed item with the key does not stop a new one; an `items` write with a
field items.schema.json does not name (`priority`) is refused on create, set, update and batch; a dir store
that already holds such a field still reads and its item still takes a schema-valid update.
"""
import json
import os
import subprocess
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402

T = "2026-11-25T00:00:00Z"


def item(title, **extra):
    return {"kind": "question", "status": "open", "title": title, "session": "S14", "goal": None, "rank": 1,
            "created": T, "updated": T, "task": "T01", **extra}


@pytest.fixture
def cli(tmp_path):
    root = str(tmp_path / "oq")

    def run(*args):
        p = subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard-db.py"), "--root", root, *args],
                           capture_output=True, text=True)
        return p.returncode, p.stdout, p.stderr
    run.root = root
    return run


def test_create_with_an_open_items_key_updates_it_instead_of_adding_one(cli):
    rc, out, err = cli("create", "items", "D", "--json", json.dumps(item("first", detail_ref="gh:1")))
    assert rc == 0, err
    first = json.loads(out)["id"]
    rc, out, err = cli("create", "items", "D", "--json", json.dumps(item("second", detail_ref="gh:1")))
    assert rc == 0, err
    assert json.loads(out)["id"] == first
    docs = ds.Store(cli.root).list_docs("items")
    assert [(d["id"], d["data"]["title"]) for d in docs] == [(first, "second")]
    rc, out, _ = cli("list", "items", "--detail-ref", "gh:1")
    assert (rc, [d["id"] for d in json.loads(out)]) == (0, [first])
    assert json.loads(cli("list", "items", "--detail-ref", "gh:2")[1]) == []


def test_a_closed_item_with_the_key_does_not_absorb_a_new_create(cli):
    cli("create", "items", "D", "--json", json.dumps(item("old", detail_ref="gh:3", status="done")))
    cli("create", "items", "D", "--json", json.dumps(item("new", detail_ref="gh:3")))
    assert len(ds.Store(cli.root).list_docs("items")) == 2


def test_an_items_write_with_a_non_schema_field_is_refused(cli, tmp_path):
    bad = json.dumps(item("x", priority="high"))
    for args in (("create", "items", "D", "--json", bad), ("set", "items", "D01", "--json", bad)):
        rc, _, err = cli(*args)
        assert rc == 2 and "/priority: property is not allowed" in err, (args, err)
    assert cli("set", "items", "D01", "--json", json.dumps(item("ok")))[0] == 0
    rc, _, err = cli("update", "items", "D01", "--json", '{"priority": "high"}')
    assert rc == 2 and "priority" in err
    ops = tmp_path / "ops.json"
    ops.write_text(json.dumps([{"action": "update", "collection": "items", "id": "D01", "data": {"priority": 1}}]))
    rc, _, err = cli("batch", str(ops))
    assert rc == 2 and "priority" in err
    assert "priority" not in ds.Store(cli.root).get("items", "D01")["data"]


def test_a_stored_legacy_field_still_reads_and_takes_a_valid_update(cli):
    ds.Store(cli.root).set("items", "D01", item("legacy", priority="high", recommendation="x"), if_version=0)
    rc, out, _ = cli("get", "items", "D01")
    assert rc == 0 and json.loads(out)["data"]["priority"] == "high"
    assert [d["id"] for d in json.loads(cli("list", "items")[1])] == ["D01"]
    rc, _, err = cli("update", "items", "D01", "--json", '{"status": "done"}')
    assert rc == 0, err

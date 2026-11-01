"""Blocker prefix B (the owner 2026-10-30, Dd1; plan MB0bda step 1): alias rule A->B, A refused for new ids, page kind/regex, media never a blocker.

Run: python3 -m pytest tests/test_dashboard_blocker_prefix.py
Test: tests/test_dashboard_blocker_prefix.py
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import dashboard_derive as derive  # noqa: E402
import dashboard_store as ds  # noqa: E402

spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)
PAGE = os.path.join(ROOT, "templates/dashboard/index.html")


def put(root, coll, doc_id, **data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def seed(root, rule=True):
    put(root, "meta", "state", session_id="S14")
    put(root, "items", "B3e", kind="action", status="open", title="moved blocker")
    put(root, "step_progress", "B3e", done=[1])
    put(root, "media", "B0bbb", kind="image")
    if rule:
        put(root, "meta", "id-aliases", rules=[{"from": "A", "to": "B", "cutover": "2026-10-30T12:00:00Z"}])


def run(root, *argv):
    """Result of dashboard-db run(); store errors propagate."""
    return db.run(db.build_parser().parse_args(["--root", str(root), *argv]))


def test_old_a_id_resolves_to_b_with_alias_of(tmp_path):
    seed(tmp_path)
    for ref in ("A3e", "S14:A3e"):
        got = run(tmp_path, "get", "items", ref)
        assert got["id"] == "B3e" and got["alias_of"] == "A3e"
    assert "alias_of" not in run(tmp_path, "get", "items", "B3e")
    assert run(tmp_path, "get", "step_progress", "A3e")["id"] == "B3e"   # step_progress follows the item


def test_no_rule_no_alias(tmp_path):
    seed(tmp_path, rule=False)
    with pytest.raises(ds.NotFound):
        run(tmp_path, "get", "items", "A3e")


def test_new_a_ids_refused_in_next_create_and_set(tmp_path):
    seed(tmp_path)
    for argv in (("next", "A"), ("create", "items", "A", "--json", '{"kind":"action","status":"open"}'),
                 ("set", "items", "A01", "--json", '{"kind":"action","status":"open"}')):
        with pytest.raises(ds.StoreError, match="retired"):
            run(tmp_path, *argv)
    assert not (tmp_path / "items" / "A01.json").exists()


def test_create_b_takes_next_hex_and_ignores_media(tmp_path):
    seed(tmp_path)
    snap = derive.snapshot(ds.Store(str(tmp_path)), derive.ID_COLLECTIONS)
    assert derive.used_ids(snap, "B") == {0x3e}   # 5-char media id B0bbb is not a blocker
    assert run(tmp_path, "next", "B") == "B3f"
    made = run(tmp_path, "create", "items", "B", "--json", '{"kind":"action","status":"open","title":"x"}')
    assert made["id"] == "B3f"


def test_b_action_counts_in_tally(tmp_path):
    seed(tmp_path)
    t = derive.tally(ds.Store(str(tmp_path)).list_docs("items"))
    assert (t["open"], t["actions"]) == (1, 1)


@pytest.mark.skipif(not shutil.which("node"), reason="node not installed")
def test_page_maps_b_to_blocker_bucket_and_links_old_a_ref():
    src = open(PAGE, encoding="utf-8").read()
    ask = re.search(r"/\* BEGIN askKind \*/(.*?)/\* END askKind \*/", src, re.S).group(1)
    refm = re.search(r"/\* BEGIN refMatches \*/(.*?)/\* END refMatches \*/", src, re.S).group(1)
    rx = re.search(r'refRe = new RegExp\((.*?)\);', src).group(1)
    assert "B" in re.search(r"\[(G[A-Z]+R)\]", rx).group(1) or "GPABDTSR" in rx
    js = ask + refm + """
const re = /(?<![\\w-])()([GPABDTSR][0-9a-f]{2})(?![\\w-])/g;
const ids = new Set(["B3e"]);
console.log(JSON.stringify([askKind({id:"B3e",kind:"action"}), askKind({id:"A3e",kind:"action"}), askKind({id:"D01"}),
  refMatches("see A3e and B3e and B0bbb", re, new Map(), ids).map((m) => m.target)]));"""
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout) == ["A", "A", "D", ["B3e", "B3e"]]

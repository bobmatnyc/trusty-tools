"""Fleet-wide ids (ar-T44): qualified "<S id>:<id>" refs, set-refuses-existing, next/create, nav only in architect mode.

Run: python3 -m pytest scripts/test_dashboard_qualified.py
Test: scripts/test_dashboard_qualified.py
"""
import importlib.util
import json
import os
import sys
import tempfile
import threading
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
import owner_answers as oa  # noqa: E402
from test_dashboard_store import serve  # noqa: E402

spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)


def put(root, coll, doc_id, **data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def seed(root, sid="S14"):
    put(root, "meta", "state", session_id=sid)   # a tmp store is not the default root, so it names its owner here
    put(root, "items", "D01", kind="question", status="open", session="tm-apex")
    put(root, "items", "D02", kind="question", status="answered", answer="y", answered_at="2026-10-20T00:00:00Z", session="tm-architect")


def run(root, *argv):
    return db.main(["--root", str(root), *argv])


def test_get_accepts_qualified_and_bare_and_adds_qualifier(tmp_path, capsys):
    seed(tmp_path)
    assert run(tmp_path, "get", "items", "S14:D01") == 0
    out = json.loads(capsys.readouterr().out)
    assert out["id"] == "D01" and out["qualified"] == "S14:D01"   # item session is tm-apex; the store is S14's
    assert run(tmp_path, "get", "items", "D01") == 0
    capsys.readouterr()
    assert run(tmp_path, "get", "items", "S0c:D01") == 1   # another store's S id
    assert "use S14:D01" in capsys.readouterr().err


def test_update_and_delete_accept_qualified(tmp_path, capsys):
    seed(tmp_path)
    assert run(tmp_path, "update", "items", "S14:D01", "--json", '{"detail": "x"}') == 0
    assert json.loads((tmp_path / "items" / "D01.json").read_text())["detail"] == "x"
    assert run(tmp_path, "update", "items", "S0c:D01", "--json", '{"detail": "z"}') == 1


def test_set_refuses_existing_without_if_version(tmp_path, capsys):
    seed(tmp_path)
    before = (tmp_path / "items" / "D01.json").read_text()
    assert run(tmp_path, "set", "items", "D01", "--json", '{"kind": "question", "status": "open"}') == 3
    assert (tmp_path / "items" / "D01.json").read_text() == before
    assert run(tmp_path, "set", "items", "D01", "--if-version", "1", "--json", '{"kind": "question", "status": "open"}') == 0
    assert run(tmp_path, "set", "items", "D77", "--json", '{"kind": "question", "status": "open"}') == 0   # new id is fine
    assert run(tmp_path, "set", "items", "S14:D78", "--json", '{"kind": "question"}') == 2


def test_batch_set_refuses_existing_without_if_version(tmp_path):
    seed(tmp_path)
    ops = json.dumps([{"op": "set", "collection": "items", "id": "D02", "data": {"kind": "question", "status": "open"}}])
    assert run(tmp_path, "batch", _write(tmp_path, ops)) == 3


def _write(tmp_path, text):
    p = tmp_path / "ops.json"
    p.write_text(text)
    return str(p)


def test_next_and_create_pick_a_free_id(tmp_path, capsys):
    seed(tmp_path)
    assert run(tmp_path, "next", "D") == 0
    assert capsys.readouterr().out.strip().strip('"') == "D03"
    assert run(tmp_path, "create", "items", "D", "--json", '{"kind": "question", "status": "open"}') == 0
    assert json.loads(capsys.readouterr().out)["id"] == "D03"
    assert (tmp_path / "items" / "D03.json").exists()


def test_owner_answers_resolve_and_qualify(tmp_path):
    seed(tmp_path)
    assert oa.resolve_id(str(tmp_path), "S14:D02") == "D02"
    assert oa.resolve_id(str(tmp_path), "S14:D01") == "D01"
    assert oa.resolve_id(str(tmp_path), "D02") == "D02"
    try:
        oa.resolve_id(str(tmp_path), "S0c:D02")
        raise AssertionError("expected NotFound")
    except ds.NotFound:
        pass
    got = oa.qualified(str(tmp_path), oa.pending(str(tmp_path)))
    assert [e["qid"] for e in got] == ["S14:D02"]


def _nav(cfg, nav_file):
    store = ds.Store(tempfile.mkdtemp())
    srv = serve.DashboardServer(0, store, serve.PAGE, config=cfg, nav_file=nav_file)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/config.json", timeout=5) as r:
            return json.load(r)["nav"]
    finally:
        srv.shutdown()
        srv.server_close()


def test_nav_only_in_architect_mode(tmp_path):
    nav_file = tmp_path / "nav.json"
    nav_file.write_text(json.dumps([{"name": "Architect", "port": 7890}, {"name": "tm-apex", "port": 7891}]))
    assert len(_nav(serve.ARCHITECT_CONFIG, str(nav_file))) == 2
    assert _nav(serve.session_config("t", ""), str(nav_file)) == []


def test_session_store_uses_its_own_s_id(tmp_path, capsys):
    seed(tmp_path, sid="S0c")
    assert run(tmp_path, "get", "items", "S0c:D02") == 0
    assert json.loads(capsys.readouterr().out)["qualified"] == "S0c:D02"
    assert run(tmp_path, "get", "items", "S14:D02") == 1


def test_default_root_is_s14(monkeypatch, tmp_path):
    monkeypatch.setattr(ds, "DEFAULT_ROOT", str(tmp_path))
    import dashboard_qual as dq
    assert dq.store_sid(ds.Store(str(tmp_path))) == "S14"

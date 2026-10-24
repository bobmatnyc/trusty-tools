"""JSONL encoding and the jsonl backend of dashboard_store: determinism, writes, change log, guards, golden bytes.

Why: owner (2026-10-11 04:12Z): deterministic data in jsonl with a schema. Golden files pin the bytes so the Rust
trusty-architect store can be checked against them (scripts/fixtures/dashboard/golden/, SHA256SUMS).
Test: scripts/test_dashboard_jsonl.py
"""
import glob
import hashlib
import json
import os
import shutil
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_jsonl as dj  # noqa: E402
import dashboard_store as ds  # noqa: E402

FIX = os.path.join(HERE, "fixtures", "dashboard")
GOLDEN = os.path.join(FIX, "golden")
T = "2026-10-11T04:00:00Z"
ITEM = {"kind": "question", "status": "open", "title": "t", "session": "s", "goal": "G01", "rank": 1, "created": T, "updated": T}
CLOCK = lambda: "2026-10-11T05:00:00Z"  # noqa: E731


def store(tmp_path, **kw):
    return ds.Store(str(tmp_path / "oq"), fmt="jsonl", clock=CLOCK, **kw)


def test_canonical_bytes_are_key_order_and_whitespace_independent():
    a = dj.render_line("A01", 3, {"b": [1, {"z": None, "a": "é"}], "a": True})
    b = dj.render_line("A01", 3, {"a": True, "b": [1, {"a": "é", "z": None}]})
    assert a == b == '{"data":{"a":true,"b":[1,{"a":"é","z":null}]},"id":"A01","sv":1,"version":3}\n'.encode()
    with pytest.raises(dj.JsonlError):
        dj.render_line("A01", 1, {"x": float("nan")})


def test_lines_sort_by_utf8_bytes_and_round_trip():
    docs = {i: (1, {"n": i}) for i in ["b", "B", "a01", "A1", "A02", "_x"]}
    blob = dj.render_file(docs)
    assert [json.loads(l)["id"] for l in blob.splitlines()] == ["A02", "A1", "B", "_x", "a01", "b"]
    assert dj.render_file(dj.parse_file(blob)) == blob


def test_sv_newer_than_reader_is_refused():
    with pytest.raises(dj.JsonlError):
        dj.parse_file(b'{"data":{},"id":"a","sv":2,"version":1}\n')


def test_set_update_delete_conflict_and_file_bytes(tmp_path):
    s = store(tmp_path)
    assert s.set("items", "D02", ITEM, if_version=0)["version"] == 1
    s.set("items", "D01", ITEM)
    assert s.update("items", "D01", {"title": "u"}, if_version=1)["version"] == 2
    with pytest.raises(ds.Conflict):
        s.update("items", "D01", {"title": "v"}, if_version=1)
    with pytest.raises(ds.Conflict):
        s.set("items", "D01", ITEM, if_version=0)
    got = s.get("items", "D01")
    assert got["version"] == 2 and got["data"]["title"] == "u"
    lines = open(tmp_path / "oq" / "items.jsonl", "rb").read().splitlines()
    assert [json.loads(l)["id"] for l in lines] == ["D01", "D02"]
    s.delete("items", "D02")
    with pytest.raises(ds.NotFound):
        s.get("items", "D02")
    s.delete("items", "D01")
    assert not os.path.exists(tmp_path / "oq" / "items.jsonl")


def test_same_documents_same_bytes_whatever_the_write_order(tmp_path):
    a, b = ds.Store(str(tmp_path / "a"), fmt="jsonl", clock=CLOCK), ds.Store(str(tmp_path / "b"), fmt="jsonl", clock=CLOCK)
    a.batch([{"op": "set", "collection": "items", "id": i, "data": {**ITEM, "title": i}} for i in ("D03", "D01", "D02")])
    for i in ("D02", "D03", "D01"):
        b.set("items", i, {**ITEM, "title": i})
    assert open(tmp_path / "a" / "items.jsonl", "rb").read() == open(tmp_path / "b" / "items.jsonl", "rb").read()


def test_batch_is_all_or_nothing_across_collections(tmp_path, monkeypatch):
    s = store(tmp_path)
    s.set("items", "D01", ITEM)
    before = open(tmp_path / "oq" / "items.jsonl", "rb").read()
    real = ds._atomic_write
    calls = []

    def boom(path, payload):
        calls.append(path)
        if path.endswith("zz.jsonl"):
            raise OSError("disk full")
        real(path, payload)
    monkeypatch.setattr(ds, "_atomic_write", boom)
    with pytest.raises(OSError):
        s.batch([{"op": "update", "collection": "items", "id": "D01", "data": {"title": "new"}},
                 {"op": "set", "collection": "zz", "id": "x", "data": {"a": 1}}])
    monkeypatch.setattr(ds, "_atomic_write", real)
    assert open(tmp_path / "oq" / "items.jsonl", "rb").read() == before
    assert not os.path.exists(tmp_path / "oq" / "zz.jsonl")


def test_change_log_records_actor_keys_hash_and_fsck(tmp_path):
    s = store(tmp_path, actor="page")
    s.set("items", "D01", ITEM)
    s.update("items", "D01", {"title": "u"}, actor="architect")
    s.set("goals", "G01", {"kind": "directed", "rank": 1, "title": "g", "lifecycle": "active", "updated": T})
    ch = dj.read_changes(str(tmp_path / "oq"))
    assert [(c["seq"], c["actor"], c["op"], c["coll"], c["id"], c["version"]) for c in ch] == [
        (1, "page", "set", "items", "D01", 1), (2, "architect", "update", "items", "D01", 2), (3, "page", "set", "goals", "G01", 1)]
    assert ch[1]["keys"] == ["title"] and ch[2]["ts"] == "2026-10-11T05:00:00Z"
    assert dj.fsck(str(tmp_path / "oq")) == []
    path = tmp_path / "oq" / "items.jsonl"
    path.write_bytes(path.read_bytes().replace(b'"u"', b'"x"'))
    probs = dj.fsck(str(tmp_path / "oq"))
    assert any("items/D01" in p and "hash" in p for p in probs)


def test_fsck_flags_noncanonical_bytes(tmp_path):
    root = tmp_path / "oq"
    root.mkdir()
    (root / "items.jsonl").write_text('{"id": "a", "data": {}, "sv": 1, "version": 1}\n')
    assert any("not canonical" in p for p in dj.fsck(str(root)))


def test_change_log_rotates_monthly(tmp_path):
    root = str(tmp_path)
    dj.append_changes(root, [{"op": "set", "coll": "c", "id": "a"}], "2026-09-30T23:00:00Z")
    dj.append_changes(root, [{"op": "set", "coll": "c", "id": "b"}], "2026-10-01T00:00:01Z")
    assert os.path.exists(tmp_path / "changes-2026-09.jsonl")
    assert [c["seq"] for c in dj.read_changes(root)] == [2]


# #10225: a torn or oversized tail line once made every later write skip the audit log.
OVERSIZED = (dj.canonical({"actor": "x", "coll": "c", "id": "big", "keys": ["k" * 20000], "seq": 7, "ts": T}) + "\n").encode()


@pytest.mark.parametrize("tail,seq,bad", [
    (b'{"seq":9,"ts":"2026-10-1', 2, True),        # torn: no LF, invalid JSON
    (b"garbage\n", 2, True),                        # terminated, invalid JSON
    (OVERSIZED, 8, False),                          # valid, longer than any fixed tail window
], ids=["torn", "garbage", "oversized"])
def test_corrupt_change_log_tail_does_not_block_later_appends(tmp_path, capsys, tail, seq, bad):
    s = store(tmp_path)
    s.set("items", "D01", ITEM)
    root = str(tmp_path / "oq")
    with open(os.path.join(root, dj.CHANGES), "ab") as f:
        f.write(tail)
    s.update("items", "D01", {"title": "after"})
    err = capsys.readouterr().err
    assert "change log not written" not in err
    raw = open(os.path.join(root, dj.CHANGES), "rb").read().split(b"\n")
    entry = json.loads(raw[-2])
    assert raw[-1] == b"" and (entry["op"], entry["id"], entry["seq"]) == ("update", "D01", seq)
    # Explicit, never silent: the corrupt line is warned about, refused by read_changes, and reported by fsck.
    assert ("corrupt line" in err) == bad
    probs = dj.fsck(root)
    assert any("changes.jsonl" in p and "invalid" in p for p in probs) == bad
    if bad:
        with pytest.raises(dj.JsonlError):
            dj.read_changes(root)
    else:
        assert probs == [] and [c["seq"] for c in dj.read_changes(root)] == [1, 7, 8]


def test_schema_warn_accepts_and_logs_enforce_refuses(tmp_path, capsys):
    s = store(tmp_path, schema_mode="warn")
    s.set("items", "D01", {**ITEM, "goal": "Goal 3 · Acme"})
    assert "schema warning: items/D01: /goal" in capsys.readouterr().err
    assert "warnings" in dj.read_changes(str(tmp_path / "oq"))[0]
    e = ds.Store(str(tmp_path / "e"), fmt="jsonl", schema_mode="enforce")
    with pytest.raises(ds.SchemaInvalid) as ei:
        e.set("items", "D01", {**ITEM, "status": "maybe"})
    assert ei.value.status == 400 and ei.value.code == "schema_invalid" and ei.value.errors
    assert not os.path.exists(tmp_path / "e" / "items.jsonl")


def test_default_schema_mode_follows_format(tmp_path):
    assert ds.Store(str(tmp_path), fmt="jsonl").schema_mode == "warn"
    assert ds.Store(str(tmp_path), fmt="dir").schema_mode == "off"


def test_format_mismatch_fails_loudly_both_ways(tmp_path):
    root = tmp_path / "oq"
    store(tmp_path).set("items", "D01", ITEM)
    with pytest.raises(ds.StoreError, match="DASHBOARD_FORMAT=jsonl"):
        ds.Store(str(root), fmt="dir").list_docs("items")
    legacy = tmp_path / "old"
    ds.Store(str(legacy), fmt="dir").set("items", "D01", ITEM)
    with pytest.raises(ds.StoreError, match="dashboard-migrate"):
        ds.Store(str(legacy), fmt="jsonl").list_docs("items")
    with pytest.raises(ds.StoreError, match="reserved"):
        store(tmp_path).set("changes", "x", {})


def test_read_cache_sees_an_external_rewrite(tmp_path):
    s, other = store(tmp_path), store(tmp_path)
    s.set("items", "D01", ITEM)
    assert s.get("items", "D01")["version"] == 1
    other.update("items", "D01", {"title": "changed"})
    assert s.get("items", "D01")["data"]["title"] == "changed"


def test_returned_documents_are_copies(tmp_path):
    s = store(tmp_path)
    s.set("items", "D01", {**ITEM, "options": [{"label": "a", "desc": "b"}]})
    s.get("items", "D01")["data"]["options"].clear()
    assert s.get("items", "D01")["data"]["options"]


def test_golden_files_match_their_sha256_list(tmp_path):
    """The sha256 list is what the Rust crate checks against."""
    out = {os.path.basename(f)[:-6]: open(f, "rb").read() for f in sorted(glob.glob(os.path.join(GOLDEN, "*.jsonl")))}
    sums = "".join(f"{hashlib.sha256(out[c]).hexdigest()}  {c}.jsonl\n" for c in sorted(out))
    assert sums == open(os.path.join(GOLDEN, "SHA256SUMS")).read()
    # the golden bytes are canonical and read back through the Store unchanged
    shutil.copytree(GOLDEN, tmp_path / "g", ignore=shutil.ignore_patterns("SHA256SUMS", "*.json"))
    st = ds.Store(str(tmp_path / "g"), fmt="jsonl", schema_mode="off")
    assert len(st.list_docs("items")) == 8 and dj.fsck(str(tmp_path / "g")) == []


def test_http_api_over_jsonl_store_sends_actor_and_maps_schema_invalid(tmp_path):
    import importlib.util
    import threading
    import urllib.error
    import urllib.request
    spec = importlib.util.spec_from_file_location("dashboard_serve", os.path.join(SCRIPTS, "dashboard-serve.py"))
    serve = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(serve)
    page = tmp_path / "index.html"
    page.write_text("<html><head></head><body></body></html>")
    st = ds.Store(str(tmp_path / "oq"), fmt="jsonl", schema_mode="enforce", clock=CLOCK)
    srv = serve.DashboardServer(0, st, str(page))
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    def call(method, path, body, headers=None):
        req = urllib.request.Request(f"http://127.0.0.1:{srv.server_address[1]}{path}", data=json.dumps(body).encode(), method=method,
                                     headers={"Content-Type": "application/json", **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())
    try:
        assert call("PUT", "/api/items/D01", {"data": ITEM, "if_version": 0})[0] == 200
        assert call("PATCH", "/api/items/D01", {"data": {"title": "z"}}, {"X-Actor": "architect"})[0] == 200
        code, body = call("PATCH", "/api/items/D01", {"data": {"status": "maybe"}})
        assert code == 400 and "schema_invalid" in json.dumps(body)
    finally:
        srv.shutdown()
        srv.server_close()
    assert [c["actor"] for c in dj.read_changes(str(tmp_path / "oq"))] == ["page", "architect"]

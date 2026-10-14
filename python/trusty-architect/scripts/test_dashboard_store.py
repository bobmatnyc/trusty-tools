"""dashboard_store, dashboard-serve.py and dashboard-db.py on temp directories.

Covers: version conflict, all-or-nothing batch (staging failure and mid-write failure),
the old mirror shape (no version), the HTTP shim endpoints round trip, page injection,
Host guard, 127.0.0.1 bind, busy-port refusal, seed of meta/state, and the CLI exit codes.
Run: python3 -m unittest scripts/test_dashboard_store.py
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402


def load(name, fname):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, fname))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


serve = load("dashboard_serve", "dashboard-serve.py")


class StoreCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = os.path.join(self._tmp.name, "oq")
        self.store = ds.Store(self.root)

    def read_file(self, coll, doc_id):
        with open(os.path.join(self.root, coll, doc_id + ".json")) as f:
            return json.load(f)


class TestVersions(StoreCase):
    def test_set_update_bump_version_and_stale_if_version_conflicts(self):
        self.assertEqual(self.store.set("items", "D01", {"status": "open"})["version"], 1)
        self.assertEqual(self.store.update("items", "D01", {"answer": "x"}, if_version=1)["version"], 2)
        with self.assertRaises(ds.Conflict):
            self.store.update("items", "D01", {"answer": "y"}, if_version=1)
        got = self.store.get("items", "D01")
        self.assertEqual((got["version"], got["data"]), (2, {"status": "open", "answer": "x"}))

    def test_if_version_zero_means_create_only(self):
        self.store.set("items", "D01", {"a": 1}, if_version=0)
        with self.assertRaises(ds.Conflict):
            self.store.set("items", "D01", {"a": 2}, if_version=0)

    def test_old_mirror_file_without_version_reads_as_v1_and_keeps_fields(self):
        os.makedirs(os.path.join(self.root, "items"))
        with open(os.path.join(self.root, "items", "D1d.json"), "w") as f:
            json.dump({"status": "done", "title": "HOST-B"}, f)
        self.assertEqual(self.store.get("items", "D1d")["version"], 1)
        self.assertEqual(self.store.update("items", "D1d", {"outcome": "ok"})["version"], 2)
        self.assertEqual(self.read_file("items", "D1d"), {"status": "done", "title": "HOST-B", "outcome": "ok", "version": 2})

    def test_bad_ids_and_reserved_version_key_are_rejected(self):
        for bad in ("../x", "a/b", ".hidden", ""):
            with self.assertRaises(ds.StoreError):
                self.store.set("items", bad, {})
        with self.assertRaises(ds.StoreError):
            self.store.set("items", "D01", {"version": 9})
        with self.assertRaises(ds.NotFound):
            self.store.update("items", "nope", {"a": 1})

    def test_list_filters_status_and_orders_with_limit(self):
        for i, (st, at) in enumerate([("new", "2"), ("applied", "3"), ("new", "1")]):
            self.store.set("goal_reorders", f"r{i}", {"status": st, "at": at})
        self.assertEqual([d["id"] for d in self.store.list_docs("goal_reorders", status="new")], ["r0", "r2"])
        top = self.store.list_docs("goal_reorders", order_by="at", desc=True, limit=2)
        self.assertEqual([d["id"] for d in top], ["r1", "r0"])
        self.assertEqual(self.store.list_docs("nothing"), [])


class TestBatch(StoreCase):
    def test_conflict_in_second_op_writes_nothing(self):
        self.store.set("items", "A", {"n": 1})
        ops = [{"op": "update", "collection": "items", "id": "A", "data": {"n": 2}},
               {"op": "set", "collection": "items", "id": "B", "data": {"n": 1}, "if_version": 5}]
        with self.assertRaises(ds.Conflict):
            self.store.batch(ops)
        self.assertEqual(self.store.get("items", "A")["data"], {"n": 1})
        self.assertFalse(os.path.exists(os.path.join(self.root, "items", "B.json")))

    def test_write_failure_midway_restores_previous_files(self):
        self.store.set("items", "A", {"n": 1})
        self.store.set("items", "B", {"n": 1})
        ops = [{"op": "update", "collection": "items", "id": "A", "data": {"n": 2}},
               {"op": "delete", "collection": "items", "id": "B"},
               {"op": "set", "collection": "items", "id": "C", "data": {"n": 3}}]
        real, calls = os.replace, {"n": 0}

        def flaky(src, dst):
            calls["n"] += 1
            if calls["n"] == 2:        # A replaced, B unlinked, C's replace fails
                raise OSError("disk full")
            return real(src, dst)

        with mock.patch.object(ds.os, "replace", flaky):
            with self.assertRaises(OSError):
                self.store.batch(ops)
        self.assertEqual(self.store.get("items", "A")["data"], {"n": 1})
        self.assertEqual(self.store.get("items", "B")["version"], 1)
        self.assertFalse(os.path.exists(os.path.join(self.root, "items", "C.json")))
        leftovers = [n for n in os.listdir(os.path.join(self.root, "items")) if n.endswith(".tmp")]
        self.assertEqual(leftovers, [])

    def test_batch_applies_all_and_chains_same_doc(self):
        res = self.store.batch([{"op": "set", "collection": "sessions", "id": "S1", "data": {"a": 1}},
                                {"op": "update", "collection": "sessions", "id": "S1", "data": {"b": 2}, "if_version": 1}])
        self.assertEqual([r["version"] for r in res], [1, 2])
        self.assertEqual(self.store.get("sessions", "S1")["data"], {"a": 1, "b": 2})


class TestSeed(StoreCase):
    def test_seed_meta_seeds_owner_name_once_and_no_next_ids(self):
        md = os.path.join(self._tmp.name, "state.md")
        with open(md, "w") as f:
            f.write("- x NEXT IDS G0b P06 A0c D1f T03 S14 (meta v32)\n- old NEXT IDS G01 P01\n")
        self.assertTrue(self.store.seed_meta(md))
        self.assertFalse(self.store.seed_meta(md))
        data = self.store.get("meta", "state")["data"]
        self.assertEqual(data["owner_name"], "Owner")
        self.assertNotIn("next_ids", data)   # next ids are computed from the store (dashboard_ids.py, the page)


class ServerCase(StoreCase):
    def setUp(self):
        super().setUp()
        self.page = os.path.join(self._tmp.name, "index.html")
        with open(self.page, "w") as f:
            f.write("<!doctype html><html><head><title>t</title></head><body><script>/*PAGE*/</script></body></html>")
        self.store.set("items", "D01", {"status": "open", "title": "Q"})
        self.store.seed_meta(os.path.join(self._tmp.name, "none.md"))
        self.srv = serve.DashboardServer(0, self.store, self.page)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def call(self, method, path, body=None, headers=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=data, method=method,
                                     headers={"Content-Type": "application/json", **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()


class TestServer(ServerCase):
    def test_binds_loopback_only(self):
        self.assertEqual(self.srv.server_address[0], "127.0.0.1")

    def test_busy_port_is_detected_and_main_refuses(self):
        self.assertTrue(serve.port_in_use(self.port))
        with mock.patch.object(sys, "stderr") as err:
            rc = serve.main(["--port", str(self.port), "--root", self.root, "--page", self.page])
        self.assertEqual(rc, 1)
        self.assertIn("already in use", "".join(c.args[0] for c in err.write.call_args_list))

    def test_page_is_served_with_shim_before_page_script(self):
        status, body = self.call("GET", "/")
        text = body.decode()
        self.assertEqual(status, 200)
        self.assertIn("window.claude = { use:", text)
        self.assertLess(text.index("window.claude = { use:"), text.index("/*PAGE*/"))
        with open(self.page) as f:
            self.assertEqual(f.read().count("window.claude"), 0)   # file on disk is untouched

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_shim_first_load_runs_in_hidden_tab_and_later_polls_continue(self):
        # Runs the served shim in node with a hidden document and a stub fetch.
        js = r"""
const fs = require("fs");
const listeners = {}; let calls = 0;
global.window = {};
global.document = { hidden: true, addEventListener: (n, f) => { listeners[n] = f; }, removeEventListener() {} };
global.fetch = async () => { calls++; return { ok: true, status: 200, json: async () => ({ rev: "r1", docs: [] }) }; };
eval(fs.readFileSync(process.argv[1], "utf8"));
(async () => {
  const db = await window.claude.use("db");
  let snaps = 0;
  const off = db.collection("items").onSnapshot(() => { snaps++; });
  await new Promise(r => setTimeout(r, 50));
  const first = [calls, snaps];
  await listeners.visibilitychange();            // still hidden: still fetches, so a new decision can beep
  await new Promise(r => setTimeout(r, 50));
  const hiddenPoll = calls;
  document.hidden = false;
  await listeners.visibilitychange();            // visible again: polls
  await new Promise(r => setTimeout(r, 50));
  off();
  console.log(JSON.stringify({ first, hiddenPoll, visiblePoll: calls }));
  process.exit(0);
})();
"""
        shim = serve.SHIM.replace("<script>", "").replace("</script>", "")
        shim_path = os.path.join(self._tmp.name, "shim.js")
        with open(shim_path, "w") as f:
            f.write(shim)
        out = subprocess.run(["node", "-e", js, shim_path], capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 0, out.stderr)
        r = json.loads(out.stdout)
        self.assertEqual(r["first"], [1, 1])      # first load ran although document.hidden
        self.assertEqual(r["hiddenPoll"], 2)      # later poll runs while hidden
        self.assertEqual(r["visiblePoll"], 3)

    def test_round_trip_list_get_patch_conflict_add_delete_batch_user(self):
        status, body = self.call("GET", "/api/items?status=open")
        docs = json.loads(body)
        self.assertEqual((status, [d["id"] for d in docs["docs"]]), (200, ["D01"]))
        s, b = self.call("PATCH", "/api/items/D01", {"data": {"status": "answered"}, "if_version": 1})
        self.assertEqual((s, json.loads(b)["version"]), (200, 2))
        s, b = self.call("PATCH", "/api/items/D01", {"data": {"status": "x"}, "if_version": 1})
        self.assertEqual((s, json.loads(b)["code"]), (409, "conflict"))
        s, b = self.call("POST", "/api/goal_removals", {"data": {"goal": "G01", "status": "new"}})
        new_id = json.loads(b)["id"]
        self.assertEqual(s, 201)
        self.assertEqual(json.loads(self.call("GET", f"/api/goal_removals/{new_id}")[1])["data"]["goal"], "G01")
        s, b = self.call("POST", "/api/batch", {"ops": [{"op": "set", "collection": "sessions", "id": "S01", "data": {"name": "x"}},
                                                         {"op": "delete", "collection": "items", "id": "missing"}]})
        self.assertEqual(s, 404)
        self.assertEqual(self.call("GET", "/api/sessions/S01")[0], 404)       # batch rolled back
        self.assertEqual(self.call("DELETE", "/api/items/D01?if_version=2")[0], 200)
        self.assertEqual(json.loads(self.call("GET", "/api/user")[1])["name"], "Owner")
        self.assertEqual(json.loads(self.call("GET", "/api/meta/state")[1])["data"]["owner_name"], "Owner")

    def test_rev_changes_only_when_a_document_changes(self):
        r1 = json.loads(self.call("GET", "/api/items")[1])["rev"]
        self.assertEqual(r1, json.loads(self.call("GET", "/api/items")[1])["rev"])
        self.store.update("items", "D01", {"x": 1})
        self.assertNotEqual(r1, json.loads(self.call("GET", "/api/items")[1])["rev"])

    def test_foreign_host_and_cross_origin_write_are_refused(self):
        self.assertEqual(self.call("GET", "/api/items", headers={"Host": "evil.example:80"})[0], 403)
        s, _ = self.call("PATCH", "/api/items/D01", {"data": {"a": 1}}, headers={"Origin": "http://evil.example"})
        self.assertEqual(s, 403)
        self.assertEqual(self.store.get("items", "D01")["version"], 1)


class TestCli(StoreCase):
    def run_cli(self, *args):
        p = subprocess.run([sys.executable, os.path.join(HERE, "dashboard-db.py"), "--root", self.root, *args],
                           capture_output=True, text=True)
        return p.returncode, p.stdout, p.stderr

    def test_set_get_list_conflict_exit_codes_and_batch(self):
        rc, out, _ = self.run_cli("set", "items", "D01", "--json", '{"status":"open"}')
        self.assertEqual((rc, json.loads(out)["version"]), (0, 1))
        rc, out, _ = self.run_cli("list", "items", "--status", "open")
        self.assertEqual([d["id"] for d in json.loads(out)], ["D01"])
        self.assertEqual(self.run_cli("update", "items", "D01", "--json", '{"a":1}', "--if-version", "9")[0], 3)
        self.assertEqual(self.run_cli("get", "items", "nope")[0], 1)
        self.assertEqual(self.run_cli("set", "items", "../x", "--json", "{}")[0], 2)
        opsf = os.path.join(self._tmp.name, "ops.json")
        with open(opsf, "w") as f:
            json.dump({"ops": [{"op": "update", "collection": "items", "id": "D01", "data": {"b": 2}},
                               {"op": "delete", "collection": "items", "id": "zzz"}]}, f)
        self.assertEqual(self.run_cli("batch", opsf)[0], 1)
        self.assertEqual(self.store.get("items", "D01")["data"], {"status": "open"})


if __name__ == "__main__":
    unittest.main()


class TestStepValidation(StoreCase):
    """items writes refuse steps without a string title; nothing is written."""

    run_cli = TestCli.run_cli

    def test_bad_steps_rejected_on_set_update_batch(self):
        good = json.dumps({"steps": [{"id": "s1", "title": "Do it"}]})
        self.assertEqual(self.run_cli("set", "items", "D01", "--json", good)[0], 0)
        bad = json.dumps({"steps": [{"id": "s1", "title": "ok"}, {"id": "s2", "text": "oops"}]})
        for args in (("set", "items", "D02", "--json", bad), ("update", "items", "D01", "--json", bad)):
            rc, _, err = self.run_cli(*args)
            self.assertEqual(rc, 2)
            self.assertIn("step 1", err)
            self.assertIn("rename it to 'title'", err)
            self.assertIn("id, title, detail, links, values", err)
        self.assertEqual(self.store.get("items", "D01")["data"], json.loads(good))
        opsf = os.path.join(self._tmp.name, "ops.json")
        with open(opsf, "w") as f:
            json.dump([{"op": "set", "collection": "items", "id": "D03", "data": {"steps": [{"id": "s1", "title": "  "}]}},
                       {"op": "set", "collection": "items", "id": "D04", "data": {}}], f)
        rc, _, err = self.run_cli("batch", opsf)
        self.assertEqual(rc, 2)
        self.assertIn("D03", err)
        self.assertEqual([d["id"] for d in self.store.list_docs("items")], ["D01"])

    def test_other_collections_and_stepless_items_unaffected(self):
        self.assertEqual(self.run_cli("set", "sessions", "S01", "--json", '{"steps":[{"text":"x"}]}')[0], 0)
        self.assertEqual(self.run_cli("set", "items", "D05", "--json", '{"status":"open"}')[0], 0)

    def test_pages_fall_back_to_text_detail_then_marker(self):
        for rel in ("templates/dashboard/index.html",):
            with open(os.path.join(HERE, "..", rel), encoding="utf-8") as f:
                html = f.read()
            self.assertIn("txt(s.title) || txt(s.text) || txt(s.detail)", html, rel)
            self.assertIn("(step text missing)", html, rel)
            self.assertNotIn('"lstep-title", txt(x.title)', html, rel)

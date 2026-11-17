"""Decision items carry a computed project (#11047, D80): tm's session list first, else the session-name rule.

Covers: the name rule (tm-foo-02, tm-foo, tm-foo-bar-12, a non-tm- name, null/blank), the session list winning over the
name rule (project-paths alias by workspace path, else the source_id repo; live records over deleted ones), the fallback
when the list is absent, unreadable, or raises, and GET /api/items: the project reaches the page in data, nothing is
written to the store, an item with no session has no project, and a failing lookup still returns every item.
Test: tests/test_dashboard_item_project.py
"""
import json
import os
import tempfile
import threading
import unittest
import urllib.request
from unittest import mock

import dashboard_store as ds
import dashboard_viz as dv
from test_dashboard_store import serve


class Root(unittest.TestCase):
    """A scratch tm root; TRUSTY_MPM_ROOT points at it (conftest already isolates the live one)."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = tmp.name
        self.web = os.path.join(self.root, "src", "web-app")
        os.makedirs(self.web)
        env = mock.patch.dict(os.environ, {"TRUSTY_MPM_ROOT": self.root})
        env.start()
        self.addCleanup(env.stop)

    def write_sessions(self, recs, raw=None):
        os.makedirs(os.path.join(self.root, "session-manager"), exist_ok=True)
        with open(os.path.join(self.root, "session-manager", "sessions.json"), "w") as f:
            f.write(raw if raw is not None else json.dumps({"sessions": {str(i): r for i, r in enumerate(recs)}}))

    def write_paths(self, rows):
        with open(os.path.join(self.root, "project-paths.json"), "w") as f:
            json.dump(rows, f)


class TestNameRule(unittest.TestCase):
    def test_name_rule(self):
        cases = [("tm-foo-02", "foo"), ("tm-foo", "foo"), ("tm-foo-bar-12", "foo-bar"), ("tm-foo-123", "foo"),
                 ("tm-poc-hotel-supply", "poc-hotel-supply"), ("ops-desk", "ops-desk"), ("architect-3", "architect"),
                 (None, None), ("", None), ("   ", None)]
        for session, want in cases:
            with self.subTest(session=session):
                self.assertEqual(dv.item_project(session, {}), want)


class TestSessionList(Root):
    def test_session_list_wins_over_the_name_rule(self):
        self.write_paths([{"alias": "web", "path": self.web}])
        self.write_sessions([
            {"tmux_name": "tm-legacy-07", "workspace_path": self.web, "state": "active"},          # alias by path
            {"tmux_name": "tm-other-01", "workspace_path": "/nowhere", "source_id": "acme/shop"},  # source_id repo
            {"tmux_name": "tm-bare-02", "workspace_path": "/nowhere"},                             # nothing: name rule
            {"tmux_name": "tm-dup-01", "source_id": "acme/gone", "state": "decommissioned"},
            {"tmux_name": "tm-dup-01", "source_id": "acme/live", "state": "active"},
        ])
        listed = dv.tm_session_projects()
        self.assertEqual(listed, {"tm-legacy-07": "web", "tm-other-01": "shop", "tm-dup-01": "live"})
        self.assertEqual(dv.item_project("tm-legacy-07", listed), "web")
        self.assertEqual(dv.item_project("tm-other-01", listed), "shop")
        self.assertEqual(dv.item_project("tm-bare-02", listed), "bare")
        self.assertEqual(dv.item_project("tm-dup-01", listed), "live")

    def test_absent_or_unreadable_list_falls_back_to_the_name_rule(self):
        self.assertEqual(dv.tm_session_projects(), {})
        for raw in ("{not json", "[1, 2]", '{"sessions": 7}'):
            with self.subTest(raw=raw):
                self.write_sessions(None, raw=raw)
                self.assertEqual(dv.tm_session_projects(), {})
        docs = [{"id": "D01", "version": 1, "data": {"session": "tm-foo-02"}}]
        self.assertEqual(dv.with_projects(docs)[0]["data"]["project"], "foo")

    def test_a_raising_lookup_keeps_every_item(self):
        docs = [{"id": "D01", "version": 1, "data": {"session": "tm-foo-02"}}, {"id": "D02", "version": 1, "data": {"title": "x"}}]
        with mock.patch.object(dv, "tm_session_projects", side_effect=RuntimeError("boom")):
            out = dv.with_projects(docs)
        self.assertEqual([d["id"] for d in out], ["D01", "D02"])
        self.assertEqual(out[0]["data"]["project"], "foo")
        self.assertNotIn("project", out[1]["data"])
        self.assertNotIn("project", docs[0]["data"])   # the input is never mutated


class TestItemsEndpoint(Root):
    def setUp(self):
        super().setUp()
        self.store = ds.Store(os.path.join(self.root, "store"))
        self.store.set("items", "D01", {"status": "open", "kind": "question", "session": "tm-foo-bar-12"})
        self.store.set("items", "D02", {"status": "open", "kind": "question"})
        self.store.set("items", "D03", {"status": "open", "kind": "question", "session": "tm-legacy-07"})
        page = os.path.join(self.root, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        self.srv = serve.DashboardServer(0, self.store, page)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def get(self):
        with urllib.request.urlopen(f"http://127.0.0.1:{self.srv.server_address[1]}/api/items", timeout=5) as r:
            return json.loads(r.read())

    def test_items_carry_the_project_and_the_store_is_untouched(self):
        self.write_sessions([{"tmux_name": "tm-legacy-07", "source_id": "acme/shop"}])
        before = [d["version"] for d in self.store.list_docs("items")]
        body = self.get()
        got = {d["id"]: d["data"].get("project") for d in body["docs"]}
        self.assertEqual(got, {"D01": "foo-bar", "D02": None, "D03": "shop"})
        self.assertNotIn("project", self.store.get("items", "D01")["data"])
        self.assertEqual([d["version"] for d in self.store.list_docs("items")], before)
        # a changed resolution changes the rev, so the page re-renders
        self.write_sessions([{"tmux_name": "tm-legacy-07", "source_id": "acme/store-two"}])
        again = self.get()
        self.assertEqual({d["id"]: d["data"].get("project") for d in again["docs"]}["D03"], "store-two")
        self.assertNotEqual(again["rev"], body["rev"])

    def test_a_failing_lookup_still_returns_every_item(self):
        with mock.patch.object(dv, "tm_session_projects", side_effect=RuntimeError("boom")):
            body = self.get()
        self.assertEqual([d["id"] for d in body["docs"]], ["D01", "D02", "D03"])
        self.assertEqual(body["docs"][2]["data"]["project"], "legacy")


if __name__ == "__main__":
    unittest.main()

"""Media item ids are B + 4 hex digits, backfilled once, and an item can be archived (the owner 2026-10-26).

Why: media rows need a short unique handle, and a finished diagram or image should leave the live list without deletion.
What: id format and uniqueness against live docs; backfill of legacy 2-hex docs is idempotent and keeps fields; archive moves
the file to <docs>/archive/ keeping its name (timestamp suffix on a clash), deletes the doc so the id is free again, and the
row leaves the /media list; POST /api/media-archive/<id> serves it; an unknown id is 404 and a stray id moves nothing.
Test: this file.
"""
import json
import os
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

from test_dashboard_doc import write
from test_dashboard_store import serve
import dashboard_media as dm
import dashboard_store as ds


def denied(segs, name):
    return any(s.startswith(".") for s in segs)


class Base(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.t = os.path.realpath(tmp.name)
        self.proj = os.path.join(self.t, "acme")
        self.png = os.path.join(self.proj, "docs", "img", "shot.png")
        write(self.png, "PNG")
        write(os.path.join(self.proj, "docs", "d", "flow.mmd"), "graph TD; A-->B")
        self.projects = {"acme": self.proj}
        self.store = ds.Store(os.path.join(self.t, "oq"))

    def ids(self):
        rows = dm.list_media(self.projects, ("diagram", "image"), denied)
        return dm.sync_ids(self.store, rows)


class TestBackfill(Base):
    def test_legacy_ids_become_four_hex_once_and_keep_fields(self):
        for i, p in (("B1f", "docs/a.png"), ("B00", "docs/b.png")):
            self.store.set("media", i, {"project": "acme", "path": p, "status": "open", "accessed": "2026-10-25T00:00:00Z"})
        self.assertEqual(dm.backfill_ids(self.store), 2)
        got = {d["id"]: d["data"] for d in self.store.list_docs("media")}
        self.assertEqual(sorted(got), ["B0000", "B001f"])
        self.assertEqual(got["B001f"]["path"], "docs/a.png")
        self.assertEqual(got["B001f"]["accessed"], "2026-10-25T00:00:00Z")
        self.assertEqual(dm.backfill_ids(self.store), 0)   # idempotent
        self.assertEqual(sorted(d["id"] for d in self.store.list_docs("media")), ["B0000", "B001f"])

    def test_sync_runs_backfill_and_new_file_does_not_collide(self):
        self.store.set("media", "B01", {"project": "acme", "path": "docs/img/shot.png", "status": "open"})
        ids = self.ids()
        self.assertEqual(ids[("acme", "docs/img/shot.png")], "B0001")
        self.assertEqual(ids[("acme", "docs/d/flow.mmd")], "B0002")
        self.assertEqual(len(set(ids.values())), 2)


class TestArchive(Base):
    def test_archive_moves_file_frees_id_and_leaves_list(self):
        ids = self.ids()
        mid = ids[("acme", "docs/img/shot.png")]
        out = dm.archive_media(self.store, self.projects, mid, denied)
        self.assertEqual(out["archived_to"], "docs/archive/shot.png")
        self.assertFalse(os.path.exists(self.png))
        self.assertEqual(open(os.path.join(self.proj, "docs", "archive", "shot.png")).read(), "PNG")
        self.assertEqual([d["id"] for d in self.store.list_docs("media") if d["id"] == mid], [])
        paths = [r["path"] for r in dm.list_media(self.projects, ("diagram", "image"), denied)]
        self.assertEqual(paths, ["docs/d/flow.mmd"])
        again = self.ids()
        self.assertNotIn(("acme", "docs/archive/shot.png"), again)
        write(os.path.join(self.proj, "docs", "img", "new.png"), "N")
        self.assertEqual(self.ids()[("acme", "docs/img/new.png")], mid if int(mid[1:], 16) > 0 else "B0001")

    def test_name_clash_gets_timestamp_suffix_and_nothing_is_overwritten(self):
        ids = self.ids()
        dm.archive_media(self.store, self.projects, ids[("acme", "docs/img/shot.png")], denied)
        write(self.png, "PNG2")
        mid = self.ids()[("acme", "docs/img/shot.png")]
        out = dm.archive_media(self.store, self.projects, mid, denied)
        self.assertRegex(out["archived_to"], r"^docs/archive/shot-\d{8}T\d{6}Z\.png$")
        names = sorted(os.listdir(os.path.join(self.proj, "docs", "archive")))
        self.assertEqual(len(names), 2)
        self.assertEqual(open(os.path.join(self.proj, "docs", "archive", "shot.png")).read(), "PNG")

    def test_unknown_id_raises_not_found(self):
        with self.assertRaises(ds.NotFound):
            dm.archive_media(self.store, self.projects, "B9999", denied)

    def test_doc_delete_failure_puts_the_file_back(self):
        mid = self.ids()[("acme", "docs/img/shot.png")]
        real = self.store.delete

        def boom(*a, **k):
            raise ds.Conflict("x")
        self.store.delete = boom
        with self.assertRaises(ds.Conflict):
            dm.archive_media(self.store, self.projects, mid, denied)
        self.store.delete = real
        self.assertTrue(os.path.exists(self.png))
        self.assertTrue(any(d["id"] == mid for d in self.store.list_docs("media")))


class TestEndpoint(Base):
    def server(self, config=None):
        page = os.path.join(self.t, "index.html")
        write(page, "<html></html>")
        kw = {"config": config} if config else {}
        srv = serve.DashboardServer(0, self.store, page, os.path.join(self.t, "b"), projects=self.projects,
                                    vendor=os.path.join(self.t, "v"), **kw)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        return f"http://127.0.0.1:{srv.server_address[1]}"

    def call(self, base, method, path):
        req = urllib.request.Request(base + path, method=method, data=b"{}" if method == "POST" else None,
                                     headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())

    def test_post_archives_and_list_drops_it(self):
        base = self.server()
        _, body = self.call(base, "GET", "/media")
        row = next(r for r in body["media"] if r["path"].endswith("shot.png"))
        self.assertRegex(row["id"], r"^B[0-9a-f]{4}$")
        code, out = self.call(base, "POST", "/api/media-archive/" + row["id"])
        self.assertEqual((code, out["archived_to"]), (200, "docs/archive/shot.png"))
        _, body = self.call(base, "GET", "/media")
        self.assertEqual([r["path"] for r in body["media"]], ["docs/d/flow.mmd"])
        self.assertEqual(self.call(base, "POST", "/api/media-archive/B9999")[0], 404)

    def test_session_mode_refuses(self):
        base = self.server({"mode": "session", "title": "S", "briefs_base": "", "storage_key": "s"})
        self.assertEqual(self.call(base, "POST", "/api/media-archive/B0000")[0], 404)
        self.assertTrue(os.path.exists(self.png))


class TestPage(unittest.TestCase):
    def test_list_rows_with_an_id_get_an_archive_button(self):
        src = open(os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "templates/dashboard/index.html"), encoding="utf-8").read()
        self.assertIn("if (r.id) ac.append(archiveAct(r, draw))", src)
        self.assertIn('"/api/media-archive/" + encodeURIComponent(r.id)', src)


if __name__ == "__main__":
    unittest.main()

"""POST /api/attach/<item id>?name=: owner file uploads stored as Intent Ledger attachments (the owner 2026-10-25).

Why: the owner answers an ask with a file; it must land in the owning project's IL attachments, be recorded on the item,
and never write outside that directory.
What: happy path (project IL dir, record fields, sha256, bytes on disk), unmapped session fallback to the Architect's
local/il, duplicate names kept, traversal / absolute / hidden names refused, symlinked attachment dir refused,
oversize refused (header and cap), bad Origin refused, closed item and unknown item refused, per-item cap, page wiring.
Test: this file.
"""
import hashlib
import json
import os
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest import mock

from test_dashboard_store import serve
import dashboard_attach as da
import dashboard_store as ds

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html")


class TestAttach(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.t = os.path.realpath(tmp.name)
        self.proj = os.path.join(self.t, "acme")
        self.il = os.path.join(self.t, "arch", "local", "il")
        os.makedirs(os.path.join(self.proj, ".trusty-mpm", "il"))
        os.makedirs(self.il)
        sched = os.path.join(self.t, "sched.json")
        with open(sched, "w") as f:
            json.dump({"sessions": [{"name": "tm-acme", "project": self.proj, "code": "ac"}]}, f)
        page = os.path.join(self.t, "index.html")
        with open(page, "w") as f:
            f.write("<html></html>")
        self.store = ds.Store(os.path.join(self.t, "oq"))
        for i, sess in (("D01", "tm-acme"), ("D02", "tm-nobody")):
            self.store.set("items", i, {"kind": "question", "status": "open", "title": "Q", "session": sess})
        self.store.set("items", "D03", {"kind": "question", "status": "done", "title": "Q", "session": "tm-acme"})
        self.srv = serve.DashboardServer(0, self.store, page, il_schedule=sched, architect_il=self.il)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def up(self, item, name, data, headers=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}/api/attach/{item}?name={urllib.request.quote(name)}",
                                     data=data, method="POST", headers={"Content-Type": "application/octet-stream", **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())

    def test_upload_lands_in_project_il_and_is_recorded(self):
        st, body = self.up("D01", "shot 1.png", b"PNGDATA")
        self.assertEqual(st, 201)
        want = os.path.join(self.proj, ".trusty-mpm", "il", "attachments", "D01", "shot 1.png")
        with open(want, "rb") as f:
            self.assertEqual(f.read(), b"PNGDATA")
        rec = self.store.get("items", "D01")["data"]["attachments"][0]
        self.assertEqual((rec["path"], rec["size"], rec["il"], rec["name"]), (want, 7, "project", "shot 1.png"))
        self.assertEqual(rec["sha256"], hashlib.sha256(b"PNGDATA").hexdigest())
        self.assertEqual(body["attachment"], rec)
        self.assertEqual([n for n in os.listdir(os.path.dirname(want)) if n.startswith(".")], [])   # temp file gone

    def test_duplicate_name_is_kept_not_overwritten(self):
        self.up("D01", "a.txt", b"one")
        st, body = self.up("D01", "a.txt", b"two")
        self.assertEqual((st, body["attachment"]["stored_name"]), (201, "a-2.txt"))
        self.assertEqual(len(self.store.get("items", "D01")["data"]["attachments"]), 2)

    def test_unmapped_session_falls_back_to_architect_il(self):
        st, body = self.up("D02", "log.txt", b"x")
        self.assertEqual(st, 201)
        self.assertEqual(body["attachment"]["il"], "architect-fallback")
        self.assertTrue(os.path.isfile(os.path.join(self.il, "attachments", "D02", "log.txt")))

    def test_traversal_and_odd_names_refused(self):
        for bad in ("../x.txt", "a/" + "../" * 2 + "b", "/etc/passwd", "..\\x", "C:\\x", "a\x00b", "..", "", "   ", "a\nb"):
            st, _ = self.up("D01", bad, b"x")
            self.assertEqual(st, 400, bad)
        self.assertFalse(os.path.exists(os.path.join(self.proj, ".trusty-mpm", "il", "attachments")))
        self.assertNotIn("attachments", self.store.get("items", "D01")["data"])
        self.assertEqual(da.clean_name(".hidden*file?.txt"), "hidden_file_.txt")

    def test_bad_item_ids_refused(self):
        for bad in ("..", "D01%2F..", "nope"):
            self.assertIn(self.up(bad, "a.txt", b"x")[0], (400, 404), bad)

    def test_symlinked_attachment_dir_refused(self):
        outside = os.path.join(self.t, "outside")
        os.makedirs(outside)
        base = os.path.join(self.proj, ".trusty-mpm", "il", "attachments")
        os.symlink(outside, base)
        st, _ = self.up("D01", "a.txt", b"x")
        self.assertEqual(st, 400)
        self.assertEqual(os.listdir(outside), [])

    def test_symlinked_item_dir_refused(self):
        outside = os.path.join(self.t, "outside2")
        os.makedirs(outside)
        base = os.path.join(self.proj, ".trusty-mpm", "il", "attachments")
        os.makedirs(base)
        os.symlink(outside, os.path.join(base, "D01"))
        self.assertEqual(self.up("D01", "a.txt", b"x")[0], 400)
        self.assertEqual(os.listdir(outside), [])

    def test_oversize_refused_by_header_and_nothing_written(self):
        with mock.patch.dict(os.environ, {"DASH_ATTACH_MAX_MB": "0.001"}):   # 1048 bytes
            st, body = self.up("D01", "big.bin", b"x" * 1100)
            self.assertEqual((st, body["code"]), (413, "too_large"))
            self.assertEqual(self.up("D01", "ok.bin", b"x" * 1000)[0], 201)
        self.assertEqual(os.listdir(os.path.join(self.proj, ".trusty-mpm", "il", "attachments", "D01")), ["ok.bin"])

    def test_save_enforces_cap_on_length(self):
        import io
        with self.assertRaises(da.TooLarge):
            da.save(io.BytesIO(b"x" * 10), 10, "a", self.t, "D01", 5)
        with self.assertRaises(ds.StoreError):   # short body
            da.save(io.BytesIO(b"xx"), 10, "a", self.t, "D01", 50)
        self.assertEqual([n for n in os.listdir(os.path.join(self.t, "D01"))], [])

    def test_cross_origin_refused(self):
        st, _ = self.up("D01", "a.txt", b"x", {"Origin": "https://evil.example"})
        self.assertEqual(st, 403)
        self.assertNotIn("attachments", self.store.get("items", "D01")["data"])

    def test_closed_or_unknown_item_refused(self):
        self.assertEqual(self.up("D03", "a.txt", b"x")[0], 400)
        self.assertEqual(self.up("DEE", "a.txt", b"x")[0], 404)

    def test_per_item_cap(self):
        with mock.patch.object(da, "MAX_PER_ITEM", 2):
            self.assertEqual(self.up("D01", "a", b"x")[0], 201)
            self.assertEqual(self.up("D01", "b", b"x")[0], 201)
            self.assertEqual(self.up("D01", "c", b"x")[0], 400)

    def test_failed_record_removes_file(self):
        with mock.patch.object(da, "record", side_effect=ds.Conflict("x")):
            self.assertEqual(self.up("D01", "a.txt", b"x")[0], 409)
        self.assertEqual(os.listdir(os.path.join(self.proj, ".trusty-mpm", "il", "attachments", "D01")), [])

    def test_get_refused(self):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}/api/attach/D01?name=a")
        with self.assertRaises(urllib.error.HTTPError) as cm:
            urllib.request.urlopen(req, timeout=5)
        self.assertEqual(cm.exception.code, 405)

    def test_attach_dir_datg_layout_and_architect_dir(self):
        proj2 = os.path.join(self.t, "p2")
        os.makedirs(proj2)
        sched = os.path.join(self.t, "s2.json")
        with open(sched, "w") as f:
            json.dump({"sessions": [{"name": "tm-p2", "project": proj2}, {"name": "tm-architect", "project": self.il}]}, f)
        self.assertEqual(da.attach_dir("tm-p2", sched, self.il), (os.path.join(proj2, ".trusty-mpm", "datg", "tm-p2", "attachments"), "project"))
        self.assertEqual(da.attach_dir("tm-architect", sched, self.il), (os.path.join(self.il, "attachments"), "project"))

    def test_page_has_upload_control_on_open_decisions_and_actions(self):
        with open(PAGE, encoding="utf-8") as f:
            html = f.read()
        self.assertIn("function attachControl", html)
        self.assertIn("/api/attach/", html)
        self.assertEqual(html.count("attachControl(body, it, tools);"), 2)   # renderQuestion + renderAction

    def get(self, path, headers=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read(), r.headers
        except urllib.error.HTTPError as e:
            return e.code, e.read(), e.headers

    def test_record_carries_type_from_extension_not_client(self):
        self.up("D01", "shot.png", b"PNG")
        self.up("D01", "run.sh", b"#!/bin/sh")
        recs = {a["name"]: a for a in self.store.get("items", "D01")["data"]["attachments"]}
        self.assertEqual((recs["shot.png"]["type"], recs["run.sh"]["type"]), ("image/png", "application/octet-stream"))

    def test_get_serves_image_inline_and_everything_else_as_download(self):
        self.up("D01", "shot.png", b"PNGDATA")
        self.up("D01", "page.html", b"<script>alert(1)</script>")
        self.up("D01", "icon.svg", b"<svg onload=alert(1)>")
        st, body, h = self.get("/api/attach/D01/shot.png")
        self.assertEqual((st, body, h["Content-Type"], h["X-Content-Type-Options"]), (200, b"PNGDATA", "image/png", "nosniff"))
        self.assertIsNone(h["Content-Disposition"])
        self.assertIn("sandbox", h["Content-Security-Policy"])
        for name in ("page.html", "icon.svg"):   # nothing executable is rendered inline
            st, body, h = self.get("/api/attach/D01/" + name)
            self.assertEqual((st, h["Content-Type"]), (200, "application/octet-stream"), name)
            self.assertTrue(h["Content-Disposition"].startswith("attachment"), name)

    def test_get_matches_the_recorded_list_and_refuses_traversal(self):
        self.up("D01", "a.txt", b"one")
        with open(os.path.join(self.t, "secret.txt"), "w") as f:
            f.write("x")
        for bad in ("..%2F..%2Fsecret.txt", "..", "b.txt", "%2Fetc%2Fpasswd", ".upload-x"):
            self.assertEqual(self.get("/api/attach/D01/" + bad)[0], 404, bad)
        self.assertEqual(self.get("/api/attach/DEE/a.txt")[0], 404)
        self.assertEqual(self.get("/api/attach/D01/a.txt", {"Host": "evil.example"})[0], 403)

    def test_get_refuses_a_record_whose_path_was_tampered_with(self):
        self.up("D01", "a.txt", b"one")
        outside = os.path.join(self.t, "outside.txt")
        with open(outside, "w") as f:
            f.write("nope")
        d = self.store.get("items", "D01")
        rec = dict(d["data"]["attachments"][0], path=outside)
        self.store.update("items", "D01", {"attachments": [rec]}, if_version=d["version"])
        self.assertEqual(self.get("/api/attach/D01/a.txt")[0], 404)

    def test_page_wires_paste_drop_staging_and_thumbnails(self):
        with open(PAGE, encoding="utf-8") as f:
            html = f.read()
        for needle in ("function stagingArea", "clipboardData.files", "dataTransfer.files", "function attachView", "stage.flush()"):
            self.assertIn(needle, html)

    def test_page_stages_text_blocks_and_places_clips_above_input(self):
        """the owner 2026-10-27: decision and action notes take pasted text blocks as .txt clips, shown above the input."""
        with open(PAGE, encoding="utf-8") as f:
            html = f.read()
        for needle in ("CLIP_TEXT_MIN", 'getData("text/plain")', "other.append(stage.row, inp, send, stage.msg)",
                       "row.prepend(stage.row)"):
            self.assertIn(needle, html)
        self.assertEqual(html.count("= stagingArea(it,"), 2)


if __name__ == "__main__":
    unittest.main()

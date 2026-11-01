"""Stable B ids for diagrams and images (owner 2026-10-22): GET /media rows carry `id`; PRs, issues and briefs get none.

Why: a media row needs a handle the owner can quote (B1f) that survives restarts, reorders and file changes.
What: allocation order and persistence, no reuse while a file is present, vanished file keeps its id (done) and
reopens, 4-hex ids past the old 256 limit, concurrent syncs allocate each file once, /media payload,
session mode (no store ids).
Test: this file.
"""
import json
import os
import re
import tempfile
import threading
import unittest
import urllib.request

from test_dashboard_doc import write
from test_dashboard_store import serve
import dashboard_media as dm
import dashboard_store as ds


def row(project, path, typ="image"):
    return {"type": typ, "project": project, "path": path, "title": path, "date": "2026-10-22", "size": 1}


class TestSyncIds(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = os.path.join(tmp.name, "oq")
        self.store = ds.Store(self.root)

    def test_allocation_is_sequential_stable_and_persisted(self):
        rows = [row("a", "docs/x.png"), row("a", "docs/y.png")]
        first = dm.sync_ids(self.store, rows)
        self.assertEqual(sorted(first.values()), ["B0000", "B0001"])
        again = dm.sync_ids(ds.Store(self.root), list(reversed(rows)))   # new Store = restart; reordered input
        self.assertEqual(again, first)
        added = dm.sync_ids(self.store, rows + [row("a", "docs/z.png")])
        self.assertEqual(added[("a", "docs/z.png")], "B0002")
        self.assertEqual({k: added[k] for k in first}, first)

    def test_svg_listed_under_both_types_shares_one_id(self):
        ids = dm.sync_ids(self.store, [row("a", "docs/p.svg", "image"), row("a", "docs/p.svg", "diagram")])
        self.assertEqual(list(ids.values()), ["B0000"])

    def test_vanished_file_keeps_id_then_reopens(self):
        a, b = row("a", "docs/a.png"), row("a", "docs/b.png")
        ids = dm.sync_ids(self.store, [a, b])
        dm.sync_ids(self.store, [b])
        doc = self.store.get("media", ids[("a", "docs/a.png")])["data"]
        self.assertEqual(doc["status"], "done")
        self.assertIn("done_at", doc)
        new = dm.sync_ids(self.store, [b, row("a", "docs/c.png")])
        self.assertEqual(new[("a", "docs/c.png")], "B0002")   # not the vanished file's B00
        back = dm.sync_ids(self.store, [a, b])
        self.assertEqual(back[("a", "docs/a.png")], ids[("a", "docs/a.png")])
        self.assertEqual(self.store.get("media", ids[("a", "docs/a.png")])["data"]["status"], "open")

    def test_ids_are_four_hex_unique_and_past_ff(self):
        rows = [row("a", f"docs/{n}.png") for n in range(300)]
        ids = dm.sync_ids(self.store, rows)
        vals = list(ids.values())
        self.assertEqual(len(set(vals)), 300)
        self.assertTrue(all(re.fullmatch(r"B[0-9a-f]{4}", v) for v in vals))
        self.assertIn("B0100", vals)   # the old 256-id limit is gone

    def test_concurrent_syncs_allocate_each_file_once(self):
        rows = [row("a", f"docs/{n}.png") for n in range(12)]
        results, errors = [], []

        def go():
            try:
                results.append(dm.sync_ids(ds.Store(self.root), rows))
            except Exception as e:   # noqa: BLE001
                errors.append(e)

        ts = [threading.Thread(target=go) for _ in range(6)]
        [t.start() for t in ts]
        [t.join() for t in ts]
        self.assertEqual(errors, [])
        self.assertTrue(all(r == results[0] for r in results))
        self.assertEqual(len(self.store.list_docs("media")), 12)
        self.assertEqual(len(set(results[0].values())), 12)

    def test_conflict_from_another_process_is_retried(self):
        # a second writer takes B00 for another file between our snapshot and our write
        real = self.store.batch
        calls = []

        def racing(ops, actor=None):
            if not calls:
                calls.append(1)
                ds.Store(self.root).set("media", "B0000", {"project": "a", "path": "docs/other.png", "status": "open",
                                                          "created": "2026-10-22T00:00:00Z", "updated": "2026-10-22T00:00:00Z"})
            return real(ops, actor)

        self.store.batch = racing
        ids = dm.sync_ids(self.store, [row("a", "docs/x.png"), row("a", "docs/other.png")])
        self.assertEqual(ids[("a", "docs/other.png")], "B0000")
        self.assertEqual(ids[("a", "docs/x.png")], "B0001")


class TestMediaPayload(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        t = os.path.realpath(tmp.name)
        self.proj = os.path.join(t, "acme")
        write(os.path.join(self.proj, "docs", "img", "shot.png"), "PNG")
        write(os.path.join(self.proj, "docs", "d", "flow.mmd"), "graph TD; A-->B")
        write(os.path.join(self.proj, "docs", "img", "pic.svg"), "<svg xmlns='http://www.w3.org/2000/svg'/>")
        write(os.path.join(self.proj, "docs", "briefs", "2026-10-22-b.md"), "# Brief\n")
        self.t, self.root = t, os.path.join(t, "oq")
        self.projects = {"acme": self.proj}

    def rows(self, config=None):
        page = os.path.join(self.t, "index.html")
        write(page, "<html></html>")
        kw = {"config": config} if config else {}
        srv = serve.DashboardServer(0, ds.Store(self.root), page, os.path.join(self.t, "b"), projects=self.projects,
                                    vendor=os.path.join(self.t, "v"), **kw)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        out = {}
        for q in ("", "?type=image", "?type=diagram"):
            with urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/media{q}", timeout=5) as r:
                out[q] = json.loads(r.read())["media"]
        return out

    def test_rows_carry_ids_stable_across_type_filters_and_restart(self):
        first = self.rows()
        allr = first[""]
        self.assertTrue(all(r["id"].startswith("B") and len(r["id"]) == 5 for r in allr))
        svg = {r["id"] for r in allr if r["path"].endswith("pic.svg")}
        self.assertEqual(len(svg), 1)   # one id for both svg rows
        self.assertEqual({r["id"] for r in first["?type=image"]} | {r["id"] for r in first["?type=diagram"]}, {r["id"] for r in allr})
        self.assertEqual(len({r["id"] for r in allr}), 3)
        self.assertEqual({(r["path"], r["id"]) for r in self.rows()[""]}, {(r["path"], r["id"]) for r in allr})

    def test_briefs_carry_ids_distinct_from_media_and_stable(self):
        def briefs():
            page = os.path.join(self.t, "index.html")
            write(page, "<html></html>")
            srv = serve.DashboardServer(0, ds.Store(self.root), page, os.path.join(self.t, "b"), projects=self.projects,
                                        vendor=os.path.join(self.t, "v"))
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            u = f"http://127.0.0.1:{srv.server_address[1]}"
            with urllib.request.urlopen(u + "/briefs", timeout=5) as r:
                b = json.loads(r.read())["briefs"]
            with urllib.request.urlopen(u + "/media", timeout=5) as r:
                m = json.loads(r.read())["media"]
            return b, m
        b1, m1 = briefs()
        self.assertEqual(len(b1), 1)
        self.assertRegex(b1[0]["id"], r"^B[0-9a-f]{4}$")
        self.assertNotIn(b1[0]["id"], {r["id"] for r in m1})
        b2, m2 = briefs()   # a media-only pass in between must not retire the brief's id
        self.assertEqual(b2[0]["id"], b1[0]["id"])
        self.assertEqual({r["id"] for r in m2}, {r["id"] for r in m1})

    def test_session_mode_rows_have_no_ids_and_store_untouched(self):
        cfg = {"mode": "session", "title": "S", "briefs_base": "", "storage_key": "s"}
        rows = self.rows(cfg)[""]
        self.assertTrue(rows and all("id" not in r for r in rows))
        self.assertEqual(ds.Store(self.root).list_docs("media"), [])


if __name__ == "__main__":
    unittest.main()

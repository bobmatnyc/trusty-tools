"""GET /media?type= and GET /media-file/<project>/<rel>: diagrams and images under the doc roots, read-only, same refusals as /doc.

Why: the right pane (media library) lists diagrams and images; a file outside the doc roots, a dot or secret path, a
symlink escape or a non-media extension must be a 404.
What: listing by type (svg under both), mermaid-block markdown, deny list, symlink escape, content types, SVG sandbox
header, dot-dot, writes refused.
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
import dashboard_store as ds


class TestMedia(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        t = os.path.realpath(tmp.name)
        proj, arch = os.path.join(t, "acme"), os.path.join(t, "arch")
        write(os.path.join(proj, "docs", "diagrams", "flow.mmd"), "graph TD; A-->B")
        write(os.path.join(proj, "docs", "briefs", "2026-10-20-m.md"), "# Has diagram\n\n```mermaid\ngraph TD; A-->B\n```\n")
        write(os.path.join(proj, "docs", "briefs", "plain.md"), "# No diagram\n")
        write(os.path.join(proj, "docs", "img", "shot.png"), "PNGDATA")
        write(os.path.join(proj, "docs", "img", "pic.svg"), "<svg xmlns='http://www.w3.org/2000/svg'/>")
        write(os.path.join(proj, "docs", ".hidden", "x.png"), "DENIED")
        write(os.path.join(proj, "docs", "secret-plan.png"), "DENIED")
        write(os.path.join(proj, "docs", "node_modules", "p", "y.png"), "DENIED")
        write(os.path.join(proj, "src", "outside.png"), "OUTSIDE ROOTS")
        write(os.path.join(proj, "docs", "notes.txt"), "plain")
        write(os.path.join(t, "elsewhere.png"), "ESCAPE")
        os.symlink(os.path.join(t, "elsewhere.png"), os.path.join(proj, "docs", "img", "link.png"))
        write(os.path.join(arch, "local", "briefs", "arch.mmd"), "graph LR; X-->Y")
        page = os.path.join(t, "index.html")
        write(page, "<html></html>")
        self.srv = serve.DashboardServer(0, ds.Store(os.path.join(t, "oq")), page, os.path.join(arch, "local", "briefs"),
                                         projects={"acme": proj, "architect": arch}, vendor=os.path.join(t, "v"))
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def get(self, path, method="GET"):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def rows(self, q=""):
        st, _, body = self.get("/media" + q)
        self.assertEqual(st, 200)
        return json.loads(body)["media"]

    def test_listing_by_type(self):
        d = {(r["project"], r["path"]) for r in self.rows("?type=diagram")}
        self.assertEqual(d, {("acme", "docs/diagrams/flow.mmd"), ("acme", "docs/briefs/2026-10-20-m.md"), ("acme", "docs/img/pic.svg"),
                             ("architect", "local/briefs/arch.mmd")})
        i = {(r["project"], r["path"]) for r in self.rows("?type=image")}
        self.assertEqual(i, {("acme", "docs/img/shot.png"), ("acme", "docs/img/pic.svg")})
        both = self.rows()
        self.assertEqual({r["type"] for r in both}, {"diagram", "image"})
        self.assertEqual(self.rows("?type=pr"), [])
        md = next(r for r in both if r["path"].endswith("2026-10-20-m.md"))
        self.assertEqual((md["title"], md["date"]), ("Has diagram", "2026-10-20"))

    def test_listing_excludes_denied_and_escaped(self):
        paths = " ".join(r["path"] for r in self.rows())
        for bad in ("hidden", "secret", "node_modules", "outside.png", "link.png", "notes.txt", "plain.md"):
            self.assertNotIn(bad, paths)

    def test_file_served_with_type_and_svg_sandbox(self):
        st, h, body = self.get("/media-file/acme/docs/img/shot.png")
        self.assertEqual((st, h["Content-Type"], body), (200, "image/png", b"PNGDATA"))
        st, h, _ = self.get("/media-file/acme/docs/img/pic.svg")
        self.assertEqual((st, h["Content-Type"]), (200, "image/svg+xml"))
        self.assertIn("sandbox", h["Content-Security-Policy"])
        self.assertEqual(self.get("/media-file/acme/docs/diagrams/flow.mmd")[2], b"graph TD; A-->B")
        self.assertEqual(self.get("/media-file/architect/local/briefs/arch.mmd")[0], 200)
        self.assertEqual(self.get("/media-file/acme/docs/img/shot.png", "HEAD")[0], 200)

    def test_refusals_are_404(self):
        for p in ("acme/docs/.hidden/x.png", "acme/docs/secret-plan.png", "acme/docs/node_modules/p/y.png", "acme/src/outside.png",
                  "acme/docs/img/link.png", "acme/docs/notes.txt", "acme/docs/briefs/plain.md", "acme/docs/%2e%2e/src/outside.png",
                  "acme/docs/img/..%2f..%2fsrc%2foutside.png", "nope/docs/img/shot.png", "acme", "acme/docs", "acme/docs/img/missing.png"):
            self.assertEqual(self.get("/media-file/" + p)[0], 404, p)

    def test_accessed_time_is_stored_and_listed(self):
        rows = self.rows()
        shot = next(r for r in rows if r["path"].endswith("shot.png"))
        self.assertNotIn("accessed", shot)
        body = json.dumps({"data": {"accessed": "2026-10-22T08:30:00.000Z"}}).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}/api/media/{shot['id']}", data=body, method="PATCH",
                                     headers={"Content-Type": "application/json"})
        self.assertEqual(urllib.request.urlopen(req, timeout=5).status, 200)
        rows = self.rows()
        self.assertEqual(next(r for r in rows if r["id"] == shot["id"])["accessed"], "2026-10-22T08:30:00.000Z")
        self.assertEqual([r for r in rows if "accessed" in r and r["id"] != shot["id"]], [])
        st, _, doc = self.get(f"/api/media/{shot['id']}")
        self.assertEqual(json.loads(doc)["data"]["accessed"], "2026-10-22T08:30:00.000Z")

    def test_writes_refused(self):
        for path in ("/media", "/media-file/acme/docs/img/shot.png"):
            req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=b"x", method="POST")
            with self.assertRaises(urllib.error.HTTPError) as cm:
                urllib.request.urlopen(req, timeout=5)
            self.assertEqual(cm.exception.code, 405)


if __name__ == "__main__":
    unittest.main()

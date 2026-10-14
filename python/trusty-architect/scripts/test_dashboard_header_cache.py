"""Dashboard header control (Collapse all) in both page copies, and Cache-Control: no-store on / and /api.

Run: python3 -I -m unittest discover -s scripts -p 'test_dashboard*.py'
"""
import http.client
import importlib.util
import os
import sys
import tempfile
import threading
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
PAGES = (
    os.path.join(REPO, "templates", "dashboard", "index.html"),
)


class CollapseAll(unittest.TestCase):
    def test_header_control_in_both_pages(self):
        for p in PAGES:
            s = open(p, encoding="utf-8").read()
            head = s[s.index("<header>"):s.index("</header>")]
            self.assertIn('<button type="button" class="collapse-all" id="collapse-all"', head, p)
            self.assertIn(">Collapse all</button>", head, p)
            self.assertIn("Expand sections", s, p)
            self.assertIn('$("collapse-all")', s, p)
            self.assertNotIn("confirm(", s[s.index("collapse all / expand"):s.index("static accordion parts")], p)


class NoStore(unittest.TestCase):
    def setUp(self):
        if HERE not in sys.path:
            sys.path.insert(0, HERE)
        spec = importlib.util.spec_from_file_location("dashboard_serve_cache", os.path.join(HERE, "dashboard-serve.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        page = os.path.join(tmp.name, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        srv = mod.DashboardServer(0, mod.ds.Store(os.path.join(tmp.name, "store")), page)
        self.port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)

    def test_no_store_on_page_and_api(self):
        for path in ("/", "/api/user"):
            c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
            c.request("GET", path)
            r = c.getresponse()
            r.read()
            c.close()
            self.assertEqual(r.status, 200, path)
            self.assertEqual(r.getheader("Cache-Control"), "no-store", path)


if __name__ == "__main__":
    unittest.main()

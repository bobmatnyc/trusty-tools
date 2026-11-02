"""#10606: a dashboard on Python < 3.11 (no tomllib) says so on stderr, in /config.json `warnings` and on the page.

Covers: runtime_warnings() for a missing tomllib and for a 3.9 version; the startup stderr line; the /config.json field
on a live server with tomllib forced off; no warning anywhere on this interpreter (>= 3.11); the page's banner hook.
Test: tests/test_dashboard_python_floor.py
"""
import http.client
import importlib.util
import io
import json
import os
import sys
import tempfile
import threading
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
PAGE = os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html")


def load():
    spec = importlib.util.spec_from_file_location("dashboard_serve_floor", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class PythonFloor(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = load()
        cls.dp = cls.mod.dashboard_projects

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name

    def config(self):
        page = os.path.join(self.tmp, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        srv = self.mod.DashboardServer(0, self.mod.ds.Store(os.path.join(self.tmp, "store")), page,
                                       claude_projects=os.path.join(self.tmp, "claude"))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        port = srv.server_address[1]
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        c.request("GET", "/config.json", headers={"Host": f"127.0.0.1:{port}"})
        body = c.getresponse().read()
        c.close()
        return json.loads(body)

    def test_a_missing_tomllib_is_reported_on_stderr_and_in_config(self):
        with mock.patch.object(self.dp, "tomllib", None):
            self.assertEqual(len(self.dp.runtime_warnings()), 1)
            err = io.StringIO()
            self.mod.warn_runtime(err)
            self.assertIn("dashboard-serve: WARNING: Python", err.getvalue())
            self.assertIn("3.11 or newer", err.getvalue())
            warnings = self.config()["warnings"]
        self.assertEqual(len(warnings), 1)
        self.assertIn("tomllib", warnings[0])

    def test_python_3_9_is_below_the_floor(self):
        (line,) = self.dp.runtime_warnings((3, 9, 6))
        self.assertTrue(line.startswith("Python 3.9 runs this dashboard; it needs Python 3.11 or newer"), line)

    @unittest.skipIf(sys.version_info < (3, 11), "this interpreter is itself below the floor")
    def test_no_warning_on_a_supported_python(self):
        self.assertEqual(self.dp.runtime_warnings(), [])
        err = io.StringIO()
        self.mod.warn_runtime(err)
        self.assertEqual(err.getvalue(), "")
        self.assertEqual(self.config()["warnings"], [])

    def test_the_page_shows_the_warnings_as_a_banner(self):
        with open(PAGE, encoding="utf-8") as f:
            page = f.read()
        self.assertIn("if (j && Array.isArray(j.warnings)) d.warnings = j.warnings.filter(", page)
        self.assertIn('wb.id = "runtime-warning"', page)
        self.assertIn("if (CFG.warnings && CFG.warnings.length) {", page)


if __name__ == "__main__":
    unittest.main()

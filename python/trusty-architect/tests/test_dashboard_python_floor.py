"""#10606: a dashboard on Python < 3.11 (no tomllib) says so on stderr, in /config.json `warnings` and on the page.

Covers: runtime_warnings() for a missing tomllib and for a 3.9 version; the startup stderr line; the /config.json field
on a live server with tomllib forced off; no warning anywhere on this interpreter (>= 3.11); the page's banner hook;
the tmux line when tmux is not on PATH (looked up, never run), none with the module's stub tmux on PATH.
Test: tests/test_dashboard_python_floor.py
"""
import http.client
import importlib.util
import io
import json
import os
import shutil
import sys
import tempfile
import threading
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
PAGE = os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html")


# #10606: a stub `tmux` (exit 1) first on PATH for the whole module, so no route reaches a real tmux server
_STUB = _PATH = None


def setUpModule():
    global _STUB, _PATH
    _STUB = tempfile.TemporaryDirectory()
    stub = os.path.join(_STUB.name, "tmux")
    with open(stub, "w") as f:
        f.write("#!/bin/sh\nexit 1\n")
    os.chmod(stub, 0o755)
    _PATH = mock.patch.dict(os.environ, {"PATH": _STUB.name + os.pathsep + os.environ.get("PATH", "")})
    _PATH.start()


def tearDownModule():
    _PATH.stop()
    _STUB.cleanup()


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

    def test_tmux_presence_on_path_decides_the_tmux_warning(self):
        # #10606: absent -> one tmux line, in runtime_warnings, on stderr and in /config.json
        empty = os.path.join(self.tmp, "no-tmux")
        os.makedirs(empty)
        with mock.patch.dict(os.environ, {"PATH": empty}):
            lines = [w for w in self.dp.runtime_warnings((3, 12)) if "tmux" in w]
            self.assertEqual(len(lines), 1, lines)
            self.assertIn("live sessions cannot be listed", lines[0])
            err = io.StringIO()
            self.mod.warn_runtime(err)
            self.assertIn("dashboard-serve: WARNING: tmux is not on this dashboard's PATH", err.getvalue())
            self.assertEqual([w for w in self.config()["warnings"] if "tmux" in w], lines)
        # present (the module's stub, found by a PATH lookup) -> no tmux line
        self.assertEqual(os.path.dirname(shutil.which("tmux")), _STUB.name)
        self.assertEqual([w for w in self.dp.runtime_warnings((3, 12)) if "tmux" in w], [])

    def test_the_page_shows_the_warnings_as_a_banner(self):
        with open(PAGE, encoding="utf-8") as f:
            page = f.read()
        self.assertIn("if (j && Array.isArray(j.warnings)) d.warnings = j.warnings.filter(", page)
        self.assertIn('wb.id = "runtime-warning"', page)
        self.assertIn("if (CFG.warnings && CFG.warnings.length) {", page)


if __name__ == "__main__":
    unittest.main()

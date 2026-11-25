"""#10606: the dashboard reads TOML on Python < 3.11 (no tomllib) and warns only when tmux is missing.

Covers: with tomllib absent, dashboard_toml loads its own reader and stack_info / config_info read Cargo.toml and
.trusty-mpm.toml; that reader parses a TOML 1.0 sample the same as tomllib (when present) and rejects malformed
input; no warning anywhere when tmux is on PATH; the page's banner hook; the tmux line when tmux is not on PATH
(looked up, never run), none with the module's stub tmux on PATH.
Test: tests/test_dashboard_python_floor.py
"""
import contextlib
import http.client
import importlib
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


SAMPLE = r'''
# a comment
title = "TOML \u00e9 \"q\""
lit = 'C:\path'
multi = """
line one \
  joined"""
raw = """""two quotes inside"""""
ints = [1_000, 0xff, 0o17, 0b101, -3, +4]
floats = [3.14, -1e3, 6.0E-2]
on = true
off = false
inline = { x = 1, "y.z" = 'two', nested = { ok = true } }
dotted.a.b = "deep"
arr = [
  "a",  # comment inside
  "b",
]

[package]
name = "demo"
version.workspace = true

[[bin]]
name = "one"

[[bin]]
name = "two"

[bin.extra]
k = "v"
'''
EXPECTED = {
    "title": 'TOML \u00e9 "q"', "lit": "C:\\path", "multi": "line one joined", "raw": '""two quotes inside""',
    "ints": [1000, 255, 15, 5, -3, 4], "floats": [3.14, -1000.0, 0.06], "on": True, "off": False,
    "inline": {"x": 1, "y.z": "two", "nested": {"ok": True}}, "dotted": {"a": {"b": "deep"}}, "arr": ["a", "b"],
    "package": {"name": "demo", "version": {"workspace": True}},
    "bin": [{"name": "one"}, {"name": "two", "extra": {"k": "v"}}],
}
BAD = ('a = "open', "a = 1\na = 2", "[t]\n[t]", "a = [1 2]", "= 1", "a = 1 b = 2")


@contextlib.contextmanager
def no_tomllib():
    """#10606: simulate Python < 3.11: block `import tomllib`, reload dashboard_toml under the block, then restore."""
    mod = sys.modules.get("dashboard_toml")
    bound = [m for m in list(sys.modules.values()) if m is not None and getattr(m, "tomllib", None) is not None]
    try:
        with contextlib.ExitStack() as stack, mock.patch.dict(sys.modules, {"tomllib": None}):
            for m in bound:   # a module that bound tomllib at import time loses it too
                stack.enter_context(mock.patch.object(m, "tomllib", None))
            if mod is not None:
                importlib.reload(mod)
            yield mod
    finally:
        if mod is not None:
            importlib.reload(mod)


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

    def test_without_tomllib_the_stack_and_config_rows_are_read(self):
        # #10606: tomllib blocked -> dashboard_toml's own reader fills the rows a 3.9 host showed empty
        with open(os.path.join(self.tmp, "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "demo" # a comment\nversion = "1.2.3"\n')
        with open(os.path.join(self.tmp, ".trusty-mpm.toml"), "w") as f:
            f.write('[project]\nname = "demo"\nport = 8080\n')
        with no_tomllib() as fallback:
            stack = self.dp.stack_info(self.tmp)
            (cfg,) = [c for c in self.dp.config_info(self.tmp) if c["file"] == ".trusty-mpm.toml"]
            warnings = self.dp.runtime_warnings()
            native = getattr(fallback, "NATIVE", None)
        self.assertIn({"kind": "rust", "name": "demo", "version": "1.2.3"}, stack)
        self.assertEqual(cfg["fields"], {"project.name": "demo", "project.port": 8080})
        self.assertEqual(warnings, [])
        self.assertIs(native, False)

    def test_the_fallback_reader_matches_tomllib_and_rejects_bad_input(self):
        with no_tomllib() as fallback:
            got = fallback.loads(SAMPLE)
            for bad in BAD:
                with self.assertRaises(ValueError, msg=bad):
                    fallback.loads(bad)
        self.assertEqual(got, EXPECTED)
        if sys.version_info >= (3, 11):
            import tomllib
            self.assertEqual(tomllib.loads(SAMPLE), EXPECTED)
            self.assertTrue(self.dp.dashboard_toml.NATIVE)   # 3.11+ keeps tomllib itself

    def test_no_warning_with_tmux_on_path(self):
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
            lines = [w for w in self.dp.runtime_warnings() if "tmux" in w]
            self.assertEqual(len(lines), 1, lines)
            self.assertIn("live sessions cannot be listed", lines[0])
            err = io.StringIO()
            self.mod.warn_runtime(err)
            self.assertIn("dashboard-serve: WARNING: tmux is not on this dashboard's PATH", err.getvalue())
            self.assertEqual([w for w in self.config()["warnings"] if "tmux" in w], lines)
        # present (the module's stub, found by a PATH lookup) -> no tmux line
        self.assertEqual(os.path.dirname(shutil.which("tmux")), _STUB.name)
        self.assertEqual([w for w in self.dp.runtime_warnings() if "tmux" in w], [])

    def test_the_page_shows_the_warnings_as_a_banner(self):
        with open(PAGE, encoding="utf-8") as f:
            page = f.read()
        self.assertIn("if (j && Array.isArray(j.warnings)) d.warnings = j.warnings.filter(", page)
        self.assertIn('wb.id = "runtime-warning"', page)
        self.assertIn("if (CFG.warnings && CFG.warnings.length) {", page)


if __name__ == "__main__":
    unittest.main()

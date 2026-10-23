"""dashboard-serve.py tailnet Host/Origin guard: MagicDNS name accepted, everything else 403.

Covers: tailnet Host (bare, :port, :443) accepted, tailnet Origin write accepted,
other Host 403, other Origin write 403, tailscale missing/failing falls back to localhost-only.
Never calls the real tailscale.
Run: python3 -I -m unittest discover -s scripts -p 'test_dashboard*.py'
"""
import http.client
import importlib.util
import io
import json
import os
import shutil
import subprocess
import tempfile
import threading
import unittest
from contextlib import redirect_stderr
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
NAME = "host.tailnet.example.com"


def load_serve():
    import sys
    if SCRIPTS not in sys.path:
        sys.path.insert(0, SCRIPTS)
    spec = importlib.util.spec_from_file_location("dashboard_serve_tailnet", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Guard(unittest.TestCase):
    def setUp(self):
        self.mod = load_serve()
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        page = os.path.join(tmp.name, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        store = self.mod.ds.Store(os.path.join(tmp.name, "store"))
        self.srv = self.mod.DashboardServer(0, store, page, tailnet_names=allowed(self.mod, [NAME]))
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def req(self, method, path, headers, body=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
        for k, v in headers.items():
            c.putheader(k, v)
        data = json.dumps(body).encode() if body is not None else None
        if data is not None:
            c.putheader("Content-Type", "application/json")
            c.putheader("Content-Length", str(len(data)))
        c.endheaders(data)
        r = c.getresponse()
        r.read()
        c.close()
        return r.status

    def test_tailnet_host_accepted_all_forms(self):
        for h in (NAME, f"{NAME}:{self.port}", f"{NAME}:443"):
            self.assertEqual(self.req("GET", "/api/user", {"Host": h}), 200, h)

    def test_local_host_accepted(self):
        self.assertEqual(self.req("GET", "/api/user", {"Host": f"127.0.0.1:{self.port}"}), 200)

    def test_other_host_403(self):
        for h in ("evil.example.com", f"evil.example.com:{self.port}", f"x.{NAME}", "127.0.0.1"):
            self.assertEqual(self.req("GET", "/api/user", {"Host": h}), 403, h)

    def test_tailnet_origin_write_accepted(self):
        for o in (f"https://{NAME}", f"https://{NAME}:{self.port}"):
            st = self.req("POST", "/api/items", {"Host": NAME, "Origin": o}, {"data": {"title": "t"}})
            self.assertNotEqual(st, 403, o)

    def test_other_origin_write_403(self):
        for o in ("https://evil.example.com", f"http://{NAME}", f"https://{NAME}.evil.com", "null"):
            st = self.req("POST", "/api/items", {"Host": NAME, "Origin": o}, {"data": {"title": "t"}})
            self.assertEqual(st, 403, o)

    def test_config_names_the_tailnet_origin_for_the_page(self):
        # #10161: the page's tailnet link comes from /config.json, never from a host name in the page
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request("GET", "/config.json", headers={"Host": f"127.0.0.1:{self.port}"})
        cfg = json.loads(c.getresponse().read())
        c.close()
        self.assertEqual(cfg["tailnet"], f"https://{NAME}:7890/")
        self.assertEqual(self.mod.tailnet_origin(frozenset()), "")


class PageTailnet(unittest.TestCase):
    """#10161: the reader's dashHosts admits the configured tailnet origin and nothing built into the page."""

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_dash_hosts_follow_data_tailnet(self):
        with open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html"), encoding="utf-8") as f:
            page = f.read()
        a = page.index("  const dashHosts = () => {")
        src = page[a:page.index("  };\n", a) + 5]
        js = src + """
        const out = [];
        for (const t of ["https://host.tailnet.example.com:7890/", ""]) {
          attr = t;
          out.push(["https://host.tailnet.example.com:7890", "http://127.0.0.1:7890", "https://evil.example.com:7890"].map((o) => dashHosts().test(o)));
        }
        console.log(JSON.stringify(out));
        """
        js = 'let attr = ""; const document = { documentElement: { getAttribute: () => attr } };\n' + js
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), [[True, True, False], [False, True, False]])


def allowed(mod, extra):
    return mod.allowed_names(extra, lookup=lambda: None)


class Lookup(unittest.TestCase):
    def setUp(self):
        self.mod = load_serve()

    def test_tailscale_missing_is_localhost_only(self):
        err = io.StringIO()
        with mock.patch.object(self.mod.shutil, "which", return_value=None), \
                mock.patch.object(self.mod.os, "access", return_value=False), redirect_stderr(err):
            self.assertIsNone(self.mod.tailnet_name())
            self.assertEqual(self.mod.allowed_names(), frozenset())
        self.assertEqual(len(err.getvalue().strip().splitlines()), 2)

    def test_tailscale_failure_is_localhost_only(self):
        with mock.patch.object(self.mod.shutil, "which", return_value="/x/tailscale"), \
                mock.patch.object(self.mod.subprocess, "run", side_effect=OSError("boom")), \
                redirect_stderr(io.StringIO()):
            self.assertIsNone(self.mod.tailnet_name())

    def test_dnsname_parsed_and_trailing_dot_stripped(self):
        out = mock.Mock(stdout=json.dumps({"Self": {"DNSName": f"{NAME}."}}))
        with mock.patch.object(self.mod.shutil, "which", return_value="/x/tailscale"), \
                mock.patch.object(self.mod.subprocess, "run", return_value=out):
            self.assertEqual(self.mod.tailnet_name(), NAME)
            self.assertEqual(self.mod.allowed_names(["Extra.Example."]), frozenset({NAME, "extra.example"}))


if __name__ == "__main__":
    unittest.main()

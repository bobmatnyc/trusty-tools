"""dashboard_routes.py routes on a live server, and the optional modules that are not shipped yet (#10571).

Covers: /static/tokens.css (fixed names only), /api/fleet and /api/projects shapes (architect mode), the Projects view
with an empty registry (no error, no secrets module), /api/cli-profiles and the Secrets and lifecycle
routes refused (404/405) while their modules are absent, PUT /doc compare-and-swap, the /live page getting the tokens link
but no shim, gzip on /api/harness, and /config.json carrying the console origin and the configured dashboard port.
Test: tests/test_dashboard_routes.py
"""
import gzip
import hashlib
import http.client
import importlib.util
import json
import os
import tempfile
import threading
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
NAME = "host.tailnet.example.com"


def load():
    spec = importlib.util.spec_from_file_location("dashboard_serve_routes", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Routes(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = load()

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.page = os.path.join(self.tmp, "index.html")
        with open(self.page, "w") as f:
            f.write("<!doctype html><html><head></head><body>x</body></html>")
        self.live = os.path.join(self.tmp, "live.html")
        with open(self.live, "w") as f:
            f.write("<html><head></head><body>live</body></html>")
        with open(os.path.join(self.tmp, "tokens.css"), "w") as f:
            f.write(":root { --x: 1px; }\n")
        self.proj = os.path.join(self.tmp, "proj")
        os.makedirs(os.path.join(self.proj, "docs"))
        self.doc = os.path.join(self.proj, "docs", "a.md")
        with open(self.doc, "w") as f:
            f.write("# A\n")
        self.srv = self.mod.DashboardServer(0, self.mod.ds.Store(os.path.join(self.tmp, "store")), self.page,
                                            tailnet_names=frozenset([NAME]), projects={"p": self.proj},
                                            static=self.tmp, live_page=self.live, inbox=os.path.join(self.tmp, "inbox"),
                                            claude_projects=os.path.join(self.tmp, "claude"))
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)
        self.port = self.srv.server_address[1]

    def req(self, method, path, body=None, headers=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        h = {"Host": f"127.0.0.1:{self.port}", **(headers or {})}
        c.request(method, path, body=body, headers=h)
        r = c.getresponse()
        data = r.read()
        c.close()
        return r.status, dict(r.getheaders()), data

    def test_static_serves_the_tokens_only(self):
        st, hd, data = self.req("GET", "/static/tokens.css")
        self.assertEqual((st, hd["Content-Type"]), (200, "text/css; charset=utf-8"))
        self.assertIn(b"--x", data)
        self.assertEqual(self.req("GET", "/static/index.html")[0], 404)

    def test_fleet_and_projects_answer_in_architect_mode(self):
        st, _, data = self.req("GET", "/api/fleet")
        self.assertEqual(st, 200)
        self.assertIn("count", json.loads(data))
        st, _, data = self.req("GET", "/api/projects")
        self.assertEqual(st, 200)
        body = json.loads(data)
        self.assertEqual(body["projects"], [])   # conftest points TRUSTY_MPM_ROOT at an empty registry
        self.assertFalse(body.get("registry_error"))   # no secrets module needed to read it (#10571)

    def test_unshipped_optional_routes_are_refused(self):
        self.assertEqual(self.req("GET", "/api/cli-profiles")[0], 404)
        hdr = {"Origin": f"http://127.0.0.1:{self.port}", "Content-Type": "application/json"}
        self.assertIn(self.req("POST", "/api/secrets/list", b"{}", hdr)[0], (404, 405))
        self.assertIn(self.req("POST", "/api/projects/archive/p", b"{}", hdr)[0], (404, 405))   # falls through to the store API

    def test_doc_put_is_compare_and_swap(self):
        st, hd, data = self.req("GET", "/doc/p/docs/a.md")
        self.assertEqual(st, 200)
        base = hd["X-Doc-Sha256"]
        self.assertEqual(base, hashlib.sha256(data).hexdigest())
        origin = {"Origin": f"http://127.0.0.1:{self.port}", "Content-Type": "application/json"}
        body = json.dumps({"text": "# B\n", "base_sha256": base}).encode()
        self.assertEqual(self.req("PUT", "/doc/p/docs/a.md", body, {"Content-Type": "application/json"})[0], 403)   # no Origin
        self.assertEqual(self.req("PUT", "/doc/p/docs/a.md", body, origin)[0], 200)
        self.assertEqual(open(self.doc).read(), "# B\n")
        st, _, data = self.req("PUT", "/doc/p/docs/a.md", body, origin)   # base is stale now
        self.assertEqual((st, json.loads(data)["code"]), (409, "conflict"))

    def test_live_page_gets_the_tokens_link_but_no_shim(self):
        st, _, data = self.req("GET", "/live")
        self.assertEqual(st, 200)
        self.assertIn(b"/static/tokens.css", data)
        self.assertNotIn(b"window.claude", data)
        st, _, data = self.req("GET", "/")
        self.assertIn(b"window.claude", data)

    def test_harness_is_gzipped_for_a_client_that_accepts_it(self):
        st, hd, data = self.req("GET", "/api/harness", headers={"Accept-Encoding": "gzip"})
        self.assertEqual((st, hd.get("Content-Encoding")), (200, "gzip"))
        self.assertIn("sessions", json.loads(gzip.decompress(data)))

    def test_config_reports_the_absent_backends_off(self):
        # #10571: no dashboard_secrets, cli_profiles or dashboard_lifecycle ships, so every flag is off
        cfg = json.loads(self.req("GET", "/config.json")[2])
        self.assertEqual(cfg["features"], {"secrets": False, "cli_profiles": False, "project_lifecycle": False})

    def test_page_drops_a_tab_whose_feature_is_off(self):
        # #10571: the nav entries name their feature, navCats filters on it, and the sections are removed
        page = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html"), encoding="utf-8").read()
        self.assertIn('{ key: "secrets", label: "Secrets", secs: ["sec-secrets"], feature: "secrets",', page)
        self.assertIn('{ key: "cliprofiles", label: "CLI Profiles", secs: ["sec-cliprof"], feature: "cli_profiles",', page)
        self.assertIn("NAV.filter((c) => !(c.arch && SESSION) && !(c.feature && featureOff(c.feature)))", page)
        self.assertIn('[["secrets", "sec-secrets"], ["cli_profiles", "sec-cliprof"]].forEach(([f, id]) => { if (featureOff(f))', page)
        self.assertIn("const featureOff = (name) => !!(CFG.features && CFG.features[name] === false);", page)
        self.assertNotIn('new Error((j && j.error) || ("HTTP " + res.status))', page)   # no raw HTTP code shown to the owner
        self.assertIn("the feature isn't set up yet", page)

    def test_config_names_the_console_origin(self):
        cfg = json.loads(self.req("GET", "/config.json")[2])
        self.assertEqual(cfg["console"], f"https://{NAME}/")


class ConfiguredSettings(unittest.TestCase):
    def test_port_and_claude_dir_come_from_local_architect_json(self):
        mod = load()
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "architect.json")
            with open(path, "w") as f:
                json.dump({"dashboard_port": 7999, "claude_config_dir": os.path.join(tmp, "cc")}, f)
            env = {"ARCHITECT_CONFIG": path, "DASHBOARD_PORT": "", "CLAUDE_CONFIG_DIR": ""}
            with mock.patch.dict(os.environ, env):
                self.assertEqual(mod.instance_server_settings(), (7999, os.path.join(tmp, "cc", "projects")))


if __name__ == "__main__":
    unittest.main()

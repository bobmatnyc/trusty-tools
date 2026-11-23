"""The Projects view's Secrets list shows key names by owner/project, never a value (#10350, DOC-82 §10).

Covers: parse_list keeps only valid key-name lines; build groups by GitHub owner with the owner vault once and
"No owner" last; a tm failure is a fixed sentence (no tm text); GET /api/projects/key-names on a live server returns the
names and no byte of any value the fake tm printed; session mode is 404 and POST is 405; the page renders with
textContent only.
Test: tests/test_dashboard_keynames.py
"""
import http.client
import importlib.util
import json
import os
import sys
import tempfile
import threading
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)
import dashboard_keynames as kn  # noqa: E402

VALUE = "sk-live-" + "Zq9" * 12   # a value-shaped string the fake tm leaks; it must never reach a response
LISTING = ("trusty/acme/web (Project):\n  API_KEY\n  grp.DB_URL (push-only for agents)\n  LEAK=%s\n  TOKEN %s\n"
           "trusty/acme (Owner):\n  OWNER_KEY\n" % (VALUE, VALUE))


def fake_runner(root):
    if root.endswith("broken"):
        return 3, VALUE   # a failure whose output carries a value: never shown
    return 0, LISTING


def rows(tmp):
    web, broken = os.path.join(tmp, "web"), os.path.join(tmp, "broken")
    return [{"id": "web", "label": "acme/web", "owner": "acme", "repo": "web", "root": web, "scanned": True, "no_github_url": False},
            {"id": "api", "label": "acme/api", "owner": "acme", "repo": "api", "root": broken, "scanned": True, "no_github_url": False},
            {"id": "solo", "label": "solo", "owner": None, "repo": None, "root": None, "scanned": False, "no_github_url": True}]


class FakeProjects:
    def __init__(self, tmp):
        self.tmp = tmp

    def get(self, sess_ids, watch_docs=()):
        return {"projects": rows(self.tmp), "registry_error": None}


class KeyNames(unittest.TestCase):
    def setUp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        self.tmp = t.name

    def test_parse_keeps_only_key_name_lines(self):
        got = kn.parse_list("stray\n" + LISTING)
        self.assertEqual([(v["vault"], v["kind"]) for v in got], [("trusty/acme/web", "project"), ("trusty/acme", "owner")])
        self.assertEqual(got[0]["names"], [{"name": "API_KEY", "push_only": False}, {"name": "grp.DB_URL", "push_only": True}])
        self.assertNotIn(VALUE, json.dumps(got))

    def test_build_groups_by_owner_with_the_owner_vault_once(self):
        out = kn.build(rows(self.tmp), fake_runner)
        self.assertEqual([g["key"] for g in out["owners"]], ["acme", kn.NONE_KEY])
        acme = out["owners"][0]
        self.assertEqual(acme["vaults"], [{"vault": "trusty/acme", "names": [{"name": "OWNER_KEY", "push_only": False}]}])
        by = {p["id"]: p for p in acme["projects"]}
        self.assertEqual([p["id"] for p in acme["projects"]], ["api", "web"])   # sorted by label
        self.assertEqual([n["name"] for n in by["web"]["names"]], ["API_KEY", "grp.DB_URL"])
        self.assertEqual(by["api"]["error"], "Not listed: `tm secrets list` exited 3. Run it in the project folder for the reason.")
        self.assertTrue(out["owners"][1]["projects"][0]["error"].startswith("Not listed: this project's folder"))
        self.assertNotIn(VALUE, json.dumps(out))

    def test_exit_zero_without_a_project_header_fails_closed(self):
        # #10350 fix round: tm's empty vault prints "  (no keys)" under a header, so no header is never "no keys"
        for text in ("", "garbage\n", "garbage " + VALUE + "\n"):
            row = kn.build(rows(self.tmp)[:1], lambda root, t=text: (0, t))["owners"][0]["projects"][0]
            self.assertEqual(row["error"], "Not listed: `tm secrets list` printed no project vault, so its output "
                                           "could not be read. Run it in the project folder to see why.", text)
            self.assertEqual((row["vault"], row["names"]), (None, []))
            self.assertNotIn(VALUE, json.dumps(row))

    def test_tm_not_runnable_is_a_fixed_sentence(self):
        out = kn.build(rows(self.tmp)[:1], lambda root: (None, ""))
        self.assertIn("tm could not be run", out["owners"][0]["projects"][0]["error"])

    def _server(self, mode="architect", runner=fake_runner):
        spec = importlib.util.spec_from_file_location("dashboard_serve_keynames", os.path.join(SCRIPTS, "dashboard-serve.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        page = os.path.join(self.tmp, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        pv = FakeProjects(self.tmp)
        cfg = dict(mod.ARCHITECT_CONFIG, mode=mode)
        srv = mod.DashboardServer(0, mod.ds.Store(os.path.join(self.tmp, "store")), page, projects={}, projects_view=pv,
                                  config=cfg, key_names=kn.KeyNamesView(pv, runner=runner))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        return srv.server_address[1]

    def _req(self, port, method="GET", path="/api/projects/key-names", headers=None):
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        h = {"Host": f"127.0.0.1:{port}", **(headers or {})}
        if method != "GET":
            h.update({"Origin": f"http://127.0.0.1:{port}", "Content-Type": "application/json"})
        c.request(method, path, body=b"{}" if method != "GET" else None, headers=h)
        r = c.getresponse()
        body = r.read()
        c.close()
        return r.status, body

    def test_route_returns_names_and_never_a_value(self):
        port = self._server()
        status, body = self._req(port, path="/api/projects/key-names?refresh=1")
        self.assertEqual(status, 200, body)
        self.assertNotIn(VALUE.encode(), body)
        self.assertNotIn(b"LEAK", body)
        d = json.loads(body)
        self.assertEqual([g["owner"] for g in d["owners"]], ["acme", None])
        self.assertIn("API_KEY", [n["name"] for p in d["owners"][0]["projects"] for n in p["names"]])

    def test_only_a_same_origin_request_can_force_a_refresh(self):
        # #10350 fix round: a cross-site no-cors GET passes the Host guard; it must not skip the cache and run tm
        calls = []

        def counting(root):
            calls.append(root)
            return fake_runner(root)
        port = self._server(runner=counting)
        self._req(port)
        first = len(calls)
        self.assertGreater(first, 0)
        for headers in ({}, {"Sec-Fetch-Site": "cross-site"}, {"Origin": "https://evil.example"}):
            self.assertEqual(self._req(port, path="/api/projects/key-names?refresh=1", headers=headers)[0], 200)
        self.assertEqual(len(calls), first, "a cross-origin or header-less refresh ran tm")
        self._req(port, path="/api/projects/key-names?refresh=1", headers={"Sec-Fetch-Site": "same-origin"})
        self.assertEqual(len(calls), 2 * first)
        self._req(port, path="/api/projects/key-names?refresh=1", headers={"Origin": f"http://127.0.0.1:{port}"})
        self.assertEqual(len(calls), 3 * first)

    def test_route_is_architect_only_and_get_only(self):
        self.assertEqual(self._req(self._server(), method="POST")[0], 405)
        self.assertEqual(self._req(self._server(mode="session"))[0], 404)

    def test_page_renders_the_list_with_text_only(self):
        page = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html"), encoding="utf-8").read()
        block = page[page.index("/* BEGIN projectsKeyNames */"):page.index("/* END projectsKeyNames */")]
        self.assertIn('fetch("/api/projects/key-names"', block)
        self.assertIn('id="pj-keys"', page)
        self.assertNotIn("innerHTML", block)
        self.assertIn("ownerHref(g.key)", block)


if __name__ == "__main__":
    unittest.main()

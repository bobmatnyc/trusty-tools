"""The Projects view reads tm's project registry with no secrets module (#10571).

Covers: a fresh HOME holding the two files tm writes (~/.trusty-mpm/project-registry/projects.json, the ProjectStore
shape {"projects": {name: {name, repo_url, default_branch}}}, and ~/.trusty-mpm/project-paths.json, the
ProjectAliasEntry shape [{alias, path}]) lists every registered project; with no map row the canonical
<projects_root>/<owner>/<repo> clone is the root, a missing one carries a plain "Root not found" note, and a map path
wins; through registered_projects() and through
GET /api/projects on a live server; secrets_bridge is made unimportable for the whole test; TRUSTY_MPM_ROOT wins over
HOME; an absent registry is an empty list; a malformed one is a plain-English registry_error, never a 500.
Test: tests/test_dashboard_projects_registry.py
"""
import http.client
import importlib.util
import json
import os
import sys
import tempfile
import threading
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)
import dashboard_projects as dp  # noqa: E402


def write_registry(home, checkouts):
    """tm's two registry files under `home`/.trusty-mpm, in the shapes the Rust serializers write."""
    root = os.path.join(home, ".trusty-mpm")
    os.makedirs(os.path.join(root, "project-registry"))
    projects = {name: {"name": name, "repo_url": url, "default_branch": "main"} for name, url, _ in checkouts}
    with open(os.path.join(root, "project-registry", "projects.json"), "w") as f:
        json.dump({"projects": projects}, f)
    with open(os.path.join(root, "project-paths.json"), "w") as f:
        json.dump([{"alias": name, "path": path} for name, _, path in checkouts if path], f)
    return root


class NoBridge(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = os.path.join(tmp.name, "home")
        self.web = os.path.join(tmp.name, "src", "web")
        os.makedirs(self.web)
        self.checkouts = [("web", "https://github.com/acme/web.git", self.web),
                          ("api", "ssh://" + "git" + "@" + "github.com:acme/api.git", os.path.join(tmp.name, "src", "api")),
                          ("local-only", "file:///srv/local-only", None)]
        # A fresh HOME, no TRUSTY_MPM_ROOT, and secrets_bridge unimportable for the whole test.
        env = mock.patch.dict(os.environ, {"HOME": self.home, "TRUSTY_MPM_ROOT": ""})
        env.start()
        self.addCleanup(env.stop)
        mods = mock.patch.dict(sys.modules, {"secrets_bridge": None})
        mods.start()
        self.addCleanup(mods.stop)

    def test_registered_projects_lists_every_record_from_a_fresh_home(self):
        write_registry(self.home, self.checkouts)
        got = dp.registered_projects()
        self.assertEqual([(r["name"], r["owner"], r["repo"]) for r in got],
                         [("web", "acme", "web"), ("api", "acme", "api"), ("local-only", None, None)])
        self.assertEqual(got[0]["path"], self.web)
        self.assertIsNone(got[2]["path"])   # no map row and no GitHub URL: no root at all
        self.assertIsNone(got[2]["path_source"])

    def test_canonical_layout_and_root_not_found(self):
        # #10571 (a)(b)(c): no project map -> <projects_root>/<owner>/<repo>; a missing clone says so; a map path wins
        pr = os.path.join(self.home, "trusty-mpm-projects")
        os.makedirs(os.path.join(pr, "acme", "web"))
        os.makedirs(os.path.join(pr, "acme", "api"))
        mapped = os.path.join(self.home, "elsewhere", "api")
        os.makedirs(mapped)
        write_registry(self.home, [("web", "https://github.com/acme/web.git", None),
                                   ("gone", "https://github.com/acme/gone.git", None),
                                   ("api", "https://github.com/acme/api.git", mapped)])
        with mock.patch.dict(os.environ, {"ARCHITECT_PROJECTS_ROOT": pr, "ARCHITECT_CONFIG": ""}):
            reg = {r["name"]: r for r in dp.registered_projects()}
            self.assertEqual((reg["web"]["path"], reg["web"]["path_source"]), (os.path.join(pr, "acme", "web"), "canonical layout"))
            self.assertEqual((reg["api"]["path"], reg["api"]["path_source"]), (mapped, "project map"))
            out = {p["id"]: p for p in dp.build(list(reg.values()), {}, {}, {})["projects"]}
        self.assertTrue(out["web"]["scanned"])
        self.assertEqual(out["web"]["root_note"], "")
        self.assertEqual(out["api"]["root"], mapped)
        self.assertFalse(out["gone"]["scanned"])
        self.assertTrue(out["gone"]["root_note"].startswith("Root not found: the folder "), out["gone"]["root_note"])
        self.assertIn(os.path.join(pr, "acme", "gone"), out["gone"]["root_note"])

    def test_mixed_case_repo_url_links_the_session_in_the_lowercase_clone(self):
        # #10571: tm clones ACME/Widget_Shop.git to acme/widget-shop (slugify_component);
        # the derived root must be that directory, so a live pane inside it links the session
        pr = os.path.join(self.home, "trusty-mpm-projects")
        clone = os.path.join(pr, "acme", "widget-shop")
        os.makedirs(os.path.join(clone, "src"))
        write_registry(self.home, [("widget-shop", "https://github.com/ACME/Widget_Shop.git", None)])
        panes = {"tm-widget-shop": {"attached": True, "panes": [
            {"window": 0, "pane": 0, "path": os.path.join(clone, "src"), "command": "claude", "activity": "2026-10-31T02:30:00Z"}]}}
        with mock.patch.dict(os.environ, {"ARCHITECT_PROJECTS_ROOT": pr, "ARCHITECT_CONFIG": ""}):
            reg = dp.registered_projects()
            self.assertEqual(reg[0]["path"], clone)
            self.assertEqual((reg[0]["owner"], reg[0]["repo"]), ("ACME", "Widget_Shop"))   # display keeps the URL's case
            out = dp.build(reg, {}, panes, {"tm-widget-shop": "S07"})["projects"]
        self.assertTrue(out[0]["scanned"], out[0]["root_note"])
        self.assertEqual([(s["name"], s["id"]) for s in out[0]["sessions"]], [("tm-widget-shop", "S07")])

    def test_clone_slug_matches_tms_rule(self):
        cases = {"Example-Org-LLC": "example-org-llc", "Widget_Shop.git": "widget-shop", "a..b": "a-b",
                 "-Lead-": "lead", "trusty-tools": "trusty-tools", "___": ""}
        self.assertEqual({k: dp.clone_slug(k) for k in cases}, cases)

    def test_a_projects_list_instead_of_a_map_is_a_plain_error(self):
        # #10571: {"projects": [...]} is not tm's shape; the page is told, never handed a silent empty list
        root = os.path.join(self.home, ".trusty-mpm", "project-registry")
        os.makedirs(root)
        with open(os.path.join(root, "projects.json"), "w") as f:
            json.dump({"projects": [{"name": "web"}]}, f)
        with self.assertRaises(dp.RegistryError) as cm:
            dp.registered_projects()
        self.assertIn("unexpected shape", str(cm.exception))
        out = dp.ProjectsView(dp.registered_projects, lambda: {}, panes=lambda: {}).get({})
        self.assertTrue(out["registry_error"].startswith("The project list file has an unexpected shape"))

    def test_an_unreadable_record_keeps_its_row_with_a_note(self):
        root = os.path.join(self.home, ".trusty-mpm", "project-registry")
        os.makedirs(root)
        with open(os.path.join(root, "projects.json"), "w") as f:
            json.dump({"projects": {"broken": "not a record", "web": {"name": "web", "repo_url": "https://github.com/acme/web"}}}, f)
        reg = {r["name"]: r for r in dp.registered_projects()}
        self.assertEqual(sorted(reg), ["broken", "web"])
        out = {p["id"]: p for p in dp.build(list(reg.values()), {}, {}, {})["projects"]}
        self.assertFalse(out["broken"]["scanned"])
        self.assertEqual(out["broken"]["root_note"], "This project's entry in the project list is unreadable, so its folder is unknown.")

    def test_the_list_row_tags_a_missing_root(self):
        page = open(os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html"), encoding="utf-8").read()
        card = page[page.index("  function card(p) {"):page.index("  function detail() {")]
        self.assertIn('if (!p.scanned) { const t = el("span", "pj-chip pj-err", "Root not found"); t.title = p.root_note', card)

    def test_trusty_mpm_root_wins_over_home(self):
        other = os.path.join(self.home, "elsewhere")
        root = write_registry(other, self.checkouts[:1])
        with mock.patch.dict(os.environ, {"TRUSTY_MPM_ROOT": root}):
            self.assertEqual([r["name"] for r in dp.registered_projects()], ["web"])

    def test_absent_registry_is_empty_and_malformed_is_a_plain_error(self):
        self.assertEqual(dp.registered_projects(), [])
        root = os.path.join(self.home, ".trusty-mpm", "project-registry")
        os.makedirs(root)
        with open(os.path.join(root, "projects.json"), "w") as f:
            f.write("{not json")
        view = dp.ProjectsView(dp.registered_projects, lambda: {}, panes=lambda: {})
        out = view.get({})
        self.assertEqual(out["projects"], [])
        self.assertTrue(out["registry_error"].startswith("The project list could not be read"), out["registry_error"])

    def test_api_projects_lists_the_registry_on_a_live_server(self):
        write_registry(self.home, self.checkouts)
        spec = importlib.util.spec_from_file_location("dashboard_serve_registry", os.path.join(SCRIPTS, "dashboard-serve.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        page = os.path.join(self.home, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        view = dp.ProjectsView(dp.registered_projects, lambda: {}, panes=lambda: {})
        srv = mod.DashboardServer(0, mod.ds.Store(os.path.join(self.home, "store")), page, projects={}, projects_view=view)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        port = srv.server_address[1]
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        c.request("GET", "/api/projects", headers={"Host": f"127.0.0.1:{port}"})
        r = c.getresponse()
        body = json.loads(r.read())
        c.close()
        self.assertEqual(r.status, 200)
        self.assertFalse(body.get("registry_error"), body.get("registry_error"))
        self.assertEqual(sorted(p["id"] for p in body["projects"]), ["api", "local-only", "web"])


if __name__ == "__main__":
    unittest.main()

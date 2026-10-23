"""One dashboard page, two modes (owner 2026-10-19): architect (default) and session (one PM session's Intent Ledger).

Covers: /config.json in each mode, session mode never touching the default root (refusal, prune log and discovery
file stay under --root), the architect page and config unchanged, the page honouring the config (gates present,
absent config = architect), session items validating against schemas/dashboard with bare ids.
Run: python3 -m unittest scripts/test_dashboard_mode.py
Test: scripts/test_dashboard_mode.py
"""
import json
import os
import re
import sys
import tempfile
import threading
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import ServerCase, serve  # noqa: E402

PAGE = serve.PAGE
SESSION_CFG = serve.session_config("tm-apex Intent Ledger", "https://github.com/example/r/blob/main/")


def start(store, config=None, **kw):
    srv = serve.DashboardServer(0, store, PAGE, config=config, **kw)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


class TestConfig(ServerCase):
    def test_architect_mode_config_is_the_default(self):
        status, body = self.call("GET", "/config.json")
        self.assertEqual(status, 200)
        cfg = json.loads(body)
        self.assertIsInstance(cfg.pop("nav"), list)
        self.assertEqual(cfg, {"mode": "architect", "title": "Architect Dashboard", "briefs_base": "",
                               "storage_key": "architect-dashboard",
                               "briefs_abs": os.path.join(serve.ds.REPO, "local", "briefs") + "/", "tailnet": ""})

    def test_session_mode_config_carries_title_briefs_base_and_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            srv = start(ds.Store(os.path.join(tmp, "s")), SESSION_CFG)
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            import urllib.request
            with urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/config.json", timeout=5) as r:
                cfg = json.load(r)
        cfg.pop("nav")
        self.assertEqual(cfg, {"mode": "session", "title": "tm-apex Intent Ledger",
                               "briefs_base": "https://github.com/example/r/blob/main/", "storage_key": "tm-apex-intent-ledger",
                               "tailnet": ""})

    def test_session_flag_sets_default_title_and_stable_key(self):
        cfg = serve.session_config("", "b/", "tm-apex")
        self.assertEqual((cfg["title"], cfg["storage_key"]), ("tm-apex: IL Dashboard", "tm-apex-il-dashboard"))

    def test_title_wins_over_session_but_key_stays_session_based(self):
        cfg = serve.session_config("Custom", "b/", "tm-apex")
        self.assertEqual((cfg["title"], cfg["storage_key"]), ("Custom", "tm-apex-il-dashboard"))

    def test_neither_title_nor_session_defaults_to_il_dashboard(self):
        cfg = serve.session_config("", "b/")
        self.assertEqual((cfg["title"], cfg["storage_key"]), ("IL Dashboard", "il-dashboard"))

    def _nav(self, nav_file, config=None):
        import urllib.request
        with tempfile.TemporaryDirectory() as tmp:
            srv = serve.DashboardServer(0, ds.Store(os.path.join(tmp, "s")), PAGE, config=config, nav_file=nav_file)
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            with urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/config.json", timeout=5) as r:
                return json.load(r)["nav"]

    def test_config_nav_is_read_per_request_from_the_registry_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = os.path.join(tmp, "nav.json")
            with open(f, "w") as fh:
                json.dump([{"name": "Architect", "port": 7890}, {"name": "tm-apex", "port": 7891, "x": 1}, {"name": "bad"}], fh)
            self.assertEqual(self._nav(f), [{"name": "Architect", "port": 7890}, {"name": "tm-apex", "port": 7891}])
            self.assertEqual(self._nav(f, SESSION_CFG), [])   # ar-T44: session links only on the Architect dash

    def test_config_nav_missing_or_bad_file_is_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = os.path.join(tmp, "bad.json")
            with open(bad, "w") as fh:
                fh.write("{not json")
            self.assertEqual(self._nav(os.path.join(tmp, "absent.json")), [])
            self.assertEqual(self._nav(bad), [])

    def test_one_session_dropdown_replaces_nav_chips_and_il_filter(self):
        # owner 2026-10-21: one header dropdown beside the user-state control; no chip row, no IL "Session" select
        with open(os.path.join(serve.ds.REPO, "templates/dashboard/index.html"), encoding="utf-8") as fh:
            page = fh.read()
        for gone in ('id="il-nav"', "il-filter", 'id="il-bar"', "arch-il-link", "data-session-only", "BEGIN ilNav"):
            self.assertNotIn(gone, page, gone)
        self.assertEqual(page.count('id="sess-pick"'), 1)
        mid = page[page.index('<div class="hdr-mid">'):page.index('<div class="hdr-actions">')]
        self.assertLess(mid.index('id="sess-pick"'), mid.index('id="user-state-badge"'))   # same cluster as the user-state dropdown
        self.assertNotIn("data-arch-only", mid.split('<div class="state-wrap"')[0])      # visible in session mode too
        script = page[page.index("/* BEGIN sessionPicker"):page.index("/* END sessionPicker */")]
        self.assertIn('location.protocol + "//" + location.hostname + ":" + sel.value + "/"', script)   # same host: 127.0.0.1 and the tailnet name
        self.assertIn('"/config.json"', script)
        self.assertIn('out.unshift({ name: "Architect", port: ARCH })', script)   # a session page offers Architect without a nav
        self.assertIn("out.push({ name: self, port: here })", script)             # and shows itself selected
        self.assertIn("sel.value = here;", script)
        self.assertRegex(page, r"@media \(max-width: 800px\) \{ \.hdr-mid \{ justify-self: stretch")   # phone width

    def test_dropdown_options_and_navigation_run(self):
        import shutil
        import subprocess
        if not shutil.which("node"):
            self.skipTest("node not installed")
        with open(os.path.join(serve.ds.REPO, "templates/dashboard/index.html"), encoding="utf-8") as fh:
            page = fh.read()
        script = page[page.index("/* BEGIN sessionPicker"):page.index("/* END sessionPicker */")]
        harness = """
        function run(port, title, nav) {
          const opts = []; let handler = null;
          const sel = { value: "", set textContent(v) { opts.length = 0; }, appendChild(o) { opts.push(o); }, addEventListener(e, f) { handler = f; } };
          const location = { port: port, protocol: "https:", hostname: "host.tailnet.example.com", href: "" };
          const document = { title: title, getElementById: () => sel, createElement: () => ({}) };
          function XMLHttpRequest() { this.open = () => {}; this.send = function () { this.responseText = JSON.stringify({ nav: nav }); this.onload(); }; }
          new Function("document", "location", "XMLHttpRequest", SCRIPT)(document, location, XMLHttpRequest);
          const before = sel.value; sel.value = "7891"; handler();
          return { options: opts.map((o) => o.value + "=" + o.textContent), selected: before, href: location.href };
        }
        const nav = [{ name: "Architect", port: 7890 }, { name: "tm-apex", port: 7891 }];
        console.log(JSON.stringify([run("7890", "Architect Dashboard", nav), run("7891", "tm-apex: IL Dashboard", [])]));
        """
        r = subprocess.run(["node", "-e", "const SCRIPT = " + json.dumps(script) + ";" + harness], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        arch, sess = json.loads(r.stdout)
        self.assertEqual(arch["options"], ["7890=Architect", "7891=tm-apex"])
        self.assertEqual(arch["selected"], "7890")                                           # Architect page: Architect selected
        self.assertEqual(arch["href"], "https://host.tailnet.example.com:7891/")   # choosing a session opens its dashboard
        self.assertEqual(sess["options"], ["7890=Architect", "7891=tm-apex"])                # PM page: Architect offered, no nav served
        self.assertEqual(sess["selected"], "7891")                                           # and its own session selected
        self.assertEqual(sess["href"], "")                                                   # choosing the current page does not navigate

    def test_config_is_get_only(self):
        self.assertEqual(self.call("PUT", "/config.json", {"data": {}})[0], 405)


class TestSessionIsolation(unittest.TestCase):
    def test_main_refuses_the_architect_store_in_session_mode(self):
        with mock.patch.object(sys, "stderr") as err:
            rc = serve.main(["--mode", "session", "--port", "0", "--root", ds.DEFAULT_ROOT])
        self.assertEqual(rc, 2)
        self.assertIn("own --root", "".join(c.args[0] for c in err.write.call_args_list))

    def test_session_main_writes_only_under_its_root(self):
        before = _snapshot(ds.DEFAULT_ROOT), _snapshot(os.path.join(ds.REPO, "local", "owner-queue-archive"))
        with tempfile.TemporaryDirectory() as tmp:
            root = os.path.join(tmp, "store")
            data = os.path.join(tmp, "data")
            def once(self_):
                raise KeyboardInterrupt   # stop main() right after bind and seed

            with mock.patch.dict(os.environ, {"TRUSTY_DATA_DIR_OVERRIDE": data}), \
                    mock.patch.object(serve, "allowed_names", return_value=frozenset()), \
                    mock.patch.object(serve.DashboardServer, "serve_forever", once):
                rc = serve.main(["--mode", "session", "--port", "0", "--root", root, "--title", "T", "--briefs-base", "https://x/"])
            self.assertEqual(rc, 0)
            self.assertTrue(os.path.isfile(os.path.join(root, "meta", "state.json")))
            self.assertFalse(os.path.exists(data), "session mode must not publish the console discovery file")
            store = ds.Store(root, prune_log=os.path.join(root, ".archive", "pruned.jsonl"))
            store.set("items", "D01", {"status": "open", "title": "x"})
            store.delete("items", "D01")
            self.assertTrue(os.path.isfile(os.path.join(root, ".archive", "pruned.jsonl")))
            self.assertEqual(store.prune_log_path(), os.path.join(root, ".archive", "pruned.jsonl"))
            self.assertEqual(sorted(os.listdir(tmp)), ["store"])
        self.assertEqual(before, (_snapshot(ds.DEFAULT_ROOT), _snapshot(os.path.join(ds.REPO, "local", "owner-queue-archive"))))

    def test_session_server_serves_no_architect_docs_or_briefs(self):
        with tempfile.TemporaryDirectory() as tmp:
            srv = start(ds.Store(os.path.join(tmp, "s")), SESSION_CFG, projects={}, briefs=os.path.join(tmp, "s", ".no-briefs"))
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            import urllib.error
            import urllib.request
            for path in ("/doc/architect/ARCHITECT.md", "/files/local/briefs/x.md"):
                with self.assertRaises(urllib.error.HTTPError) as cm:
                    urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}{path}", timeout=5)
                self.assertEqual(cm.exception.code, 404, path)
                cm.exception.close()

    def test_session_store_validates_against_the_shared_schemas_with_bare_ids(self):
        item = {"kind": "question", "status": "open", "title": "Pick one", "session": "tm-apex", "goal": "G01", "rank": 1,
                "created": "2026-10-19T10:00:00Z", "updated": "2026-10-19T10:00:00Z"}
        self.assertEqual(dsch.validate("items", "D41", item), [])
        self.assertNotEqual(dsch.validate("items", "D41", {k: v for k, v in item.items() if k != "session"}), [])
        with tempfile.TemporaryDirectory() as tmp:
            store = ds.Store(os.path.join(tmp, "s"), fmt="jsonl", schema_mode="enforce")
            store.set("items", "D41", item)
            self.assertEqual(store.get("items", "D41")["data"]["title"], "Pick one")
            with self.assertRaises(ds.StoreError):
                store.set("items", "D42", {"status": "nonsense"})


def _snapshot(path):
    out = {}
    for d, _, files in os.walk(path):
        for f in files:
            p = os.path.join(d, f)
            st = os.stat(p)
            out[p] = (st.st_mtime_ns, st.st_size)
    return out


class TestPageGates(ServerCase):
    """The served page is the one shared file: architect mode renders as before, session mode is config-gated."""

    def setUp(self):
        super().setUp()
        with open(PAGE, encoding="utf-8") as f:
            self.page_src = f.read()

    def test_architect_pane_markup_is_still_in_the_page_and_only_gated(self):
        for needle in ('id="user-state-badge"', 'class="console-link"', 'id="sec-sessions"', 'id="sec-accounts"',
                       'id="set-tz"', 'id="pool-gd"', "<h1>Architect Dashboard</h1>", "<title>Architect Dashboard</title>"):
            self.assertIn(needle, self.page_src)
        self.assertEqual(self.page_src.count(" data-arch-only"), 10)   # 10 = 7 + sec-il (Df7) + sec-visualize + the Live link (ar-Te8)
        self.assertIn('html[data-mode="session"] [data-arch-only] { display: none !important; }', self.page_src)

    def test_page_reads_config_json_and_falls_back_to_architect(self):
        m = re.search(r"/\* BEGIN modeConfig \*/(.*?)/\* END modeConfig \*/", self.page_src, re.S)
        self.assertIsNotNone(m)
        block = m.group(1)
        self.assertIn('"/config.json"', block)
        self.assertIn('mode: "architect"', block)
        self.assertIn("catch (_)", block)

    def test_every_session_edit_is_conditional(self):
        g = 'typeof SESSION !== "undefined" && SESSION'
        for needle in ("if (!SESSION) { renderSessions(); renderAccounts(); }", 'if (!SESSION) db.collection("sessions")',
                       'if (!SESSION) db.collection("accounts")', 'if (!SESSION) db.doc("meta/settings")',
                       "if (!(%s) && q.session)" % g, "if (!(%s) && it.session)" % g, "if (!(%s) && m &&" % g,
                       '"architect-dashboard") + "-open"', '"architect-dashboard") + "-seen"', "if (!SESSION) {\n    /* Named window",
                       "window.DocReader.open = (project, path) =>"):
            self.assertIn(needle, self.page_src, needle)
        self.assertIn("sessions", self.page_src)   # code stays; only rendering is conditional
        self.assertIn("function renderSessions", self.page_src)

    def test_served_architect_page_is_the_file_plus_the_shim_only(self):
        with mock.patch.object(serve, "PAGE", PAGE):
            srv = start(ds.Store(self.root), None)
            self.addCleanup(srv.server_close)
            self.addCleanup(srv.shutdown)
            import urllib.request
            with urllib.request.urlopen(f"http://127.0.0.1:{srv.server_address[1]}/", timeout=5) as r:
                served = r.read().decode("utf-8")
        rev = serve.page_rev(PAGE)
        self.assertEqual(served, serve.inject(self.page_src, rev))


if __name__ == "__main__":
    unittest.main()

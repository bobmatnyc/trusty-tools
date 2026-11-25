"""Watch / Unwatch over a real dashboard-serve handler (#10896, AC1 and AC5).

Covers: AC1, a Watch POST to /api/watch_changes (and the GET /api/projects that follows) leaves local/watch.json byte-
and mtime-identical and creates exactly one `new` request, with the list present and with it absent (absent stays
absent). AC5, the page's own watchState code, run in node over the dashboard-serve shim (tests/shim_harness.js "page")
against the live server: Retry turns a failed request back to new, Dismiss sets it to dismissed and the row then reads
Not watched, and a write pinned to a stale version is a 409 that changes nothing. Skips the node tests when node is absent.
Test: tests/test_dashboard_watch_http.py
"""
import http.client
import importlib.util
import json
import os
import re
import shutil
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

import dashboard_projects as dp
import dashboard_store as ds

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
PAGE = os.path.join(os.path.dirname(HERE), "templates", "dashboard", "index.html")
NODE = shutil.which("node")


def load_serve():
    spec = importlib.util.spec_from_file_location("dashboard_serve_watch", os.path.join(SCRIPTS, "dashboard-serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


serve = load_serve()


class Live(unittest.TestCase):
    """A registry with one project `web`, an Architect dir, and a dashboard-serve on an ephemeral port."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.arch = os.path.join(self.tmp, "arch")
        os.makedirs(self.arch)
        mpm = os.path.join(self.tmp, "mpm")
        os.makedirs(os.path.join(mpm, "project-registry"))
        self.web = os.path.join(self.tmp, "src", "web")
        os.makedirs(self.web)
        with open(os.path.join(mpm, "project-registry", "projects.json"), "w") as f:
            json.dump({"projects": {"web": {"name": "web", "repo_url": "https://github.com/acme/web.git"}}}, f)
        with open(os.path.join(mpm, "project-paths.json"), "w") as f:
            json.dump([{"alias": "web", "path": self.web}], f)
        env = mock.patch.dict(os.environ, {"TRUSTY_MPM_ROOT": mpm, "ARCHITECT_CONFIG": ""})
        env.start()
        self.addCleanup(env.stop)
        page = os.path.join(self.tmp, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body>x</body></html>")
        self.store = ds.Store(os.path.join(self.tmp, "store"))
        view = dp.ProjectsView(dp.registered_projects, lambda: {}, panes=lambda: {}, lifecycle=lambda: {}, arch_dir=self.arch)
        srv = serve.DashboardServer(0, self.store, page, projects={}, projects_view=view)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        self.port = srv.server_address[1]

    def call(self, method, path, body=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {"Host": "127.0.0.1:%d" % self.port}
        if body is not None:
            headers.update({"Content-Type": "application/json", "Origin": "http://127.0.0.1:%d" % self.port})
        c.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
        r = c.getresponse()
        out = (r.status, json.loads(r.read()))
        c.close()
        return out

    def row(self):
        status, body = self.call("GET", "/api/projects")
        self.assertEqual(status, 200, body)
        return {p["id"]: p for p in body["projects"]}["web"]

    def write_watch(self, keys):
        os.makedirs(os.path.join(self.arch, "local"), exist_ok=True)
        with open(os.path.join(self.arch, "local", "watch.json"), "w") as f:
            json.dump({"version": 1, "projects": [{"key": k} for k in keys]}, f)


class WatchPostLeavesTheListAlone(Live):
    """AC1."""

    def post_watch(self):
        req = {"key": "web", "action": "watch", "status": "new", "created": "2026-11-10T21:00:00.000Z", "by": "owner"}
        status, body = self.call("POST", "/api/watch_changes", {"data": req})
        self.assertEqual(status, 201, body)
        self.assertEqual(self.row()["watch"]["label"], "Watch pending")
        new = [d for d in self.store.list_docs("watch_changes") if d["data"]["status"] == "new"]
        self.assertEqual(len(new), 1)
        self.assertEqual(new[0]["data"]["key"], "web")

    def test_with_the_list_present_its_bytes_and_mtime_are_unchanged(self):
        self.write_watch(["api"])
        path = os.path.join(self.arch, "local", "watch.json")
        before = (open(path, "rb").read(), os.stat(path).st_mtime_ns)
        self.post_watch()
        self.assertEqual((open(path, "rb").read(), os.stat(path).st_mtime_ns), before)

    def test_with_the_list_absent_it_stays_absent(self):
        self.assertEqual(self.row()["watch"]["label"], "watch list not created yet")
        self.post_watch()
        self.assertFalse(os.path.exists(os.path.join(self.arch, "local", "watch.json")))
        self.assertFalse(os.path.exists(os.path.join(self.arch, "local")))


@unittest.skipUnless(NODE, "node is not installed")
class PageRetryAndDismiss(Live):
    """AC5: the page's watchState functions over the shim, against this server."""

    def setUp(self):
        super().setUp()
        self.write_watch([])
        self.store.set("watch_changes", "W1", {"key": "web", "action": "watch", "status": "failed", "by": "owner",
                                               "created": "2026-11-10T21:00:00.000Z", "done_at": "2026-11-10T21:01:00.000Z",
                                               "message": "exit 1: no such project"}, if_version=0)

    def page(self, body):
        src = open(PAGE, encoding="utf-8").read()
        region = re.search(r"/\* BEGIN watchState \*/(.*?)/\* END watchState \*/", src, re.S).group(1)
        shim = re.search(r"<script>(.*?)</script>", serve.SHIM, re.S).group(1)
        d = os.path.join(self.tmp, "node")
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "shim.js"), "w") as f:
            f.write(shim)
        with open(os.path.join(d, "code.js"), "w") as f:
            f.write(region + "\nconst row = async () => (await (await fetch('/api/projects')).json()).projects.find((p) => p.id === 'web');\n" + body)
        r = subprocess.run([NODE, os.path.join(HERE, "shim_harness.js"), os.path.join(d, "shim.js"), "page",
                            "http://127.0.0.1:%d" % self.port, os.path.join(d, "code.js")], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout.strip().splitlines()[-1])

    def test_retry_returns_a_failed_request_to_new(self):
        out = self.page("async function main(db) { const p = await row(); const before = watchLabel(p);"
                        " await watchRetry(db, p.watch.request); return { before: before, after: watchLabel(await row()) }; }")
        self.assertEqual(out, {"before": "Watch failed", "after": "Watch pending"})
        data = self.store.get("watch_changes", "W1")["data"]
        self.assertEqual(data["status"], "new")
        self.assertNotIn("message", data)   # the old outcome is cleared; the applier writes a fresh one

    def test_dismiss_sets_dismissed_and_the_row_reads_not_watched(self):
        out = self.page("async function main(db) { const p = await row(); const before = watchLabel(p);"
                        " await watchDismiss(db, p.watch.request); return { before: before, after: watchLabel(await row()) }; }")
        self.assertEqual(out, {"before": "Watch failed", "after": "Not watched"})
        data = self.store.get("watch_changes", "W1")["data"]
        self.assertEqual((data["status"], data["message"]), ("dismissed", "exit 1: no such project"))

    def test_a_stale_version_is_a_409_and_changes_nothing(self):
        out = self.page("async function main(db) { const p = await row();"
                        " await db.collection('watch_changes').doc('W1').update({ message: 'changed meanwhile' });"
                        " try { await watchDismiss(db, p.watch.request); return { status: 0 }; } catch (e) { return { status: e.status }; } }")
        self.assertEqual(out, {"status": 409})
        data = self.store.get("watch_changes", "W1")["data"]
        self.assertEqual((data["status"], data["message"]), ("failed", "changed meanwhile"))


if __name__ == "__main__":
    unittest.main()

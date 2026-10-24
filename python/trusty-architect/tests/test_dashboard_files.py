"""dashboard-serve.py GET /files/local/briefs/<rel> (read-only, briefs dir only) and the page's
linkSplit() helper (run under node; skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
from test_dashboard_store import serve  # noqa: E402

PAGES = [os.path.join(os.path.dirname(HERE), p) for p in
         ("templates/dashboard/index.html",)]
ABS = "/srv/architect/local/briefs/"


class TestFilesRoute(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        t = tmp.name
        self.briefs = os.path.join(t, "local", "briefs")
        os.makedirs(os.path.join(self.briefs, "sub"))
        with open(os.path.join(self.briefs, "a.md"), "w") as f:
            f.write("# brief\n")
        with open(os.path.join(self.briefs, "sub", "b.txt"), "w") as f:
            f.write("nested")
        with open(os.path.join(t, "secret.md"), "w") as f:
            f.write("SECRET")
        os.symlink(os.path.join(t, "secret.md"), os.path.join(self.briefs, "link.md"))
        os.symlink(t, os.path.join(self.briefs, "linkdir"))
        page = os.path.join(t, "index.html")
        with open(page, "w") as f:
            f.write("<html><head></head><body></body></html>")
        self.srv = serve.DashboardServer(0, ds.Store(os.path.join(t, "oq")), page, self.briefs)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)

    def get(self, path, method="GET"):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.headers.get("Content-Type"), r.read()
        except urllib.error.HTTPError as e:
            return e.code, None, e.read()

    def test_allowed_files_served_as_plain_text(self):
        st, ct, body = self.get("/files/local/briefs/a.md")
        self.assertEqual((st, ct, body), (200, "text/plain; charset=utf-8", b"# brief\n"))
        self.assertEqual(self.get("/files/local/briefs/sub/b.txt")[2], b"nested")

    def test_escapes_directories_and_missing_are_404(self):
        for p in ("/files/local/briefs/../secret.md", "/files/local/briefs/%2e%2e/secret.md",
                  "/files/local/briefs/sub/" + "../" * 2 + "secret.md",   # #10161: built; see test_dashboard_doc "/files/etc/passwd", "/files/local/briefs/",
                  "/files/local/briefs/sub", "/files/local/briefs/nope.md", "/files/local/briefs/link.md",
                  "/files/local/briefs/linkdir/secret.md", "/files/local/briefs/a.md%00.png",
                  "/files//etc/passwd", "/files/local/briefs//etc/passwd"):
            self.assertEqual(self.get(p)[0], 404, p)

    def test_writes_refused(self):
        self.assertEqual(self.get("/files/local/briefs/a.md", "POST")[0], 405)
        self.assertEqual(self.get("/files/local/briefs/a.md", "DELETE")[0], 405)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestLinkSplit(unittest.TestCase):
    def split(self, page, text):
        src = open(page, encoding="utf-8").read()
        m = re.search(r"/\* BEGIN linkSplit \*/(.*?)/\* END linkSplit \*/", src, re.S)
        js = m.group(1) + "\nconsole.log(JSON.stringify(linkSplit(" + json.dumps(text) + ")));"
        return json.loads(subprocess.run(["node", "-e", js], capture_output=True, text=True, check=True).stdout)

    def test_briefs_path_and_url_become_links_other_text_stays_plain(self):
        text = f"See {ABS}2026-10-08-x.md. Also https://github.com/example/b/pull/1, and <b>/etc/passwd</b>"
        for page in PAGES:
            parts = self.split(page, text)
            self.assertEqual("".join(p["t"] for p in parts), text.replace("2026-10-08-x.md.", "2026-10-08-x.md."))
            links = [p for p in parts if "href" in p]
            self.assertEqual([p["href"] for p in links],
                             ["/files/local/briefs/2026-10-08-x.md", "https://github.com/example/b/pull/1"])
            self.assertTrue(all("/etc/passwd" not in p["t"] for p in links))

    def test_path_outside_briefs_is_not_linked(self):
        for page in PAGES:
            parts = self.split(page, "/srv/architect/local/state.md and " + ABS)
            self.assertEqual([p for p in parts if "href" in p], [])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestTaskSection(unittest.TestCase):
    def run_js(self, page, tail):
        src = open(page, encoding="utf-8").read()
        js = "".join(re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1)
                     for n in ("askKind", "partitionItems")) + tail
        return json.loads(subprocess.run(["node", "-e", js], capture_output=True, text=True, check=True).stdout)

    def test_tasks_leave_asks_and_have_their_own_section(self):
        items = [{"id": i, "kind": k} for i, k in (("D01", "question"), ("A02", "action"), ("T03", "task"),
                                                  ("R4", "action"), ("A05", "action"), ("T06", "task"))]
        for page in PAGES:
            out = self.run_js(page, "console.log(JSON.stringify(partitionItems(%s, [], [%s])));" %
                              (json.dumps(items[:5]), json.dumps(items[5])))
            ids = lambda k: [i["id"] for i in out[k]]
            self.assertEqual((ids("q"), ids("a"), ids("t"), ids("tx")), (["D01"], ["A02", "A05"], ["T03", "R4"], ["T06"]))
            out = self.run_js(page, "console.log(JSON.stringify(partitionItems([{id:'D1',kind:'question'}],"
                              "[{id:'D2',kind:'question'}],[{id:'D3',kind:'question'}])));")
            self.assertEqual(([i["id"] for i in out["q"]], [i["id"] for i in out["qc"]]), (["D1"], ["D2", "D3"]))
            src = open(page, encoding="utf-8").read()
            self.assertLess(src.index('id="sec-asks"'), src.index('id="sec-tasks"'))
            self.assertLess(src.index('id="sec-tasks"'), src.index('id="sec-goals"'))
            self.assertIn("<h2>Tasks</h2>", src)

    def test_decided_asks_and_tasks_go_to_completed(self):
        for page in PAGES:
            out = self.run_js(page, "console.log(JSON.stringify(partitionItems([{id:'A1',kind:'action'}],"
                              "[{id:'A2',kind:'action'},{id:'T2',kind:'task'}],[{id:'A3',kind:'action'},{id:'R3',kind:'action'},{id:'T3',kind:'task'}])));")
            ids = lambda k: [i["id"] for i in out[k]]
            self.assertEqual((ids("a"), ids("ac"), ids("tc"), ids("t")), (["A1"], ["A2", "A3"], ["T2", "R3", "T3"], []))

    def test_completed_block_filters_and_pages_newest_first(self):
        items = [{"id": "D%02d" % n, "title": "t%d" % n, "answered_at": "2026-10-%02dT00:00:00Z" % n} for n in range(1, 26)]
        items.append({"id": "D90", "title": "Odd Title", "status": "done", "done_at": "2026-10-30T00:00:00Z", "answer_note": "Needle"})
        items.append({"id": "D91", "title": "x", "answered_at": "2026-10-02T00:00:00Z", "done_at": "2026-10-31T00:00:00Z"})
        call = "console.log(JSON.stringify((() => { const r = completedPage(%s, %s, %d, 20); return [r.rows.map((i) => i.id), r.page, r.pages, r.total, r.from, r.to]; })()));"
        for page in PAGES:
            def run(q, pg):
                src = open(page, encoding="utf-8").read()
                js = "function txt(v){return v==null?'':typeof v==='string'?v:String(v);}" + re.search(
                    r"/\* BEGIN completedPage \*/(.*?)/\* END completedPage \*/", src, re.S).group(1) + call % (json.dumps(items), json.dumps(q), pg)
                return json.loads(subprocess.run(["node", "-e", js], capture_output=True, text=True, check=True).stdout)
            ids, pg, pages, total, lo, hi = run("", 0)
            self.assertEqual((ids[:3], len(ids), pages, total, lo, hi), (["D91", "D90", "D25"], 20, 2, 27, 1, 20))
            ids, pg, pages, total, lo, hi = run("", 1)
            self.assertEqual((len(ids), lo, hi, ids[-1]), (7, 21, 27, "D01"))
            self.assertEqual(run("", 9)[1], 1)   # page clamps into range
            self.assertEqual(run("NEEDLE", 0)[0], ["D90"])   # answer_note, case-insensitive
            self.assertEqual(run("d2", 0)[3], 6)   # id match: D02, D20-D25
            self.assertEqual(run("zzz", 0)[3:], [0, 0, 0])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestOwnerState(unittest.TestCase):
    """Needs-owner shows only while an open item naming the session has blocks_session true."""

    def states(self, page, items, stored):
        src = open(page, encoding="utf-8").read()
        js = re.search(r"/\* BEGIN ownerState \*/(.*?)/\* END ownerState \*/", src, re.S).group(1)
        js += "const b = blockedSessions(%s); console.log(JSON.stringify(%s.map((s) => ownerState(s, b.has('pm')))));" % (
            json.dumps(items), json.dumps(stored))
        return json.loads(subprocess.run(["node", "-e", js], capture_output=True, text=True, check=True).stdout)

    def test_only_open_blocking_item_sets_needs_owner(self):
        stored = ["working", "idle", "waiting-owner", "blocked", "parked"]
        cases = [
            ([{"status": "open", "session": "pm"}], ["working", "idle", "idle", "blocked", "parked"]),
            ([{"status": "open", "session": "pm", "blocks_session": False}], ["working", "idle", "idle", "blocked", "parked"]),
            ([{"status": "open", "session": "pm", "blocks_session": True}],
             ["waiting-owner", "waiting-owner", "waiting-owner", "blocked", "parked"]),
            ([{"status": "answered", "session": "pm", "blocks_session": True}], ["working", "idle", "idle", "blocked", "parked"]),
        ]
        for page in PAGES:
            for items, want in cases:
                self.assertEqual(self.states(page, items, stored), want, (page, items))

    def test_blocked_is_not_in_the_owner_group(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('{ key: "owner", states: ["waiting-owner"],', src)
            self.assertRegex(src, r'key: "waiting", states: \["blocked", "waiting-ci", "idle"\]')


if __name__ == "__main__":
    unittest.main()

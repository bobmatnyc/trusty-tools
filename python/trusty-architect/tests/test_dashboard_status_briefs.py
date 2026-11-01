"""Task status briefs (the owner 2026-10-30): `status_briefs` [{id, at, brief}] on a T item, newest first in the page, prepended by
`dashboard-db.py status-brief`, checked by the items schema; the old status text shows only when there is no brief.

Test: tests/test_dashboard_status_briefs.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

from test_dashboard_progress import FAKE, PAGES

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
DB = os.path.join(SCRIPTS, "dashboard-db.py")
REF = "cto/docs/briefs/2026-10-30-t1c-jira-linear-status.md"


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestStatusBriefsPage(unittest.TestCase):
    def js(self, body):
        src = open(PAGES[0], encoding="utf-8").read()
        part = re.search(r"/\* BEGIN statusBriefs \*/(.*?)/\* END statusBriefs \*/", src, re.S).group(1)
        r = subprocess.run(["node", "-e", FAKE + part + body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_newest_first_regardless_of_stored_order_and_label(self):
        out = self.js("""
        const it = { status_briefs: [{ id: "MB0001", at: "2026-10-28T10:00:00Z", brief: "p/a.md" }, { id: "MB0003", at: "2026-10-30T12:57:08Z", brief: "p/c.md" },
          { id: "MB0002", at: "2026-10-29T09:00:00Z", brief: "p/b.md" }, { id: "", at: "2026-10-31T00:00:00Z", brief: "p/x.md" }, { id: "MB0009", at: "2026-10-31T00:00:00Z" }] };
        const l = statusBriefsOf(it);
        console.log(JSON.stringify({ ids: l.map((x) => x.id), label: statusBriefLabel(l[0]), none: statusBriefsOf({}).length, bad: statusBriefsOf({ status_briefs: "x" }).length }));""")
        self.assertEqual(out, {"ids": ["MB0003", "MB0002", "MB0001"], "label": "MB0003 \u00B7 2026-10-30", "none": 0, "bad": 0})

    def test_page_wiring_summary_list_and_text_fallback(self):
        src = open(PAGES[0], encoding="utf-8").read()
        self.assertIn('askKind(it) === "T" ? statusBriefsOf(it)[0] : null', src)
        self.assertIn("const sb = statusBriefSlot(it); if (sb) sum.appendChild(sb);", src)   # collapsed + expanded summary
        task = src[src.index("  function renderTask(it)"):src.index("  function renderDone(it)")]
        self.assertLess(task.index("statusBriefsOf(it)"), task.index("else if (txt(it.status_note))"))
        self.assertIn("window.DocReader.open(d.project, d.path)", src[src.index("function statusBriefLink"):])


class TestStatusBriefCli(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = dict(os.environ, DASHBOARD_ROOT=self.tmp.name)
        self.db("set", "items", "T01", "--json", json.dumps({"kind": "task", "status": "open", "title": "x", "session": "tm-a-01", "steps": [{"id": "1", "title": "Next: x"}]}))

    def tearDown(self):
        self.tmp.cleanup()

    def db(self, *a):
        return subprocess.run([sys.executable, DB, "--root", self.tmp.name, *a], capture_output=True, text=True, env=self.env)

    def test_append_puts_newest_at_front_and_repeat_id_moves(self):
        for i in ("MB0001", "MB0002"):
            r = self.db("status-brief", "T01", i, REF)
            self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.db("status-brief", "T01", "MB0001", REF).returncode, 0)
        doc = json.loads(self.db("get", "items", "T01").stdout)
        sb = doc["data"]["status_briefs"]
        self.assertEqual([e["id"] for e in sb], ["MB0001", "MB0002"])
        self.assertTrue(all(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", e["at"]) and e["brief"] == REF for e in sb))
        self.assertEqual(doc["data"]["title"], "x")

    def test_refuses_bad_id_bad_ref_and_missing_item(self):
        self.assertNotEqual(self.db("status-brief", "T01", "XX1", REF).returncode, 0)
        self.assertNotEqual(self.db("status-brief", "T01", "MB0001", "no-slash.txt").returncode, 0)
        self.assertNotEqual(self.db("status-brief", "T99", "MB0001", REF).returncode, 0)
        self.assertNotIn("status_briefs", json.loads(self.db("get", "items", "T01").stdout)["data"])

    def test_schema_accepts_shape_and_rejects_extra_keys(self):
        import dashboard_schema as dsch
        base = {"kind": "task", "status": "open", "title": "x", "session": "tm-a-01", "goal": "G01", "rank": 1, "created": "2026-10-28T09:00:00Z", "updated": "2026-10-28T09:00:00Z"}
        errs = lambda d: dsch.validate_store({"items": [{"id": "T01", "data": d}]})
        ok = {"id": "MB0be9", "at": "2026-10-30T12:57:08Z", "brief": REF}
        self.assertEqual(errs(dict(base, status_briefs=[ok])), [])
        self.assertTrue(errs(dict(base, status_briefs=[dict(ok, x=1)])))


if __name__ == "__main__":
    unittest.main()

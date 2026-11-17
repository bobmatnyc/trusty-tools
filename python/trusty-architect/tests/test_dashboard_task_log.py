"""Task rows: "<session>: <title>" at render time, `log` field (CLI append, schema, page), MB brief ids on the media rows (the owner 2026-10-28).

Test: tests/test_dashboard_task_log.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

from test_dashboard_progress import FAKE

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
ROOT = os.path.dirname(HERE)
TEMPLATE = os.path.join(ROOT, "templates/dashboard/index.html")
DB = os.path.join(SCRIPTS, "dashboard-db.py")


class TestTaskLogCli(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.env = dict(os.environ, DASHBOARD_ROOT=self.tmp.name)
        self.run_db("set", "items", "T01", "--json", json.dumps({"kind": "task", "status": "open", "title": "x", "session": "tm-a-01", "steps": [{"id": "1", "title": "Next: x"}]}))

    def tearDown(self):
        self.tmp.cleanup()

    def run_db(self, *a):
        return subprocess.run([sys.executable, DB, "--root", self.tmp.name, *a], capture_output=True, text=True, env=self.env)

    def test_log_appends_entries_and_bumps_version(self):
        r1 = self.run_db("log", "items", "T01", "first win")
        self.assertEqual(r1.returncode, 0, r1.stderr)
        r2 = self.run_db("log", "items", "T01", "second win")
        self.assertEqual(r2.returncode, 0, r2.stderr)
        doc = json.loads(self.run_db("get", "items", "T01").stdout)
        log = doc["data"]["log"]
        self.assertEqual([e["text"] for e in log], ["first win", "second win"])
        self.assertTrue(all(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", e["at"]) for e in log))
        self.assertEqual(doc["data"]["title"], "x")
        self.assertEqual(doc["version"], 3)

    def test_log_refuses_empty_text_missing_item_and_other_collections(self):
        self.assertEqual(self.run_db("log", "items", "T01", "  ").returncode, 2)
        self.assertEqual(self.run_db("log", "items", "T99", "x").returncode, 1)
        self.assertEqual(self.run_db("log", "goals", "G01", "x").returncode, 2)

    def test_schema_allows_log_and_rejects_extra_keys(self):
        import dashboard_schema as dsch
        base = {"kind": "task", "status": "open", "title": "x", "session": "tm-a-01", "goal": "G01", "rank": 1, "created": "2026-10-28T09:00:00Z", "updated": "2026-10-28T09:00:00Z"}
        errs = lambda d: dsch.validate_store({"items": [{"id": "T01", "data": d}]})
        self.assertEqual(errs(base), [])
        self.assertEqual(errs(dict(base, log=[{"at": "2026-10-28T10:00:00Z", "text": "done a"}])), [])
        self.assertTrue(errs(dict(base, log=[{"at": "2026-10-28T10:00:00Z", "text": "a", "x": 1}])))


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestTaskLogPage(unittest.TestCase):
    def eval(self, body):
        src = open(TEMPLATE, encoding="utf-8").read()
        ak = re.search(r"/\* BEGIN askKind \*/(.*?)/\* END askKind \*/", src, re.S).group(1)
        fns = []
        for n in ("rowTitle", "logOf"):
            fns.append(re.search(r"  function %s\(.*?\n  }\n" % n, src, re.S).group(0))
        fns.append(re.search(r"  const briefRef = .*\n", src).group(0))
        js = FAKE + "\n" + ak + "\n" + "\n".join(fns) + "\n" + body
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_row_title_prefixes_tasks_once_and_leaves_other_kinds(self):
        out = self.eval("""console.log(JSON.stringify([
          rowTitle({id: "T01", session: "tm-jira-linear-01", title: "JIRA to Linear migration"}),
          rowTitle({id: "T02", session: "tm-jira-linear-01", title: "tm-jira-linear-01: already"}),
          rowTitle({id: "T03", title: "no session"}),
          rowTitle({id: "D01", session: "tm-x-01", title: "a decision"})]));""")
        self.assertEqual(out, ["tm-jira-linear-01: JIRA to Linear migration", "tm-jira-linear-01: already", "no session", "a decision"])

    def test_log_is_newest_first_and_skips_blank_lines(self):
        out = self.eval("""console.log(JSON.stringify(logOf({log: [{at: "2026-10-27T01:00:00Z", text: "old"}, {at: "2026-10-28T01:00:00Z", text: "new"}, {at: "x", text: ""}]}).map((e) => e.text)));""")
        self.assertEqual(out, ["new", "old"])

    def test_brief_ref_shows_legacy_b_as_mb_and_keeps_mb(self):
        out = self.eval("""console.log(JSON.stringify([briefRef("B09d7"), briefRef("MB0a00"), briefRef("B1f")]));""")
        self.assertEqual(out, ["MB09d7", "MB0a00", "B1f"])


if __name__ == "__main__":
    unittest.main()

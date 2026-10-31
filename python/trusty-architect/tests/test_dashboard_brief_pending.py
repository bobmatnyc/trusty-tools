"""Asking for a brief clears the item from the owner's open lists until the brief is linked (the owner 2026-10-26).

Why: an item waiting on a brief is not the owner's to act on; it should not count as open or sit in the main list.
What: the button writes status brief_pending with brief_request; bucketItems keeps it out of open and counts it apart;
tally and summary do not count it open; pending-answers and the hook still list the brief_request; a write that links
`brief` (dashboard-db.py update, the API, any Store write) restores status open, and an unrelated write does not.
Test: this file.
"""
import io
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout

import dashboard_derive as derive
import dashboard_store as ds
import owner_answers
from test_dashboard_item_brief import run as run_brief
from test_dashboard_progress import FAKE, PAGES
from test_dashboard_interactive import load


class Base(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = tmp.name
        self.store = ds.Store(self.root)
        base = {"kind": "question", "title": "Pick one", "created": "2026-10-26T00:00:00Z", "updated": "2026-10-26T00:00:00Z"}
        self.store.set("items", "D01", dict(base, status="open"))
        self.store.set("items", "D02", dict(base, status="brief_pending", brief_request={"at": "2026-10-26T16:00:00Z", "by": "the owner"}))


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestPage(unittest.TestCase):
    def test_request_writes_brief_pending(self):
        body = """(async () => { const body = el("div"); const it = { id: "A02", status: "open" };
        briefControl(body, it); body.find((n) => n.tagName === "button")[0].listeners.click(); await new Promise((r) => setTimeout(r, 20));
        console.log(JSON.stringify([written.patch.status, written.patch.brief_request.by, written.opts])); })();"""
        for page in PAGES:
            self.assertEqual(run_brief(page, body), ["brief_pending", "owner", {"if_version": 7}])

    def test_brief_pending_is_not_open_and_is_counted_apart(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            bucket = re.search(r"  function bucketItems\(list\) \{.*?\n  \}\n", src, re.S).group(0)
            askk = re.search(r"/\* BEGIN askKind \*/(.*?)/\* END askKind \*/", src, re.S).group(1)
            js = FAKE + askk + bucket + """
const list = [{ id: "D01", status: "open" }, { id: "D02", status: "brief_pending" }, { id: "A03", status: "brief_pending" }];
const b = bucketItems(list);
console.log(JSON.stringify({ open: b.open.map((i) => i.id), briefing: b.briefing.map((i) => i.id), done: b.done.length, answered: b.answered.length }));"""
            r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(json.loads(r.stdout), {"open": ["D01"], "briefing": ["D02", "A03"], "done": 0, "answered": 0})
            self.assertIn('" awaiting brief"', src)


class TestStore(Base):
    def test_tally_and_summary_do_not_count_it_open(self):
        t = derive.tally(self.store.list_docs("items"))
        self.assertEqual((t["open"], t["questions"]), (1, 1))

    def test_pending_lists_brief_request_while_pending(self):
        got = [p for p in owner_answers.pending(self.root) if p["kind"] == "brief_request"]
        self.assertEqual([(g["id"], g["text"]) for g in got], [("D02", "Pick one")])

    def test_pending_answers_cli_and_hook_still_list_it(self):
        pa = load("pa", "pending-answers.py")
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = pa.main(["--root", self.root])
        self.assertEqual(rc, 1)
        self.assertIn("D02\tbrief_request", buf.getvalue())
        hook = load("hk", "pending_hook_dedupe.py")
        line = hook.decide(owner_answers.pending(self.root), os.path.join(self.root, "st.json"), "s", now=1000.0)
        self.assertIn("D02 (brief_request)", line)

    def test_linking_the_brief_restores_open_and_stops_the_report(self):
        self.store.update("items", "D02", {"brief": "architect/docs/briefs/2026-10-26-x.md"})
        d = self.store.get("items", "D02")["data"]
        self.assertEqual((d["status"], d["brief"]), ("open", "architect/docs/briefs/2026-10-26-x.md"))
        self.assertEqual([p for p in owner_answers.pending(self.root) if p["kind"] == "brief_request"], [])
        self.assertEqual(derive.tally(self.store.list_docs("items"))["open"], 2)

    def test_unrelated_writes_and_bad_paths_keep_it_pending(self):
        self.store.update("items", "D02", {"detail": "more"})
        self.store.update("items", "D02", {"brief": "not a path"})
        self.assertEqual(self.store.get("items", "D02")["data"]["status"], "brief_pending")
        self.store.update("items", "D01", {"brief": "architect/docs/briefs/x.md"})   # an open item is untouched by the rule
        self.assertEqual(self.store.get("items", "D01")["data"]["status"], "open")

    def test_dashboard_db_update_restores(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("ddb", os.path.join(os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"), "dashboard-db.py"))
        db = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(db)
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = db.main(["--root", self.root, "update", "items", "D02", "--json", '{"brief": "architect/docs/briefs/y.md"}'])
        self.assertEqual(rc, 0)
        self.assertEqual(self.store.get("items", "D02")["data"]["status"], "open")


if __name__ == "__main__":
    unittest.main()

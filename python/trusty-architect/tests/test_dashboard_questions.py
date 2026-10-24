"""Dashboard Questions section (owner 2026-10-14 04:15Z), checked on both HTML files (node, no browser) plus the store side.

Why: the Architect's answers to the owner's questions were lost in scrolling and context clears; they now sit in a
Questions section below Decisions until the owner clears them.
What: section order; pure helpers (mdSplit, questionLists, clearPatch, restorePatch); the row render; Clear and Restore
write `status` through the update path with if_version pinned; schema; Q id counter; tally suffix.
Test: scripts/test_dashboard_questions.py
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_derive as derive  # noqa: E402
import dashboard_schema as schema  # noqa: E402
from test_dashboard_progress import FAKE, PAGES  # noqa: E402

spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)

ISO = "2026-10-14T04:30:00Z"


def run(page, body, whole=False):
    src = open(page, encoding="utf-8").read()
    pure = re.search(r"/\* BEGIN questions \*/(.*?)/\* END questions \*/", src, re.S).group(1)
    pure += re.search(r"/\* BEGIN docPaths \*/(.*?)/\* END docPaths \*/", src, re.S).group(1)
    pure += re.search(r"/\* BEGIN planView \*/(.*?)/\* END planView \*/", src, re.S).group(1)   # groupSources / groupInto for grouped links
    ui = ""
    if whole:
        ui = src[src.index("  let questions = [];"):src.index('  $("ques-clear-all").addEventListener')]
    js = FAKE + r"""
const readerButton = (label, fn) => { const b = el("button", "read-btn", label); b.listeners.click = fn; return b; };
const refRows = new Map(), bindOpen = () => {}, utc = (s) => txt(s), errBox = (q) => el("div", "err");
const idTitle = (id, title) => el("span", "title", id + " · " + title);
const linkify = (p, t) => { p.appendChild(el("span", null, t)); return p; };
const docOf = (r) => { const m = /^([^/]+)\/(docs\/briefs\/.+\.md)$/.exec(txt(r)); return m ? { project: m[1], path: m[2] } : null; };
const fillList = (box, nodes) => { box.children = nodes; };
let readOnly = false, written = [];
const window = { DocReader: { open: () => {} } };
const rows = { "list-ques": el("div"), "list-quz": el("div") };
const $ = (id) => rows[id] || el("span");
const db = { collection: () => ({ doc: (id) => ({ get: async () => ({ version: 5 }), update: async (p, o) => { written.push([id, p, o]); } }) }) };
""" + pure + "\n" + ui + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


class TestSections(unittest.TestCase):
    def test_questions_section_sits_below_decisions_above_asks(self):
        for page in PAGES:
            s = open(page, encoding="utf-8").read()
            a, b, c = (s.index(f'id="{i}"') for i in ("sec-decisions", "sec-questions", "sec-asks"))
            self.assertTrue(a < b < c, page)
            for needle in ("No answered questions.", 'id="ques-clear-all"', 'id="sub-quz"', 'db.collection("questions").onSnapshot', "renderQuestions();"):
                self.assertIn(needle, s, page)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestQuestionsPage(unittest.TestCase):
    def each(self, body, whole=False):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body, whole)

    def test_pure_helpers(self):
        body = """
        const docs = [
          { id: "Q00", status: "open", answered_at: "2026-10-14T01:00:00Z" },
          { id: "Q01", status: "open", answered_at: "2026-10-14T03:00:00Z" },
          { id: "Q02", status: "cleared", cleared_at: "2026-10-14T02:00:00Z" }];
        for (let i = 0; i < 25; i++) docs.push({ id: "Q" + (16 + i).toString(16), status: "cleared", cleared_at: "2026-10-13T00:" + String(i).padStart(2, "0") + ":00Z" });
        const L = questionLists(docs);
        console.log(JSON.stringify([L.open.map((q) => q.id), L.cleared.length, L.cleared[0].id,
          mdSplit("see [the doc](https://x.io/a?b=1) now, [bad](javascript:alert(1))"),
          clearPatch(new Date(0)), restorePatch(new Date(0))]));"""
        for r in self.each(body):
            self.assertEqual(r[0], ["Q01", "Q00"])           # newest answered_at first, cleared docs excluded
            self.assertEqual(r[1], 20)                        # Cleared bucket keeps the last 20
            self.assertEqual(r[2], "Q02")
            self.assertEqual(r[3][1], {"t": "the doc", "href": "https://x.io/a?b=1"})
            self.assertTrue(all("href" not in p or p["href"].startswith("https://") for p in r[3]))
            self.assertEqual(r[3][2]["t"], " now, [bad](javascript:alert(1))")   # a non-http target stays text
            self.assertEqual(r[4], {"status": "cleared", "cleared_at": "1970-01-01T00:00:00.000Z", "updated": "1970-01-01T00:00:00.000Z"})
            self.assertEqual(r[5], {"status": "open", "cleared_at": "", "updated": "1970-01-01T00:00:00.000Z"})

    def test_row_render_and_clear_restore_writes(self):
        body = """
        questions = [
          { id: "Q00", status: "open", asked: "where is the flyr doc?", answer: "It is [here](https://docs.google.com/d/1).", answered_at: "2026-10-14T04:20:00Z",
            links: [{ label: "Doc", url: "https://docs.google.com/d/1" }, { label: "bad", url: "javascript:x" }], brief: "architect/docs/briefs/flyr.md" },
          { id: "Q01", status: "cleared", asked: "old", answer: "a", answered_at: "2026-10-14T01:00:00Z", cleared_at: "2026-10-14T02:00:00Z" }];
        renderQuestions();
        const open = rows["list-ques"].children, cleared = rows["list-quz"].children;
        const anchors = open[0].find((n) => n.tagName === "a");
        const btn = (card, label) => card.find((n) => n.tagName === "button" && n.textContent === label)[0];
        btn(open[0], "Clear").listeners.click();
        btn(cleared[0], "Restore").listeners.click();
        setTimeout(() => console.log(JSON.stringify([open.length, cleared.length, open[0].textContent.includes("where is the flyr doc?"),
          anchors.map((a) => [a.href, a.target, a.rel]), btn(open[0], "Read brief") ? 1 : 0, written])), 10);"""
        for r in self.each(body, whole=True):
            self.assertEqual((r[0], r[1], r[2]), (1, 1, True))
            self.assertEqual(r[3], [["https://docs.google.com/d/1", "_blank", "noopener"]] * 2)   # answer link + Doc button; javascript: dropped
            self.assertEqual(r[4], 1)
            (cid, cp, co), (rid, rp, ro) = r[5]
            self.assertEqual((cid, cp["status"], co), ("Q00", "cleared", {"if_version": 5}))
            self.assertTrue(cp["cleared_at"].endswith("Z"))
            self.assertEqual((rid, rp["status"], ro), ("Q01", "open", {"if_version": 5}))

    def test_open_question_card_is_not_faded(self):
        for page in PAGES:
            self._check_question_card_class(open(page, encoding="utf-8").read())

    def _check_question_card_class(self, src):
        m = re.search(r'function renderQuestionDoc\(q, cleared\) \{\s*const card = el\("details", ([^;]*)\);', src)
        self.assertIsNotNone(m)
        self.assertIn('(cleared ? " faded cleared" : "")', m.group(1))
        self.assertNotIn("item faded", m.group(1))

    def test_empty_state(self):
        body = 'questions = []; renderQuestions(); console.log(JSON.stringify([rows["list-ques"].children.length]));'
        for r in self.each(body, whole=True):
            self.assertEqual(r, [0])


class TestStoreSide(unittest.TestCase):
    def doc(self, **kw):
        return dict({"asked": "q?", "answer": "a", "status": "open", "answered_at": ISO, "updated": ISO}, **kw)

    def test_schema_accepts_and_rejects(self):
        ok = self.doc(links=[{"label": "Doc", "url": "https://x.io"}], brief="architect/docs/briefs/x.md", cleared_at="", session="tm-x", asked_at=ISO)
        self.assertEqual(schema.validate("questions", "Q00", ok), [])
        for bad in (self.doc(status="done"), self.doc(links=[{"label": "d", "url": "javascript:x"}]), self.doc(extra=1)):
            self.assertTrue(schema.validate("questions", "Q00", bad))

    def test_q_counter(self):
        def snap(ids):
            return {"questions": [{"id": i, "data": {}} for i in ids]}
        self.assertEqual(derive.next_id(snap(["Q00", "Q01"]), "Q"), "Q02")
        self.assertEqual(derive.next_id({"questions": []}, "Q"), "Q00")
        self.assertEqual(derive.next_id(snap(["Q00"]), "D"), "D00")   # other prefixes unaffected


def _put(root, coll, doc_id, **data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def test_ids_cli_prints_q_line():
    r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "dashboard_ids.py")], capture_output=True, text=True, timeout=30,
                       env=dict(os.environ, DASHBOARD_ROOT="/nonexistent-q-test-root"))
    assert any(line.startswith("Q\tnext=") for line in r.stdout.splitlines()), r.stdout + r.stderr


def test_tally_suffix_only_when_answers_wait(tmp_path, capsys):
    _put(tmp_path, "items", "D01", kind="question", status="open")
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "1 open dashboard item: 1 question, 0 actions, 0 tasks\n"   # unchanged when none
    _put(tmp_path, "questions", "Q00", status="open")
    _put(tmp_path, "questions", "Q01", status="open")
    _put(tmp_path, "questions", "Q02", status="cleared")
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "1 open dashboard item: 1 question, 0 actions, 0 tasks, 2 answers waiting\n"


if __name__ == "__main__":
    unittest.main()

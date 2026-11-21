"""Lists and counts before their data loads (#11588, the owner: "Don't say how many tasks until they've loaded").

Why: render() runs on any collection's snapshot, so a list whose own collection had not answered yet showed 0 and its
empty-state text ("The Architect has no open tasks.") under a spinning "Loading…".
What: in node, the real renderAsks/renderQuestions/renderGoals/renderSessions/renderAccounts run before their
collection loads and paint a loading mark, never 0 and never empty text; a failed first load paints "!" and an error;
the nav sums keep a mark instead of adding it to 0; start() marks each collection loaded or failed.
Run: python3 -m pytest tests/test_dashboard_loading.py
Test: tests/test_dashboard_loading.py
"""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_live_progress import fn_src
from test_dashboard_progress import FAKE, PAGES

RENDERS = {
    "renderAsks": ("items", ["n-decisions", "n-asks", "n-tasks", "n-qa", "n-aa", "n-qx", "n-ax", "n-tx", "n-qp", "n-ap", "n-tp"], ["list-q", "list-a", "list-t", "list-tx"]),
    "renderQuestions": ("questions", ["n-questions", "n-quz", "n-qpd"], ["list-qpd", "list-ques", "list-quz"]),
    "renderGoals": ("goals", ["n-goals", "n-gd", "n-gs", "n-gf", "n-gp", "n-gc"], ["list-gd", "list-gs", "list-gf"]),
    "renderSessions": ("sessions", ["n-sessions"], ["sessions-empty"]),
    "renderAccounts": ("accounts", ["n-accounts"], ["accounts-body"]),
}
EMPTY_TEXT = ["no open tasks", "Nothing to decide", "No blockers", "No answered questions", "No directed goals", "No sessions recorded", "No usage readings"]


def run(page, body):
    src = open(page, encoding="utf-8").read()
    m = re.search(r"/\* BEGIN loadState \*/(.*?)/\* END loadState \*/", src, re.S)
    assert m, "loadState block missing"
    fns = "\n".join(fn_src(src, n) for n in RENDERS)
    js = FAKE + r"""
N.prototype.replaceChildren = function (...cs) { this.children = cs; this._t = ""; };
N.prototype.classList = null;
const nodes = {};
const $ = (id) => { if (!nodes[id]) { const n = new N("span"); n.classList = { remove() {} }; nodes[id] = n; } return nodes[id]; };
""" + m.group(1) + "\n" + fns + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestLoading(unittest.TestCase):
    def paint(self, setup):
        chips = sorted({c for v in RENDERS.values() for c in v[1]})
        lists = sorted({c for v in RENDERS.values() for c in v[2]})
        body = setup + "\n" + "".join(f"{n}();\n" for n in RENDERS) + \
            "console.log(JSON.stringify([%s, %s]));" % (
                "{" + ",".join(f'"{c}": $("{c}").textContent' for c in chips) + "}",
                "{" + ",".join(f'"{c}": $("{c}").textContent' for c in lists) + "}")
        return run(PAGES[0], body)

    def test_loading_lists_show_a_mark_not_zero_or_empty_text(self):
        chips, lists = self.paint("")
        for c, v in chips.items():
            self.assertEqual(v, "·", c)
        for c, v in lists.items():
            self.assertEqual(v, "Loading…", c)
            for e in EMPTY_TEXT:
                self.assertNotIn(e, v, c)

    def test_failed_load_shows_an_error_not_zero_or_empty_text(self):
        chips, lists = self.paint('["items", "questions", "goals", "sessions", "accounts"].forEach((n) => { loadFailed[n] = "denied"; });')
        for c, v in chips.items():
            self.assertEqual(v, "!", c)
        for c, v in lists.items():
            self.assertIn("Could not load", v, c)
            self.assertNotIn("0", v.replace("(denied)", ""), c)

    def test_a_loaded_empty_list_shows_zero_and_its_empty_text(self):
        """Once the items have loaded, an empty list is a real 0 and says so."""
        stubs = r"""
loaded.items = true;
let items = [], decisionSort = "rank", undone = 0;
const comp = { qx: {}, ax: {}, tx: {}, qa: {}, aa: {}, ta: {} }, bucketItems = () => ({ open: [], answered: [], done: [], briefing: [] });
const partitionItems = () => ({ q: [], qa: [], qx: [], a: [], aa: [], ax: [], t: [], ta: [], tx: [] });
const renderParked = () => {}, decisionList = (l) => l, sortKey = () => 0, decisionNodes = () => [];
const fillList = (box, nodes, emptyText) => { box.replaceChildren(); if (!nodes.length) box.appendChild(el("p", "empty", emptyText)); };
const nl = () => ({}), paneFilter = () => () => true, qhit = () => true, renderCompleted = () => {};
const undoneCount = () => 0, autoCollapse = () => {}, askKind = () => "T";
const renderQuestion = () => null, renderAction = () => null, renderTask = () => null;
renderAsks();
console.log(JSON.stringify([$("n-tasks").textContent, $("list-t").textContent]));"""
        out = run(PAGES[0], stubs)
        self.assertEqual(out[0], "0")
        self.assertIn("no open tasks", out[1])

    def test_nav_sum_keeps_the_mark(self):
        out = run(PAGES[0], 'console.log(JSON.stringify([addN(0, "\\u00b7"), addN(2, 3), addN(1, "!"), srcMark("items"), (loaded.items = true, srcMark("items"))]));')
        self.assertEqual(out, ["·", 5, "!", "·", None])


class TestWiring(unittest.TestCase):
    def test_static_markup_has_no_empty_state_text(self):
        """Before the first render every list placeholder reads "Loading…", never a list's empty text (#11588)."""
        src = open(PAGES[0], encoding="utf-8").read()
        stale = [t for t in re.findall(r'<p class="empty"[^>]*>([^<]*)</p>', src) if t and t != "Loading\u2026"]   # the hidden, blank #main-empty is no list
        self.assertEqual(stale, [])

    def test_start_marks_each_collection_loaded_or_failed(self):
        src = open(PAGES[0], encoding="utf-8").read()
        for name in ("items", "questions", "goals", "sessions", "accounts"):
            self.assertIn('db.collection("%s").onSnapshot(live(got("%s",' % (name, name), src)
        self.assertIn("const m = srcMark(CAT_SRC[c.key]);", src)   # nav and tab counts take the mark too


if __name__ == "__main__":
    unittest.main()

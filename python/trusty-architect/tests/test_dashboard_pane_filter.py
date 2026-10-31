"""Pane filter (the owner 2026-10-30): a text box at the top of each main pane filters only that pane's items, across every
status (done included), while the nav search (bar.q / qhit) keeps checking every type. Logic is run under node (skipped
when node is absent); the wiring is asserted against the page source."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "paneFilter", "partitionItems")]
    bucket = re.search(r"  function bucketItems\(list\) \{.*?\n  \}\n", src, re.S).group(0)
    js = FAKE + "\nconst OPEN_KEY = 'k'; const localStorage = { getItem: () => null, setItem: () => {} };\n" + "\n".join(parts) + bucket + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


ITEMS = """
const items = [
  { id: "D01", kind: "D", status: "open", title: "Pick the database", detail: "Postgres or Dynamo", session: "tm-apex-01" },
  { id: "D02", kind: "D", status: "done", title: "Old naming call", answer: "use kebab", session: "tm-apex-01" },
  { id: "D03", kind: "D", status: "answered", title: "Retention", answer: "30 days" },
  { id: "T01", kind: "T", status: "open", title: "Port dashboard", notes: "kebab cleanup pending", session: "tm-joanies-01" },
  { id: "T02", kind: "T", status: "done", title: "Ship kebab fix", session: "tm-joanies-01" },
  { id: "A01", kind: "A", status: "wont_do", title: "Sign the form", wont_do: { note: "Not needed" } }
];
"""


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardPaneFilter(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, ITEMS + body)

    def test_matches_id_text_notes_and_session_case_insensitively(self):
        body = """
        const hit = (q) => { paneQ.tasks = q; return items.filter(paneFilter("tasks")).map((i) => i.id); };
        console.log(JSON.stringify({ id: hit("t01"), text: hit("PORT dash"), notes: hit("Cleanup"), sess: hit("JOANIES"), none: hit("zzz"), empty: hit("  ").length }));"""
        for r in self.each(body):
            self.assertEqual(r, {"id": ["T01"], "text": ["T01"], "notes": ["T01"], "sess": ["T01", "T02"], "none": [], "empty": 6})

    def test_includes_done_and_answered_items_of_the_pane(self):
        body = """
        paneQ.decisions = "kebab";
        const { open, answered, done } = bucketItems(items);
        const P = partitionItems(open, answered, done), f = paneFilter("decisions");
        console.log(JSON.stringify({ open: P.q.filter(f).map((i) => i.id), done: P.qx.filter(f).map((i) => i.id), ans: P.qa.filter(f).map((i) => i.id) }));"""
        for r in self.each(body):
            self.assertEqual(r, {"open": [], "done": ["D02"], "ans": []})

    def test_a_pane_query_does_not_reach_other_panes(self):
        body = """
        paneQ.decisions = "kebab";
        const t = items.filter(paneFilter("tasks")).length, a = items.filter(paneFilter("asks")).length, d = items.filter(paneFilter("decisions")).map((i) => i.id);
        console.log(JSON.stringify({ t, a, d }));"""
        for r in self.each(body):
            self.assertEqual(r, {"t": 6, "a": 6, "d": ["D02", "T01", "T02"]})   # D02/T01/T02 mention kebab; tasks/asks panes are unfiltered

    def test_wiring_and_nav_filter_unchanged(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            # nav filter: same predicate, still reading bar.q across every type
            self.assertIn('const qhit = (r) => !qOn() || recText(r).indexOf(bar.q.trim().toLowerCase()) >= 0;', src)
            self.assertIn("comp.qx.list = P.qx.filter(qhit).filter(pD)", src)   # both filters compose on the done lists
            self.assertIn("if (!off && q) { const rec = recFor(r); off = rec ? !qhit(rec)", src)
            self.assertIn("off = rec ? !paneHit(rec, pq)", src)
            # one box per main pane, Esc clears, state saved in a try/catch
            for sec in ("sec-decisions", "sec-questions", "sec-asks", "sec-tasks", "sec-goals", "sec-sessions"):
                self.assertIn('"%s"]' % sec, src)
            self.assertIn('e.key === "Escape" && inp.value', src[src.index("function buildPaneFilters"):])
            self.assertRegex(src, r"function paneSave\(\) \{ try \{.*?localStorage\.setItem\(PANE_KEY.*?catch")
            # the tab choice is ignored while a pane filter is on, so every status shows
            self.assertIn('sub = raw === "all" || pq ? null : raw', src)
            self.assertIn("buildPaneFilters();\n  navRender();", src)

    def test_styles_use_tokens_and_mobile_rule(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            css = re.search(r"\.pane-filter \{.*?\n\.cbar input\[type=search\] \{", src, re.S).group(0)
            self.assertIn("var(--surface-1)", css)
            self.assertNotRegex(css, r"#[0-9a-fA-F]{3,6}\b")
            self.assertIn(".pane-filter input.pf-input, .pane-filter .pf-clear, .cbar input[type=search]", src)


if __name__ == "__main__":
    unittest.main()

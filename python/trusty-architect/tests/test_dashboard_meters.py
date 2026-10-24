"""Account usage meters (owner 2026-10-09 15:3xZ, "Graphs are still backwards"): the bar fill and the text
both show percent USED (pct5h/pctwk are percent used), capped at 100. Checked on both HTML files via
renderAccounts, so a second render path or a stale copy fails here."""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def render(page, docs):
    with open(page, encoding="utf-8") as fh:
        src = fh.read()
    acc = re.search(r"/\* BEGIN accounts \*/(.*?)/\* END accounts \*/", src, re.S).group(1)
    lt = re.search(r"/\* BEGIN localTime \*/(.*?)/\* END localTime \*/", src, re.S).group(1)
    fns = [re.search(r"  function %s\(.*?\n  }\n" % f, src, re.S).group(0) for f in ("meter", "renderAccounts")]
    js = FAKE + """
const ACCT_BEEP_PCT = 95;
N.prototype.replaceChildren = function () { this.children = []; };
const nodes = {}; const $ = (id) => (nodes[id] = nodes[id] || el("div")); const accounts = %s;
""" % json.dumps(docs) + lt + acc + "\n".join(fns) + """
renderAccounts();
const out = $("accounts-body").find((n) => n.className.startsWith("meter")).map((m) => ({
  text: m.children[0].textContent, fill: m.find((n) => n.tagName === "progress").map((p) => [p.value, p.max]) }));
console.log(JSON.stringify(out));"""
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardMeters(unittest.TestCase):
    def test_fill_and_text_show_percent_used(self):
        docs = [{"account": "a@example.com", "pct5h": 102, "pctwk": 92, "ts": "2026-10-09T15:00:00Z"}]
        for page in PAGES:
            with self.subTest(page=page):
                five, week = render(page, docs)
                self.assertEqual(week["text"], "Week: 92% used")
                self.assertEqual(week["fill"], [[92, 100]])     # 92 used = 92% full
                self.assertEqual(five["text"], "5-hour: 100% used")
                self.assertEqual(five["fill"], [[100, 100]])    # 102 is capped at full

    def test_zero_used_is_an_empty_bar(self):
        for page in PAGES:
            five, week = render(page, [{"account": "b@example.com", "pct5h": 0, "pctwk": 0}])
            self.assertEqual((five["fill"], week["fill"]), ([[0, 100]], [[0, 100]]))


if __name__ == "__main__":
    unittest.main()

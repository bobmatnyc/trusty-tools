"""Open decisions render oldest first (run under node, skipped when node is absent), checked on both HTML files."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardDecisionOrder(unittest.TestCase):
    def test_open_decisions_sorted_oldest_first_and_wired_to_decisions_only(self):
        body = """
        const txt = (v) => (v == null ? "" : String(v));
        const byRank = (a, b) => txt(a.rank).localeCompare(txt(b.rank));
        const items = [
          { id: "D3", created: "2026-10-09T01:00:00Z", rank: "1" },
          { id: "D1", created: "2026-10-07T01:00:00Z", rank: "9" },
          { id: "D2b", created: "2026-10-08T01:00:00Z", rank: "2" },
          { id: "D2a", created: "2026-10-08T01:00:00Z", rank: "1" },
        ];
        console.log(JSON.stringify(items.sort(oldestFirst).map((i) => i.id)));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                part = re.search(r"/\* BEGIN decisionOrder \*/(.*?)/\* END decisionOrder \*/", src, re.S).group(1)
                r = subprocess.run(["node", "-e", body.split("const items")[0] + part + "\nconst items" + body.split("const items")[1]],
                                   capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(json.loads(r.stdout), ["D1", "D2a", "D2b", "D3"])
                self.assertIn("const q = P.q.sort(oldestFirst)", src)
                self.assertIn("const a = P.a.sort(sortKey)", src)

    def test_page_clears_window_opener_on_load(self):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                part = re.search(r"/\* BEGIN clearOpener \*/(.*?)/\* END clearOpener \*/", src, re.S).group(1)
                body = "const window = { opener: { name: 'console' } };\n" + part + "\nconsole.log(JSON.stringify(window.opener));"
                r = subprocess.run(["node", "-e", body], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(json.loads(r.stdout), None)
                # A throwing setter must not break page load.
                body = "const window = { get opener() { return 1; }, set opener(v) { throw new Error('ro'); } };\n" + part + "\nconsole.log('ok');"
                r = subprocess.run(["node", "-e", body], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()

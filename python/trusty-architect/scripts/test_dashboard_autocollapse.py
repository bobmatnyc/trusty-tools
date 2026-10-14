"""Dashboard section auto-collapse (run under node, skipped when node is absent), checked on both HTML files:
a section collapses when its open count goes from at least 1 to 0; it never expands and never collapses on first load."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    part = re.search(r"/\* BEGIN autoCollapse \*/(.*?)/\* END autoCollapse \*/", src, re.S).group(1)
    r = subprocess.run(["node", "-e", part + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardAutoCollapse(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body)

    def test_last_open_item_leaving_collapses_the_section(self):
        # Parked, done, answered, cancelled and wont_do all reduce the open count the same way.
        body = """
        const d = { open: true };
        const out = [];
        out.push(autoCollapse("decisions", 1, d), d.open);
        out.push(autoCollapse("decisions", 0, d), d.open);
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, [False, True, True, False])

    def test_other_open_items_keep_it_open_and_nothing_reexpands(self):
        body = """
        const d = { open: true }; const out = [];
        autoCollapse("asks", 3, d); out.push(autoCollapse("asks", 2, d), d.open);
        d.open = false; out.push(autoCollapse("asks", 1, d), d.open);
        out.push(autoCollapse("asks", 0, d), d.open);       // already collapsed by the viewer: no error, stays collapsed
        out.push(autoCollapse("asks", 2, d), d.open);       // a new open item never expands it
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, [False, True, False, False, False, False, False, False])

    def test_first_load_never_collapses_and_sections_are_independent(self):
        body = """
        const a = { open: true }, b = { open: true }; const out = [];
        out.push(autoCollapse("decisions", 0, a), a.open);   // first count seen is 0: keep the remembered state
        autoCollapse("tasks", 2, b);
        out.push(autoCollapse("decisions", 0, a), a.open, autoCollapse("tasks", 0, b), b.open);
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, [False, True, False, True, True, False])

    def test_page_calls_it_for_the_three_sections(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            for key, sec in (("decisions", "sec-decisions"), ("asks", "sec-asks"), ("tasks", "sec-tasks")):
                self.assertRegex(src, r'autoCollapse\("%s", \w+\.length, \$\("%s"\)\)' % (key, sec))


if __name__ == "__main__":
    unittest.main()

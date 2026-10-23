"""Dashboard factory goals (node, no browser), on both HTML files.

owner 2026-10-11 04:19Z: goals.kind adds "factory"; a factory goal links sessions to a project board.
Page: FACTORY badge, board link, repo, own list between Directed and Standing.

Test: scripts/test_dashboard_factory_goal.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    part = re.search(r"/\* BEGIN factory \*/(.*?)/\* END factory \*/", src, re.S).group(1)
    r = subprocess.run(["node", "-e", FAKE + part + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestFactoryGoal(unittest.TestCase):
    def test_kind_and_board_href(self):
        body = """console.log(JSON.stringify([goalKind("factory"), goalKind("standing"), goalKind("directed"), goalKind(undefined), goalKind("bogus"),
          boardHref({ url: " https://github.com/example/x/projects/1 " }), boardHref({ url: "javascript:alert(1)" }), boardHref({}), boardHref(null)]));"""
        for p in PAGES:
            self.assertEqual(run(p, body), ["factory", "standing", "directed", "directed", "directed",
                                            "https://github.com/example/x/projects/1", "", "", ""])

    def test_pages_wire_factory_list_between_directed_and_standing(self):
        for p in PAGES:
            s = open(p, encoding="utf-8").read()
            for needle in ('id="list-gf"', 'id="err-gf"', 'chip factory-chip', '"Board"', '"Repo"', 'goalsOf("factory")', 'renderGoal(g, 0, 1, "factory")'):
                self.assertIn(needle, s, (os.path.basename(os.path.dirname(p)), needle))
            self.assertLess(s.index('id="sub-gd"'), s.index('id="sub-gf"'))
            self.assertLess(s.index('id="sub-gf"'), s.index('id="sub-gs"'))


if __name__ == "__main__":
    unittest.main()

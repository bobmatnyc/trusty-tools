"""Dashboard goal park / unpark and the session-goal links (node, no browser), on both HTML files.

owner 2026-10-11 04:09Z: goals can be parked and unparked. 04:12Z: goals link to sessions; parking a session
that an unparked goal depends on asks for confirmation first.

Test: scripts/test_dashboard_goal_park.py
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
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("park", "goalPark")]
    js = FAKE + "\n".join(parts) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestGoalPark(unittest.TestCase):
    def each(self, body):
        return [run(p, body) for p in PAGES]

    def test_park_patch_dates_and_until_unparked(self):
        body = """const now = new Date(2026, 9, 11, 4, 0, 0);
        const t = goalParkPatch("tomorrow", "owner", now), u = goalParkPatch("unparked", "owner", now);
        console.log(JSON.stringify({ t, u, tu: new Date(t.parked.until).getDate(), un: goalUnparkPatch("owner", now) }));"""
        for r in self.each(body):
            self.assertEqual(r["tu"], 12)
            self.assertEqual(r["t"]["parked"]["choice"], "owner")
            self.assertIsNone(r["t"]["park_ack"])
            self.assertIn("park_ack", r["t"])
            self.assertIsNone(r["u"]["parked"]["until"])
            self.assertEqual(r["u"]["parked"]["by"], "owner")
            self.assertIsNone(r["un"]["parked"])
            self.assertIn("park_ack", r["un"])
            self.assertEqual(r["un"]["unparked_by"], "owner")

    def test_parked_flag_and_text(self):
        body = """console.log(JSON.stringify([goalParked({}), goalParked({ parked: null }), goalParked({ parked: { until: null } }),
          goalParkText({ parked: { until: null } }), goalParkText({ parked: { until: "2026-10-12T09:00:00Z" } }).indexOf("parked until ") === 0]));"""
        for r in self.each(body):
            self.assertEqual(r, [False, False, True, "parked until unparked", True])

    def test_serving_goals_skip_parked_and_removed_and_confirm_text(self):
        body = """const gs = [{ id: "G01", title: "Top", sessions: ["tm-dogfood", "tm-x"] },
          { id: "G02", title: "Parked", sessions: ["tm-dogfood"], parked: { until: null } },
          { id: "G03", title: "Gone", status: "removed", sessions: ["tm-dogfood"] },
          { id: "G04", title: "By id", sessions: ["S05"] }, { id: "G05", title: "Other", sessions: ["tm-y"] }, { id: "G06" }];
        const a = goalsServing({ id: "S01", name: "tm-dogfood" }, gs), b = goalsServing({ id: "S05", name: "tm-z" }, gs), c = goalsServing({ id: "S09", name: "tm-none" }, gs);
        console.log(JSON.stringify([a.map((g) => g.id), b.map((g) => g.id), c.length, parkConfirmText("tm-dogfood", a)]));"""
        for r in self.each(body):
            self.assertEqual(r, [["G01"], ["G04"], 0, "tm-dogfood serves G01 Top. Park it anyway?"])


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestGoalParkWiring(unittest.TestCase):
    def test_pages_wire_park_confirm_and_links(self):
        for p in PAGES:
            s = open(p, encoding="utf-8").read()
            for needle in ('id="sub-gp"', "goalParkControl(g, kind)", "parkConfirmText(s.name, deps)", 'addKV(kv, "Goals"',
                           'kind: "pconf"', "parkedGoals()", 'db.collection("goals")'):
                self.assertIn(needle, s, (os.path.basename(os.path.dirname(p)), needle))


if __name__ == "__main__":
    unittest.main()

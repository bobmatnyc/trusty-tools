"""Tasks tabs on the dashboard page (owner 2026-10-22 17:0xZ, ar-Tc2).

Tasks show All, In progress, Parked, Done and no Answered tab; an answered task counts as in progress.
Decisions and Actions keep their Answered tab.

Test: scripts/test_dashboard_task_tabs.py
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = open(os.path.join(ROOT, "templates/dashboard/index.html"), encoding="utf-8").read()


def nav_entry(key):
    return re.search(r'\{ key: "%s",[^\n]*' % key, SRC).group(0)


class TestTaskTabs(unittest.TestCase):
    def test_tasks_nav_order_has_no_answered(self):
        subs = re.findall(r'\["(\w+)", "([^"]+)"', nav_entry("tasks").split("subs:")[1])
        self.assertEqual([s[1] for s in subs], ["In progress", "Parked", "Done"])

    def test_decisions_and_asks_keep_answered(self):
        self.assertIn('"Answered"', nav_entry("decisions"))
        self.assertIn('"Answered"', nav_entry("asks"))

    def test_answered_tasks_fold_into_in_progress(self):
        self.assertIn("const t = P.t.concat(P.ta).sort(sortKey), ta = [];", SRC)
        self.assertIn('id="sub-ta" data-sub="answered" hidden', SRC)
        self.assertIn('tParts = [t.length + " in progress"]', SRC)


if __name__ == "__main__":
    unittest.main()

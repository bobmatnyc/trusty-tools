"""Dashboard control-group layout (owner request 2026-10-09 03:27Z): Mark done and Won't do share one sizing
class and rule, and Feedback joins the Park row on asks, decisions and tasks. Static checks on both HTML files."""
import os
import re
import unittest

from test_dashboard_progress import PAGES


class TestDashboardActionLayout(unittest.TestCase):
    def test_done_and_wontdo_share_sizing_rule_and_feedback_joins_park_row(self):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                self.assertRegex(src, r'el\("button", "btn act-btn[^"]*"(?: \+ [^,]*)?, "Mark done"\)')
                self.assertIn('el("button", "rm wd-open act-btn", "Won\'t do")', src)
                self.assertRegex(src, r"\n\.act-btn \{[^}]*flex: 0 0 9rem; width: 9rem;")
                self.assertRegex(src, r"@media[^{]*\{(?:.|\n)*\n  \.act-btn \{ flex: 1 1 100%; width: 100%;")
                # Feedback is handed the Park row container and appends its closed button there.
                self.assertEqual(src.count("feedbackControl(body, it, parkControl(body, it));"), 3)
                self.assertEqual(len(re.findall(r"\n    parkControl\(body, it\);", src)), 0)
                fb = re.search(r"function feedbackControl\(body, it, into\) \{.*?if \(into\) \{ into\.appendChild\(b\); return; \}", src, re.S)
                self.assertIsNotNone(fb)
                self.assertIn("    return row;\n  }\n  async function parkItem", src)


if __name__ == "__main__":
    unittest.main()

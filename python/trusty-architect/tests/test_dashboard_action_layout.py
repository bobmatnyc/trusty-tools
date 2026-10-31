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
                self.assertRegex(src, r'el\("button", "btn act-btn[^"]*"(?: \+ [^,]*)?, "Done"\)')
                self.assertIn('el("button", "rm wd-open act-btn", "Won\'t do")', src)
                self.assertRegex(src, r"\n\.act-btn \{[^}]*flex: 0 0 9rem; width: 9rem;")
                self.assertRegex(src, r"@media[^{]*\{(?:.|\n)*\n  \.act-btn \{ flex: 1 1 100%; width: 100%;")
                # Feedback is handed the Park row container and appends its closed button there.
                # D and A hand the Park menu in the item's .act-tools row (the owner 2026-10-26); the task keeps its own Park row.
                self.assertEqual(src.count("feedbackControl(body, it, parkControl(tools, it));"), 2)
                self.assertEqual(src.count("feedbackControl(body, it, parkControl(body, it));"), 1)
                self.assertEqual(len(re.findall(r"\n    parkControl\(body, it\);", src)), 0)
                fb = re.search(r"function feedbackControl\(body, it, into\) \{.*?if \(into\) \{ into\.appendChild\(b\); return; \}", src, re.S)
                self.assertIsNotNone(fb)
                self.assertIn("    return row;\n  }\n  async function steerItem", src)

    def test_have_architect_do_button_writes_answered_architect_do(self):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                self.assertIn('el("button", "btn ghost act-btn ad-btn", "Architect Do")', src)
                self.assertRegex(src, r'status: "answered", answer: "Architect Do", answer_note: later\.value\.trim\(\), answered_at: new Date\(\)\.toISOString\(\)')
                self.assertIn("row.append(done, arch, wont, later);", src)

    def test_wont_do_button_on_primary_row_shares_note_and_writes_wont_do(self):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                src = open(page, encoding="utf-8").read()
                self.assertNotIn("Mark done", src)
                fn = re.search(r"  function renderAction\(it\) \{.*?\n  \}\n", src, re.S).group(0)
                self.assertIn("\"btn ghost act-btn wd-btn\", \"Won't Do\")", fn)
                self.assertNotIn("wontDoControl", fn)
                # Order Done, Architect Do, Won't Do (Done filled; the other two ghost).
                self.assertLess(fn.index('"Done")'), fn.index('"Architect Do")'))
                self.assertLess(fn.index('"Architect Do")'), fn.index("\"Won't Do\")"))
                # The shared note field and staged attachments feed the Won't Do write.
                wd = fn[fn.index("wont.addEventListener"):]
                self.assertIn("stage.flush()", wd)
                self.assertIn("later.value.trim()", wd)
                self.assertIn("wontDoPrimarySend(it, note)", wd)


if __name__ == "__main__":
    unittest.main()

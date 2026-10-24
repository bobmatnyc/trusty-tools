"""Run-window task rows reuse the T08 progress components and stay one line (owner 2026-10-21: "the T08
version looks better"). The old full-width run line (.runline, flex 1 1 100%) is gone; the window state is a
chip, the due date sits in the "est." slot, and the percent is the session-row chip via etaSummary.

Test: scripts/test_dashboard_summary_wrap.py
"""
import re
import unittest

from test_dashboard_progress import PAGES

PAGE = PAGES[0]   # templates/dashboard/index.html: the page that carries run windows


class TestSummaryWrap(unittest.TestCase):
    def setUp(self):
        self.html = open(PAGE, encoding="utf-8").read()
        self.css = re.search(r"<style[^>]*>(.*?)</style>", self.html, re.S).group(1)

    def test_no_second_line_run_bar(self):
        self.assertNotIn(".runline", self.css)
        self.assertNotIn("runline", self.html)
        self.assertNotIn("rtrack", self.html)

    def test_run_window_rows_use_shared_progress_component(self):
        # the summary renders the run-window progress through etaSummary (session-row percent chip), same as T08
        self.assertRegex(self.html, r"etaProgress\(it, true, Date\.now\(\)\) \|\| \(rs \? runProgress\(it, Date\.now\(\)\)")
        self.assertRegex(self.html, r"sum\.appendChild\(etaSummary\(ep\)\)")
        body = re.search(r"function runProgress\(.*?\n  \}\n", self.html, re.S).group(0)
        self.assertIn("overdue:", body)
        self.assertIn("stale:", body)
        self.assertNotIn('el("div"', body)   # builds data only; no bespoke bar markup

    def test_state_chip_is_a_chip(self):
        self.assertRegex(self.html, r'el\("span", "chip runstate"')

    def test_row_title_cannot_collapse_and_chips_shrink(self):
        # a long nowrap chip (T06 parked/stale reason) once took the whole row and left the title 0px wide
        title = re.search(r"\.item > summary \.title \{([^}]*)\}", self.css).group(1)
        self.assertRegex(title, r"min-width: min\(10rem, 100%\)")
        chip = re.search(r"\.item > summary \.chip \{([^}]*)\}", self.css).group(1)
        # owner 2026-10-21: chips are short fixed labels and never truncate; the row wraps instead
        for want in ("flex: 0 0 auto", "white-space: nowrap"):
            self.assertIn(want, chip)
        for bad in ("overflow: hidden", "text-overflow: ellipsis"):
            self.assertNotIn(bad, chip)
        self.assertIn("flex-wrap: wrap", re.search(r"\.item > summary \{([^}]*)\}", self.css).group(1))

    def test_chips_are_short_labels_and_reasons_live_in_the_body(self):
        h = self.html
        self.assertIn('function staleText(it) { return "stale"; }', h)
        self.assertIn('return "steer " + (l.relayed_at ? "relayed" : "sent");', h)
        self.assertIn('return "clarify";', h)
        self.assertIn('"reopened"', h)
        for field in ('dets.push(["Stale", staleReason(it)])', 'dets.push(["Clarify", clarifyDetail(it)])', '"Steer " + (l.relayed_at', 'dets.push(["Run window", rs.title])'):
            self.assertIn(field, h)
        self.assertNotIn('"waiting on PM: "', h)
        self.assertNotRegex(self.css, r"\.item > summary \.chip, \.item > summary \.state[^{]*\{ flex: none")

    def test_pulse_rule_is_not_a_descendant_of_lstep_done(self):
        # a stray selector fragment once turned `.pulse` into `.lstep.done .pulse`
        self.assertRegex(self.css, r"(?m)^\.pulse \{ position: relative; \}")


if __name__ == "__main__":
    unittest.main()

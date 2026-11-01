"""Dashboard D/A action area (the owner 2026-10-26): row 1 answer controls, row 2 one compact .act-tools row. Node checks, no browser.

Why: the buttons were stacked as separate rows all over an open item.
What: the secondary row holds Request brief, Respond interactively, Won't do, a styled Attach label and a "Park until" menu
(Tomorrow / Next week / Next month / Feedback) in that order; handlers and ids are unchanged; CSS gives one height, padding and
font to the row and wraps it on mobile; a task keeps its own Park row.
Test: this file.
"""
import re
import unittest

from test_dashboard_progress import PAGES


class TestActTools(unittest.TestCase):
    def src(self):
        return open(PAGES[0], encoding="utf-8").read()

    def fn(self, name):
        return re.search(r"  function %s\(it\) \{.*?\n  \}\n" % name, self.src(), re.S).group(0)

    def test_d_and_a_build_one_tools_row_in_the_requested_order(self):
        for name in ("renderQuestion", "renderAction"):
            body = self.fn(name)
            order = ["briefControl(body, it, tools)", "body.appendChild(tools);", "interactiveControl(body, it, tools);",
                     "wontDoControl(body, it, tools);", "attachControl(body, it, tools);", "parkControl(tools, it)"]
            if name == "renderAction":   # Won't Do moved to the primary row (the owner 2026-10-29)
                order.remove("wontDoControl(body, it, tools);")
            idx = [body.index(x) for x in order]
            self.assertEqual(idx, sorted(idx), name)
        self.assertNotIn("tools", self.fn("renderTask"))

    def test_attach_is_a_styled_label_keeping_the_input_and_its_id(self):
        s = self.src()
        self.assertIn('el("label", "att-btn", "Attach"); lab.setAttribute("for", inp.id); lab.appendChild(inp);', s)
        self.assertIn('inp.id = "attach-" + it.id', s)

    def test_park_menu_holds_choices_and_feedback(self):
        s = self.src()
        self.assertIn('el("details", "park-menu")', s)
        self.assertIn("Park until \\u25be", s)
        self.assertIn("PARK_CHOICES.forEach", s)
        self.assertIn("into.appendChild(b)", s)   # Feedback joins the panel through the returned container

    def test_css_one_size_for_the_row_and_mobile_wrap(self):
        s = self.src()
        rule = re.search(r"\.act-tools > button, [^{]*\{([^}]*)\}", s).group(1)
        for part in ("min-height: 32px", "padding: 4px 12px", "font-size: var(--fs-sm)", "white-space: nowrap", "box-sizing: border-box"):
            self.assertIn(part, rule)
        self.assertRegex(s, r"\n\.act-tools \{ display: flex; flex-wrap: wrap;")
        self.assertRegex(s, r"@media[^{]*\{(?:.|\n)*\n  \.act-tools \.act-btn \{ flex: 1 1 auto; width: auto;")
        self.assertIn(".act-tools .act-btn { flex: 0 0 auto; width: auto;", s)   # no fixed 9rem width, so no text overflow


if __name__ == "__main__":
    unittest.main()

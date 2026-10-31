"""Dashboard/console cross links reuse a named tab: the page names its window "architect-dashboard" and the
Trusty Console link targets "trusty-console" without rel=noopener (noopener makes Chrome open a new tab)."""
import os
import re
import unittest

from test_dashboard_progress import PAGES


class TestDashboardCrossLink(unittest.TestCase):
    def test_console_link_targets_named_tab_without_noopener(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                tag = re.search(r'<a class="ctl console-link"[^>]*target=[^>]*>', src).group(0)
                self.assertIn('target="trusty-console"', tag)
                self.assertNotIn("rel=", tag)

    def test_page_sets_window_name(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                self.assertIn('window.name = "architect-dashboard"', src)


if __name__ == "__main__":
    unittest.main()

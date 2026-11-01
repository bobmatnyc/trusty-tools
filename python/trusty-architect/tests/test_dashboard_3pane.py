"""Dashboard 3-pane layout (owner 2026-10-21): left nav, main pane, right pane (the reader). Structural checks on the page text (no browser)."""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = open(os.path.join(ROOT, "templates/dashboard/index.html"), encoding="utf-8").read()


class TestThreePane(unittest.TestCase):
    def test_three_panes_exist_in_order(self):
        self.assertTrue(re.search(r'<div class="shell">\s*<aside id="navcol">.*?<nav id="nav" aria-label="[^"]+"><div id="nav-top"></div><div id="nav-cats"></div></nav>\s*</aside>\s*<div id="nav-scrim"[^>]*></div>\s*<div id="pane">.*?<header>.*?</header>\s*<main id="main">.*?</main>\s*</div>\s*</div>', SRC, re.S))
        self.assertIn('root.id = "reader"', SRC)   # right pane is the reader

    def test_every_section_belongs_to_a_nav_category(self):
        cats = re.findall(r'<details class="sec" id="(sec-[a-z]+)"[^>]*data-cat="([a-z]+)"', SRC)
        self.assertEqual({c for _, c in cats}, {"decisions", "questions", "asks", "tasks", "goals", "sessions", "accounts", "visualize", "secrets", "projects", "cliprofiles"})   # secrets: the owner 2026-10-24; projects: the owner 2026-10-27; cliprofiles: the owner 2026-10-28
        nav = re.search(r"const NAV = \[(.*?)\n  \];", SRC, re.S).group(1)
        for key in ("decisions", "questions", "asks", "tasks", "goals", "sessions"):
            self.assertIn('key: "%s"' % key, nav)
        for sec, _ in cats:
            self.assertIn('"%s"' % sec, nav)

    def test_subcategory_blocks_are_tagged(self):
        for sid, sub in (("sub-qx", "done"), ("sub-ap", "parked"), ("sub-tx", "done"), ("sub-gs", "standing"), ("list-q", "open"), ("list-ques", "answered")):
            self.assertRegex(SRC, r'id="%s"[^>]*data-sub="%s"' % (sid, sub))
        self.assertIn("d.dataset.sub = g.key", SRC)   # session user-state groups

    def test_hash_routes(self):
        self.assertIn('"#view/" + cat', SRC)
        self.assertRegex(SRC, r'\^#\?view\\/\(\[a-z\]\+\)')
        self.assertIn('ref.indexOf("view/") === 0) return', SRC)   # item-anchor handler ignores nav routes
        self.assertIn('ref.indexOf("doc/") === 0) return', SRC)    # and reader routes
        self.assertIn("window.navView.ensure(row)", SRC)           # #D13 anchors switch the main pane first
        self.assertIn("parseDocHash(location.hash)", SRC)          # #doc/ full-page route kept

    def test_nav_accessibility_and_counts(self):
        self.assertIn('setAttribute("aria-current", "page")', SRC)
        self.assertIn('setAttribute("aria-expanded"', SRC)
        self.assertIn('setAttribute("aria-controls", "nvs-"', SRC)

    def test_drawer_and_right_pane_breakpoints(self):
        self.assertRegex(SRC, r"@media \(max-width: 840px\) \{\s*\.shell")
        self.assertIn("body.nav-open #navcol", SRC)
        self.assertIn("(min-width: 1600px)", SRC)                  # right pane pushes only on wide screens, else overlays
        self.assertIn("html.rd-dock .wrap", SRC)
        self.assertRegex(SRC, r"rd-listmode[^}]*width: 380px")      # briefs list width

    def test_write_controls_still_present(self):
        for ident in ("attn-seen", "ques-clear-all", "user-state-badge", "sess-pick", "settings-btn", "rd-pop", "rd-close"):
            self.assertIn('id="%s"' % ident, SRC)

    def test_accordion_headers_are_buttons_with_regions(self):
        self.assertIn('hd.setAttribute("aria-controls", "nvs-" + c.key)', SRC)
        self.assertIn('rg.setAttribute("role", "region")', SRC)
        self.assertRegex(SRC, r"\.nv-region \{[^}]*grid-template-rows: 0fr[^}]*transition")   # animated collapse
        self.assertRegex(SRC, r"\.nv-region\.open \{[^}]*1fr")
        self.assertIn("function setOpen(cat, on)", SRC)                                           # in place, saved, several open at once

    def test_media_pane_starts_minimized_as_icon_strip(self):
        self.assertNotIn('id="briefs-btn"', SRC)                                                 # header Briefs button removed
        self.assertIn('b.setAttribute("aria-label", "Media")', SRC)
        self.assertIn('saveLib("min")', SRC)                                                     # minimized state persists
        self.assertIn('LIB_KEY = "architect-library"', SRC)
        self.assertRegex(SRC, r'class="rd-pane">.*?</svg>Media</span>')                          # expanded header reads Media
        self.assertIn('ev.code !== "KeyL" || !ev.altKey', SRC)                                   # Alt+L

    def test_library_tabs_and_sources(self):
        for tab in ('["brief", "Briefs"]', '["diagram", "Diagrams"]', '["image", "Images"]', '["file", "Files"]', '["pr", "PRs"]', '["all", "All"]'):
            self.assertIn(tab, SRC)
        self.assertIn('fetch("/briefs"', SRC)
        self.assertIn('fetch("/media?type=diagram,image,file"', SRC)
        self.assertIn("github\\.com", SRC)                                                       # PR/issue links parsed from items
        self.assertIn('"/media-file/"', SRC)

    def test_one_header_bar_with_tabs_and_no_section_header(self):
        self.assertTrue(re.search(r'<div id="main-bar">.*?id="main-title".*?id="main-count".*?id="main-sum".*?id="main-tabs" role="tablist"', SRC, re.S))
        self.assertNotIn('id="main-sort"', SRC)                                    # no order control in the main pane
        self.assertNotIn('id="main-tools"', SRC)
        self.assertIn("#main > .sec { border-top: 0; border-bottom: 0 !important; }", SRC)   # one divider: the bar's
        self.assertIn("#main > .sec > summary { display: none; }", SRC)            # the section's own header is gone from the main pane
        self.assertIn('b.setAttribute("role", "tab")', SRC)
        self.assertIn('e.key === "ArrowRight"', SRC)                               # tablist arrow keys
        self.assertNotIn('id="main-filter"', SRC)                                  # the filter moved to the left column
        self.assertNotIn('id="main-chip"', SRC)

    def test_every_category_has_an_open_tab_that_is_the_default(self):
        self.assertIn('sub: sub || (cc && cc.def) || "open"', SRC)                                   # a bare #view/<cat> is Open
        self.assertIn('view = { cat: cat, sub: (cc && cc.def) || "open" }', SRC)                     # the header click resets to Open
        self.assertIn("OPEN_SETS", SRC)                                            # derived Open for Goals and Sessions
        self.assertIn('count: "n-accounts", subs: [["open", "Open", "n-accounts"]]', SRC)
        self.assertIn('["questions", "Questions"' if False else 'subs: [["pending", "Pending", "n-qpd"], ["answered", "Answered", "n-questions"]', SRC)
        self.assertIn("subs.filter((t) => !t[3])", SRC)                            # All is the true total

    def test_nav_search_matches_id_title_body_and_clears(self):
        self.assertIn('inp.id = "nav-search"', SRC)
        self.assertIn('e.key === "Escape" && inp.value', SRC)
        self.assertIn("r.options.forEach", SRC)                                    # option labels and descriptions are searched
        for f in ("r.answer_note", "r.outcome", "r.task", "r.next", "r.detail"):
            self.assertIn(f, SRC)
        self.assertIn('box.classList.add("dim")', SRC)                             # categories without a match dim
        self.assertIn("P.qx.filter(qhit)", SRC)                                    # the Done tabs page through matches

    def test_answered_items_have_their_own_tab_so_counts_add_up(self):
        for ident in ("sub-qa", "sub-aa", "sub-ta", "list-qa", "n-qa"):
            self.assertIn('id="%s"' % ident, SRC)
        self.assertIn('["answered", "Answered", "n-qa"]', SRC)
        self.assertIn("comp.qx.list = P.qx.filter(qhit)", SRC)                     # Done holds done items only, never answered ones
        self.assertIn("comp.qa.list = P.qa.filter(qhit)", SRC)

    def test_bar_actions_never_print_null_and_one_divider(self):
        self.assertIn('.append(...[$("attn-seen"), $("ques-clear-all")].filter(Boolean))', SRC)   # append(null) prints "null"
        self.assertNotIn('$("main-actions").append($("attn-seen")', SRC)
        self.assertIn("#main .empty { border-top: 0; }", SRC)                                    # the bar's border is the only line

    def test_goal_complete_wiring(self):
        self.assertIn("function goalDoneControl(g, kind)", SRC)
        self.assertIn('["completed", "Completed", "n-gc"]', SRC)
        self.assertIn("db.batch", SRC)
        self.assertIn("goalIsDone(g)", SRC)



class TestNavClickCollapsesMain(unittest.TestCase):
    """the owner 2026-10-25: clicking a nav item collapses every expanded item in the main pane."""

    def test_collapse_helper_skips_sections_and_forced_open_blocks(self):
        self.assertIn('"#main details[open]:not(.sec):not(.nv-solo)"', SRC)

    def test_nav_link_click_collapses_before_the_route_shows(self):
        m = re.search(r'\$\("nav"\)\.addEventListener\("click", \(e\) => \{(.*?)\n  \}\);', SRC, re.S)
        self.assertTrue(m)
        body = m.group(1)
        self.assertIn('closest("a.nv-link")', body)
        self.assertLess(body.index("collapseMain()"), body.index("scrollTo"))
        self.assertIn("window.scrollTo(0, 0)", body)               # same-route click still scrolls to the top

    def test_category_header_reset_uses_the_same_helper_and_state_stays_persisted(self):
        self.assertTrue(re.search(r"function resetView\(cat\) \{.*?collapseMain\(\);", SRC, re.S))
        self.assertIn("openState[key] = d.open; saveOpen();", SRC)   # toggle handler persists the collapse

    def test_category_header_click_collapses_on_toggle_and_on_navigate(self):
        m = re.search(r'hd\.addEventListener\("click", \(\) => \{(.*?)\n        \}\);', SRC, re.S)
        self.assertTrue(m)
        body = m.group(1)
        self.assertLess(body.index("collapseMain()"), body.index("setOpen(c.key, on)"))   # before the toggle/navigate branch

    def test_collapse_writes_open_state_before_render_rebuilds_cards(self):
        # The toggle event is async; render() right after resetView rebuilt T13/T02 open from the stale openState.
        m = re.search(r"function collapseMain\(\) \{(.*?)\n  \}", SRC, re.S)
        self.assertTrue(m)
        self.assertIn("openState[d._key] = false", m.group(1))
        self.assertIn("d._key = key;", SRC)   # bindOpen records the key on the element


if __name__ == "__main__":
    unittest.main()

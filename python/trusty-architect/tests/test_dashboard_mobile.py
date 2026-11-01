"""Dashboard mobile layout (brief 2026-10-08-dashboard-mobile.md), checked on both HTML files.
CSS rules are parsed from the page text (no browser); view-state helpers run under node (skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]
CTL = open(os.path.join(ROOT, "templates/dashboard/tokens.css"), encoding="utf-8").read()
TAP_CLASSES = [".btn", ".rm", ".opt", ".cp", ".jump", ".mv", ".handle", ".step-hit", ".tap"]
MAIN_CONTAINERS = {"body", "html", ".wrap", "header", ".sec", ".sec-body", ".rows", ".item", ".body", ".kv", ".opts", ".brand", ".goal-row"}


def read(page):
    return open(page, encoding="utf-8").read()


def css_rules(src):
    """[(selectors, {prop: value})] for every plain rule, media nesting flattened."""
    css = re.sub(r"/\*.*?\*/", "", re.search(r"<style>(.*?)</style>", src, re.S).group(1), flags=re.S)
    out = []
    for m in re.finditer(r"([^{}]+)\{([^{}]*)\}", css):
        sels = [s.strip() for s in m.group(1).split(",") if s.strip() and not s.strip().startswith("@")]
        decls = {}
        for d in m.group(2).split(";"):
            if ":" in d:
                k, v = d.split(":", 1)
                decls[k.strip()] = v.strip()
        out.append((sels, decls))
    return out


class TestDashboardMobileCss(unittest.TestCase):
    def pages(self):
        return [read(page) for page in PAGES]

    def test_viewport_meta_and_schemes(self):
        for src in self.pages():
            self.assertIn('name="viewport" content="width=device-width, initial-scale=1', src)
            self.assertIn("@media (prefers-reduced-motion: reduce)", src)
        tok = read(os.path.join(ROOT, "templates/dashboard/tokens.css"))      # the dark selectors moved to the shared file; the pages follow the theme through it
        self.assertIn("@media (prefers-color-scheme: dark)", tok)
        self.assertIn(':root[data-theme="dark"]', tok)
        for page in PAGES + [os.path.join(ROOT, "templates/dashboard/live.html")]:
            self.assertNotIn("--surface-0:", read(page), page)                   # no page carries its own token block

    def test_no_fixed_width_above_360px_on_main_containers(self):
        for src in self.pages():
            for sels, decls in css_rules(src):
                if not MAIN_CONTAINERS.intersection(sels):
                    continue
                for prop in ("width", "min-width"):
                    m = re.fullmatch(r"(\d+(?:\.\d+)?)px", decls.get(prop, ""))
                    self.assertFalse(m and float(m.group(1)) > 360, "%s { %s: %s }" % (sels, prop, decls.get(prop)))

    def test_touch_target_rule_covers_every_control(self):
        for src in self.pages():
            tap = [sels for sels, decls in css_rules(src) if decls.get("min-height") == "44px"]
            covered = {s for sels in tap for s in sels}
            for cls in TAP_CLASSES:
                self.assertIn(cls, covered, cls + " has no 44px min-height rule")
            for row in (".item > summary", ".sub > summary", ".sec > summary"):
                self.assertIn(row, covered)

    def test_rows_wrap_instead_of_truncating_on_phone(self):
        for src in self.pages():
            rules = css_rules(src)
            wrap = [d for s, d in rules if ".item > summary" in s and d.get("flex-wrap") == "wrap"]
            title = [d for s, d in rules if ".item > summary .title" in s and d.get("white-space") == "normal"]
            self.assertTrue(wrap and title)


class TestDashboardHeaderIcons(unittest.TestCase):
    """Header (the owner 2026-10-24): Live is an icon link; the Menu hamburger lives in the left column header."""

    def test_live_icon_and_column_hamburger(self):
        for src in [read(p) for p in PAGES]:
            live = re.search(r'<a [^>]*id="live-link"[^>]*>(.*?)</a>', src, re.S)
            self.assertTrue(live, "no Live link")
            self.assertIn('href="/live"', live.group(0))
            self.assertIn('aria-label="Live"', live.group(0))
            self.assertIn("<svg", live.group(1))
            self.assertNotIn("Live", live.group(1))
            menu = re.search(r'<button [^>]*id="nav-toggle"[^>]*>(.*?)</button>', src, re.S)
            self.assertTrue(menu, "no Menu button")
            self.assertIn('aria-label="Collapse menu"', menu.group(0))
            self.assertIn("<svg", menu.group(1))

    def test_icon_buttons_are_at_least_40px(self):
        for src in [read(p) for p in PAGES]:
            self.assertRegex(CTL, r"\.ctl \{[^}]*height: var\(--h-control\)")
            self.assertRegex(CTL, r"\.ctl--icon \{ width: var\(--h-control\)")
            self.assertIn("--h-control: 40px", CTL)

    def test_header_controls_have_40px_hit_areas(self):
        # one control (.ctl in tokens.css): 40px, 44px on phones and coarse pointers; pages keep no private header-button sizes
        self.assertIn("--h-touch: 44px", CTL)
        self.assertRegex(CTL, r"@media \(max-width: 600px\), \(pointer: coarse\) \{\s*\.ctl \{ height: var\(--h-touch\)")
        for src in [read(p) for p in PAGES]:
            for old in ("\n.collapse-all {", "\n.console-link {", "\n.state-badge { border-block", "\n.sess-pick { box-sizing"):
                self.assertNotIn(old, src)


class TestDashboardCollapsibleColumn(unittest.TestCase):
    """Left column (the owner 2026-10-24): the toggle sits in the column header, the main header is inside the main pane, collapse state persists."""

    def setUp(self):
        self.src = read(PAGES[0])

    def test_toggle_is_in_column_header_not_main_header(self):
        col = re.search(r'<aside id="navcol">(.*?)</aside>', self.src, re.S).group(1)
        head = re.search(r'<div id="navcol-head">(.*?)</div>', col, re.S).group(1)
        self.assertIn('id="nav-toggle"', head)
        self.assertIn("<svg", head)
        self.assertLess(col.index("navcol-head"), col.index('<nav id="nav"'))
        main_header = re.search(r"<header>(.*?)</header>", self.src, re.S).group(1)
        self.assertNotIn("nav-toggle", main_header)

    def test_main_header_is_inside_the_pane_above_main(self):
        pane = self.src.index('<div id="pane">')
        self.assertLess(self.src.index('<aside id="navcol">'), pane)
        self.assertLess(pane, self.src.index("<header>"))
        self.assertLess(self.src.index("</header>"), self.src.index('<main id="main">'))
        self.assertLess(self.src.index("</main>"), self.src.index("<footer"))
        self.assertNotRegex(self.src, r'<div class="wrap">\s*<header>')   # header no longer spans the full page

    def test_reopen_control_when_drawer_closed(self):
        m = re.search(r'<button [^>]*id="nav-reopen"[^>]*>', self.src)
        self.assertTrue(m and 'aria-label="Open menu"' in m.group(0))
        self.assertGreater(m.start(), self.src.index('<div id="pane">'))
        self.assertLess(m.start(), self.src.index("<header>"))
        rules = css_rules(self.src)
        self.assertEqual([d["display"] for s, d in rules if s == ["#nav-reopen"] and "display" in d][0], "none")
        self.assertTrue([d for s, d in rules if s == ["#nav-reopen"] and d.get("display") == "inline-flex"])

    def test_desktop_collapses_to_rail_and_widens_main(self):
        rules = css_rules(self.src)
        rail = [d for s, d in rules if "body.nav-collapsed .shell" in s][0]
        self.assertEqual(rail["grid-template-columns"], "56px minmax(0, 1fr)")
        self.assertTrue([d for s, d in rules if "body.nav-collapsed #nav" in s and d.get("display") == "none"])
        wide = [d for s, d in rules if "body.nav-collapsed #main" in s][0]
        self.assertGreater(int(wide["max-width"][:-2]), 980)
        self.assertTrue([d for s, d in rules if "body.nav-open #navcol" in s])   # <= 840px overlay drawer

    def test_collapse_state_persists_in_try_catch_localstorage(self):
        self.assertIn('NAVCOL_KEY = "arch.navcol.collapsed"', self.src)
        self.assertRegex(self.src, r'try \{ localStorage\.setItem\(NAVCOL_KEY[^}]*\} catch \(_\) \{\}')
        self.assertRegex(self.src, r'try \{ if \(localStorage\.getItem\(NAVCOL_KEY\) === "1"\)[^}]*\} catch \(_\) \{\}')


class TestLiveFrameFit(unittest.TestCase):
    """The Visualize Live frame is sized from its real offset (header sits inside the pane), not a fixed calc(100vh - N)."""

    def test_frame_height_fits_from_offset_and_page_does_not_scroll(self):
        src = read(PAGES[0])
        fn = src[src.index("window.fitLiveFrame = function"):src.index("window.addEventListener(\"resize\"")]
        self.assertIn("getBoundingClientRect().top + window.scrollY", fn)
        self.assertIn("window.innerHeight - top - below", fn)
        self.assertIn("window.fitLiveFrame", src[src.index('b.id === "viz-live"'):][:400])   # run when the frame is shown
        self.assertIn('hashchange", () => requestAnimationFrame(window.fitLiveFrame)', src)
        live = [d for s_, d in css_rules(src) if any("footer" in x and "data-sub=\"live\"" in x for x in s_)]
        self.assertTrue(live and live[0]["display"] == "none")   # nothing under the frame to scroll to


FAKE = r"""
class F { constructor(id, value) { this.id = id; this.value = value; } }
"""


def run(page, body):
    src = read(page)
    m = re.search(r"/\* BEGIN keepView \*/(.*?)/\* END keepView \*/", src, re.S)
    assert m
    r = subprocess.run(["node", "-e", FAKE + m.group(1) + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardKeepView(unittest.TestCase):
    def test_update_keeps_open_row_and_draft(self):
        body = """
        const openState = { "item:D01": true }, seen = new Map([["D01", "open"]]);
        const same = syncOpen(openState, seen, "D01", "open");            /* a store update, status unchanged */
        const drafts = captureDrafts([new F("note-A02", "half typed"), new F("empty", ""), new F("", "x")]);
        const rebuilt = { "note-A02": new F("note-A02", ""), "other": new F("other", "kept") };
        restoreDrafts(drafts, (id) => rebuilt[id]);
        console.log(JSON.stringify([same, openState, drafts, rebuilt["note-A02"].value]));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                same, open_state, drafts, value = run(page, body)
                self.assertFalse(same)
                self.assertEqual(open_state, {"item:D01": True})
                self.assertEqual(drafts, {"note-A02": "half typed"})
                self.assertEqual(value, "half typed")

    def test_status_change_resets_open_state_and_store_value_wins(self):
        body = """
        const openState = { "item:D01": true, "item:D02": false }, seen = new Map([["D01", "open"], ["D02", "answered"]]);
        const a = syncOpen(openState, seen, "D01", "answered");            /* answered: collapses */
        const b = syncOpen(openState, seen, "D02", "open");                /* reopened: expands */
        const filled = new F("n", "from store"); restoreDrafts({ n: "draft" }, () => filled);
        console.log(JSON.stringify([a, b, openState, filled.value]));"""
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                a, b, open_state, value = run(page, body)
                self.assertTrue(a and b)
                self.assertEqual(open_state, {"item:D02": True})
                self.assertEqual(value, "from store")


if __name__ == "__main__":
    unittest.main()

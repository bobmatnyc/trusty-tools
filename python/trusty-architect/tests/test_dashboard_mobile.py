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
TAP_CLASSES = [".btn", ".rm", ".opt", ".cp", ".jump", ".collapse-all", ".console-link", ".cbar .pg", ".mv", ".handle", ".step-hit", ".tap"]
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
            self.assertIn("@media (prefers-color-scheme: dark)", src)
            self.assertIn(':root[data-theme="dark"]', src)

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

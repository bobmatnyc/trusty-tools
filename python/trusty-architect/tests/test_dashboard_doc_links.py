"""Doc links on the dashboard show a name and keep the path for hover only (owner 2026-10-23: "always link to artifact,
paths only for mouseover.").

Why: specs and prose paths printed full paths (or "label (localhost . tailnet)") as the visible text.
What: the pure docRefOfUrl / docPathSplit helpers (run under node; skipped when node is absent) and the page source:
docAnchor sets the title to project/path, specs and the links row use it, prose escapes through text nodes only.
Test: scripts/test_dashboard_doc_links.py
"""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES

TAIL = "https://dash.example:7890/"


def run_js(page, tail):
    src = open(page, encoding="utf-8").read()
    js = re.search(r"/\* BEGIN docPaths \*/(.*?)/\* END docPaths \*/", src, re.S).group(1) + tail
    return json.loads(subprocess.run(["node", "-e", js], capture_output=True, text=True, check=True).stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDocHelpers(unittest.TestCase):
    def test_url_forms_resolve_to_project_and_path(self):
        urls = ["http://127.0.0.1:7890/#doc/mcp/docs/a%20b.md", TAIL + "#doc/architect/local/briefs/x.md",
                "/files/local/briefs/y.md", "https://github.com/example/r/pull/1", "#doc/p/.claude/x.md", "http://127.0.0.1:7890/live"]
        for page in PAGES:
            out = run_js(page, "console.log(JSON.stringify(%s.map((u) => docRefOfUrl(u, %s))));" % (json.dumps(urls), json.dumps(TAIL)))
            self.assertEqual(out, [{"project": "mcp", "path": "docs/a b.md"}, {"project": "architect", "path": "local/briefs/x.md"},
                                   {"project": "architect", "path": "local/briefs/y.md"}, None, None, None])

    def test_prose_paths_link_only_when_they_resolve(self):
        text = "see local/briefs/a.md, /srv/x/architect/docs/b.md and architect/docs/c.md; keep ../local/d.md, infra/e.md, .claude/f.md"
        for page in PAGES:
            call = "console.log(JSON.stringify(docPathSplit(%s, %%s, %s).filter((p) => p.doc).map((p) => [p.t, p.doc.path])));"
            call = call % (json.dumps(text), json.dumps("/srv/x/architect/"))
            self.assertEqual(run_js(page, call % "true"), [["local/briefs/a.md", "local/briefs/a.md"], ["/srv/x/architect/docs/b.md", "docs/b.md"], ["architect/docs/c.md", "docs/c.md"]])
            # a non-Architect item: bare local/ paths stay text, the explicit forms still link
            self.assertEqual([x[0] for x in run_js(page, call % "false")], ["/srv/x/architect/docs/b.md", "architect/docs/c.md"])
            # the text parts always rebuild the input
            self.assertEqual(run_js(page, "console.log(JSON.stringify(docPathSplit(%s, true, '').map((p) => p.t).join('')));" % json.dumps(text)), text)


class TestDocLinkSource(unittest.TestCase):
    def test_title_on_hover_is_the_path_and_text_is_the_name(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('a.title = project + "/" + path;', src)
            self.assertIn('el("a", "doc-link", label || docName(path))', src)
            self.assertIn("sr.appendChild(docAnchor(dr.project, dr.path, txt(x.label)))", src)   # specs row
            self.assertIn("if (l.title) a.title = l.title;", src)                               # Linked: row
            self.assertIn("dr.project + \"/\" + dr.path", src)
            body = re.search(r"function docAnchor\(.*?\n  }\n", src, re.S).group(0)
            self.assertNotIn("innerHTML", body)

    def test_prose_and_answer_note_get_doc_links_for_architect_items(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn("function isArchItem(it)", src)
            self.assertGreaterEqual(src.count("isArchItem(it)"), 8)   # detail, outcome, answer_note, updates call sites
            self.assertIn("docPathSplit(str, arch, ARCH_ROOT)", src)


class TestVisualizeBar(unittest.TestCase):
    def test_tabs_sit_right_of_the_heading_and_live_frame_uses_the_saved_space(self):
        """owner 2026-10-23: the Visualize tabs share the heading's line; the Live frame offset shrank from 170px."""
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            bar = re.search(r'#main\[data-cat="visualize"\] #main-bar \{([^}]*)\}', src).group(1)
            self.assertIn("display: flex", bar)
            self.assertIn("flex-wrap: wrap", bar)       # phone: tabs wrap below, no horizontal scroll
            self.assertIn("align-items: center", bar)
            self.assertIn('#main[data-cat="visualize"] #main-tabs { margin: 0;', src)
            self.assertIn("height: calc(100dvh - 118px)", src)
            self.assertNotIn("calc(100dvh - 170px)", src)


class TestThemeInSettings(unittest.TestCase):
    def test_one_theme_control_and_it_lives_in_the_settings_panel(self):
        """owner 2026-10-23: settings stay behind the gear; the theme is set only there."""
        import os
        base = os.path.dirname(PAGES[0])
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            dlg = re.search(r'<dialog id="settings-pane".*?</dialog>', src, re.S).group(0)
            self.assertEqual(len(re.findall(r'type="radio" name="set-theme"', src)), 3)          # system, light, dark: one control
            self.assertEqual(len(re.findall(r'type="radio" name="set-theme"', dlg)), 3)          # all inside the settings dialog
            self.assertIn('id="settings-btn"', src)                                 # the gear opens it
            self.assertIn('id="beep-toggle"', dlg)                                  # other settings kept
            self.assertIn('localStorage.getItem("architect-theme")', src)
        for name in ("live.html",):   # #10161: preview-live.html is a prototype draft, not shipped
            live = open(os.path.join(base, name), encoding="utf-8").read()
            self.assertNotIn('id="theme"', live)                                    # no second toggle
            self.assertIn('localStorage.getItem("architect-theme")', live)          # follows the stored choice


if __name__ == "__main__":
    unittest.main()

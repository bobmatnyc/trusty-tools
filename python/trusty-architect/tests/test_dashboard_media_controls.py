"""Dashboard media copy/download controls (owner 2026-10-23, IL ar-Tdf). Structural checks on the page text (no browser)."""
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = open(os.path.join(ROOT, "templates/dashboard/index.html"), encoding="utf-8").read()


class TestMediaControls(unittest.TestCase):
    def test_row_and_viewer_share_the_controls(self):
        self.assertIn("function mediaActs(", SRC)
        self.assertIn("ac.append(...mediaActs(r.project, r.path, r.title, false))", SRC)   # list row
        self.assertIn("box.append(...mediaActs(current.project, current.path, current.title, true))", SRC)   # viewer
        self.assertIn('id="rd-acts"', SRC)
        # a row's controls are siblings of the row button, never nested inside it
        self.assertIn('li.className = "rd-item"', SRC)

    def test_buttons_are_accessible(self):
        self.assertIn('b.type = "button"; b.className = "rd-act"', SRC)
        self.assertIn('b.setAttribute("aria-label", aria)', SRC)
        self.assertIn('"Download " + (md ?', SRC)
        self.assertIn('"Copy " + (RASTER', SRC)
        self.assertRegex(SRC, r'role", "status"\); el\.setAttribute\("aria-live", "polite"\)')

    def test_touch_target_and_focus_style(self):
        rule = re.search(r"\.rd-act \{([^}]*)\}", SRC).group(1)
        self.assertIn("min-height: 44px", rule)
        self.assertIn("min-width: 44px", rule)
        self.assertIn(".rd-act:focus-visible", SRC)

    def test_download_uses_original_filename(self):
        self.assertIn("a.href = mediaUrl(project, path); a.download = baseName(path)", SRC)
        # mermaid block in a .md file saves as .mmd
        self.assertIn('baseName(path).replace(/\\.md$/i, "") + ".mmd"', SRC)
        self.assertIn("```mermaid[ \\t]*", SRC)

    def test_rendered_svg_download_only_for_drawn_diagrams(self):
        self.assertIn("function downloadSvg(", SRC)
        self.assertIn("XMLSerializer().serializeToString", SRC)
        self.assertIn('s.id = "rd-dl-svg"; s.hidden = true', SRC)
        self.assertIn("if (token === seq) syncSvgBtn();", SRC)   # shown after renderMermaid

    def test_copy_paths(self):
        self.assertIn('new ClipboardItem({ "image/png": png })', SRC)       # raster -> PNG
        self.assertIn('c.toBlob(', SRC)                                       # JPEG converted via canvas
        self.assertIn("await copyText(await mermaidSource(project, path))", SRC)   # svg / mermaid -> source text
        self.assertIn('flash(btn, "Copied link")', SRC)                       # fallback: the file URL
        self.assertIn('flash(btn, "Copy failed")', SRC)
        self.assertIn('flash(btn, "Copied")', SRC)

    def test_pop_out_hidden_in_full_window(self):
        self.assertIn("#reader.rd-full .rd-pop { display: none; }", SRC)
        self.assertIn('.rd-pop[hidden] { display: none; }', SRC)   # normal pane keeps the button

    def test_no_new_server_endpoint(self):
        serve = open(os.path.join(ROOT, "scripts/dashboard-serve.py"), encoding="utf-8").read()
        self.assertNotIn("media-download", serve)
        self.assertNotIn("media-copy", serve)

    def test_page_script_still_parses(self):
        node = shutil.which("node")
        if not node:
            self.skipTest("node not available")
        for i, body in enumerate(re.findall(r"<script>(.*?)</script>", SRC, re.S)):
            with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False) as f:
                f.write(body)
            try:
                r = subprocess.run([node, "--check", f.name], capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, "script %d: %s" % (i, r.stderr[:300]))
            finally:
                os.unlink(f.name)


if __name__ == "__main__":
    unittest.main()

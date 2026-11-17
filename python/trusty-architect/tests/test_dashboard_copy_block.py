"""Doc viewer copy buttons (node, fake DOM): a fenced block in a list gets its own button that copies only the block.

Why: the owner 2026-10-29, the copy icon on a step copied the step text and numbering with the command.
What: copyDecorate wraps each pre in a list or quote with its own button; the click copies the block text with no trailing newline.
Test: tests/test_dashboard_copy_block.py
"""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES

JS = r"""
class E {
  constructor(tag, text) { this.tagName = tag; this.kids = []; this.className = ""; this.text = text || ""; this.listeners = {}; this.parent = null;
    this.classList = { contains: (c) => this.className.split(" ").includes(c) }; }
  get children() { return this.kids; }
  get textContent() { return this.text + this.kids.map((k) => k.textContent).join(""); }
  add(...k) { k.forEach((x) => { x.parent = this; this.kids.push(x); }); return this; }
  append(...k) { this.add(...k); }
  addEventListener(ev, f) { this.listeners[ev] = f; }
  setAttribute() {}
  querySelector(sel) { return sel === "code.language-mermaid" ? null : this.all("PRE")[0] || null; }
  querySelectorAll(sel) { return this.all("PRE"); }
  all(tag, out = []) { this.kids.forEach((k) => { if (k.tagName === tag) out.push(k); k.all(tag, out); }); return out; }
  replaceWith(w) { const i = this.parent.kids.indexOf(this); this.parent.kids[i] = w; w.parent = this.parent; }
}
const document = { createElement: (t) => new E(t) };
const CP_ICON = "", copied = [];
const cpButton = (label) => { const b = new E("button"); b.label = label; return b; };
const copyDo = (b, text) => copied.push(text);
const blockCopy = (el) => ({ text: el.textContent.replace(/\n+$/, "") });
const art = new E("DIV"), li = new E("LI", "1. Run this: "), li2 = new E("LI", "2. Then: ");
const pre1 = new E("PRE", "git status\n"), pre2 = new E("PRE", "echo hi && ls\n");
art.add(new E("OL").add(li.add(pre1), li2.add(pre2)));
copyDecorate(art);
const btns = []; (function walk(n) { if (n.tagName === "button") btns.push(n); n.kids.forEach(walk); })(art);
btns.forEach((b) => b.listeners.click());
console.log(JSON.stringify({ n: btns.length, labels: btns.map((b) => b.label), copied }));
"""


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestCopyBlock(unittest.TestCase):
    def test_each_code_block_copies_only_its_text(self):
        for page in PAGES:
            with self.subTest(page=page):
                src = open(page, encoding="utf-8").read()
                body = re.search(r"/\* BEGIN copyDecorate \*/(.*?)/\* END copyDecorate \*/", src, re.S).group(1)
                r = subprocess.run(["node", "-e", JS.split("const art")[0] + body + "const art" + JS.split("const art")[1]], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                out = json.loads(r.stdout)
                self.assertEqual(out, {"n": 2, "labels": ["Copy command", "Copy command"], "copied": ["git status", "echo hi && ls"]})


if __name__ == "__main__":
    unittest.main()

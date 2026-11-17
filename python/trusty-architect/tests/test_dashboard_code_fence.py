"""Item text code blocks (node, fake DOM): a fenced block renders as pre/code with its own copy button; inline backticks render as inline code.

Why: the owner 2026-10-29 (A50): a shell command written inline in an item's detail looked like stray text; commands must sit in a marked command block.
What: codeSplit + codeBox (the path linkify and addText take for detail, outcome, step detail and answer notes) build div>pre>code for a fence and span>code for a span; the button copies only the code text; all text goes in as text nodes, never HTML; excerptOf keeps a fence whole.
Test: tests/test_dashboard_code_fence.py
"""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import PAGES

JS = r"""
class N {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.children = []; this.className = ""; this.attrs = {}; this.listeners = {}; this._t = ""; this.tabIndex = -1;
    const self = this; this.classList = { add: (c) => { self.className += " " + c; }, remove: () => {} }; }
  set textContent(v) { this._t = String(v); this.children = []; } get textContent() { return this._t + this.children.map((c) => c.textContent).join(""); }
  appendChild(c) { this.children.push(c); return c; }
  setAttribute(k, v) { this.attrs[k] = v; }
  addEventListener(ev, f) { this.listeners[ev] = f; }
  get innerHTML() { throw new Error("innerHTML read"); } set innerHTML(v) { throw new Error("innerHTML written"); }
  find(pred, out = []) { if (pred(this)) out.push(this); this.children.forEach((c) => c.find(pred, out)); return out; }
}
const document = { createElement: (t) => new N(t) };
function el(tag, cls, text) { const e = new N(tag); if (cls) e.className = cls; if (text != null && text !== "") e.textContent = text; return e; }
const copied = [];
function copyText(v) { copied.push(v); return Promise.resolve(); }
const setTimeout = () => 0;
"""

BODY = r"""
const out = {};
const render = (s) => { const p = el("p", "detail"); for (const c of codeSplit(s)) { if (c.code != null) p.appendChild(codeBox(c.code, c.block)); else if (c.t) p.appendChild(el("span", null, c.t)); } return p; };
const tags = (n) => n.find((x) => x.tagName === "PRE" || x.tagName === "CODE").map((x) => x.tagName);
const p1 = render("Run this:\n```bash\ndashboard-db.py set items A50 --value -\n```\nthen check.");
out.fence = { tags: tags(p1), blocks: p1.find((x) => x.className.indexOf("codeblock") >= 0).map((x) => x.tagName), btns: p1.find((x) => x.attrs.role === "button").length, text: p1.find((x) => x.tagName === "CODE")[0].textContent };
const b = p1.find((x) => x.attrs.role === "button")[0]; b.listeners.click({ preventDefault() {}, stopPropagation() {} });
out.copied = copied.slice();
const p2 = render("use `git status` first");
out.inline = { tags: tags(p2), blocks: p2.find((x) => x.className.indexOf("codeblock") >= 0).length, inlineBox: p2.find((x) => x.tagName === "SPAN" && x.className.indexOf("codebox") === 0).length, text: p2.find((x) => x.tagName === "CODE")[0].textContent };
const p3 = render("```\n<script>alert(1)</script>\n```");
out.script = { code: p3.find((x) => x.tagName === "CODE")[0].textContent, scriptEls: p3.find((x) => x.tagName === "SCRIPT").length };
const fenceWithBlank = "Do:\n```\nline one\n\nline two\n```\n\nSecond paragraph.";
out.excerpt = excerptOf(fenceWithBlank);
out.plain = excerptOf("First.\n\nSecond.");
console.log(JSON.stringify(out));
"""


def blocks(src, names):
    parts = []
    for name in names:
        m = re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (name, name), src, re.S)
        assert m, name
        parts.append(m.group(1))
    return "\n".join(parts)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestCodeFence(unittest.TestCase):
    def run_page(self, page):
        src = open(page, encoding="utf-8").read()
        r = subprocess.run(["node", "-e", JS + blocks(src, ["codeSplit", "codeBox", "longText"]) + BODY], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_fence_renders_pre_code_with_copy_button_copying_only_the_block(self):
        for page in PAGES:
            with self.subTest(page=page):
                out = self.run_page(page)
                self.assertEqual(out["fence"]["tags"], ["PRE", "CODE"])
                self.assertEqual(out["fence"]["blocks"], ["DIV"])
                self.assertEqual(out["fence"]["btns"], 1)
                self.assertEqual(out["fence"]["text"], "dashboard-db.py set items A50 --value -")
                self.assertEqual(out["copied"], ["dashboard-db.py set items A50 --value -"])   # no fence, no trailing newline

    def test_inline_backticks_render_inline_code(self):
        for page in PAGES:
            with self.subTest(page=page):
                out = self.run_page(page)["inline"]
                self.assertEqual(out, {"tags": ["CODE"], "blocks": 0, "inlineBox": 1, "text": "git status"})

    def test_script_in_a_fence_stays_text(self):
        for page in PAGES:
            with self.subTest(page=page):
                out = self.run_page(page)["script"]
                self.assertEqual(out, {"code": "<script>alert(1)</script>", "scriptEls": 0})   # the fake DOM throws on any innerHTML use

    def test_excerpt_keeps_a_fence_whole(self):
        for page in PAGES:
            with self.subTest(page=page):
                out = self.run_page(page)
                self.assertEqual(out["excerpt"], "Do:\n```\nline one\n\nline two\n```" + " …")
                self.assertEqual(out["plain"], "First. …")

    def test_item_text_paths_use_codebox(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertEqual(len(re.findall(r"codeBox\(", src)), 3, "definition + linkify + addText")


if __name__ == "__main__":
    unittest.main()

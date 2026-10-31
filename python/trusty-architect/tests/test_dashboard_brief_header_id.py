"""Brief headers carry the durable id (the owner 2026-10-30): "MB0be9 · T1c JIRA to Linear status" in the reader header, the library list,
the item brief button and the status-brief label; a brief with no id shows the title alone. The server lists the id from the marker
line, or from the H1 prefix when there is no marker.

Test: tests/test_dashboard_brief_header_id.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

import test_dashboard_doc as _doc
write = _doc.write
from test_dashboard_progress import FAKE, PAGES


class TestBriefListId(unittest.TestCase):
    setUp, tearDown, get = _doc.TestDocRoute.setUp, _doc.TestDocRoute.tearDown, _doc.TestDocRoute.get

    def test_list_carries_id_from_marker_or_h1_prefix_and_strips_title(self):
        d = os.path.join(self.t, "acme", "docs", "briefs")
        write(os.path.join(d, "2026-10-30-m.md"), "<!-- brief-id: MB0be9 -->\n# MB0be9: T1c JIRA status\n\nbody\n")
        write(os.path.join(d, "2026-10-30-h.md"), "# MB1234: Only in H1\n\nbody\n")
        write(os.path.join(d, "2026-10-30-n.md"), "# No id here\n\nbody\n")
        rows = {r["path"].split("/")[-1]: r for r in json.loads(self.get("/briefs")[2])["briefs"]}
        self.assertEqual((rows["2026-10-30-m.md"]["brief_id"], rows["2026-10-30-m.md"]["title"]), ("MB0be9", "T1c JIRA status"))
        self.assertEqual((rows["2026-10-30-h.md"]["brief_id"], rows["2026-10-30-h.md"]["title"]), ("MB1234", "Only in H1"))
        self.assertEqual((rows["2026-10-30-n.md"]["brief_id"], rows["2026-10-30-n.md"]["title"]), (None, "No id here"))


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestBriefHeaderPage(unittest.TestCase):
    def js(self, body):
        src = open(PAGES[0], encoding="utf-8").read()
        parts = "".join(re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("statusBriefs",))
        head = re.search(r"  function briefHeading\(id, title\) \{.*?\n  \}\n", src, re.S).group(0)
        r = subprocess.run(["node", "-e", FAKE + parts + head + body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_heading_puts_id_before_title_and_omits_when_absent(self):
        out = self.js("""console.log(JSON.stringify([briefHeading("MB0be9", "T1c JIRA to Linear status"), briefHeading("", "Title"), briefHeading(null, "Title"),
          briefHeading("B0be9", "T"), briefHeading("MB0be9", "MB0be9 already"), statusBriefLabel({ id: "MB0be9", at: "2026-10-30T12:00:00Z" }, "T1c status"),
          statusBriefLabel({ id: "MB0be9", at: "2026-10-30T12:00:00Z" }, "")]));""")
        self.assertEqual(out, ["MB0be9 · T1c JIRA to Linear status", "Title", "Title", "MB0be9 · T",
                               "MB0be9 already", "MB0be9 · T1c status · 2026-10-30", "MB0be9 · 2026-10-30"])

    def test_reader_header_list_row_and_buttons_use_the_id(self):
        src = open(PAGES[0], encoding="utf-8").read()
        sd = src[src.index("  function setDocId(id)"):src.index("  /* END docid */")]
        self.assertIn('$("rd-title").textContent = briefHeading(rdId, rdBase)', sd)   # header re-rendered when the id arrives
        self.assertNotIn('$("rd-title").textContent = title', src)                     # no path bypasses rdSetTitle
        self.assertIn('id: r.brief_id || ""', src)                                      # list rows carry the id; rowRef shows it before the title
        self.assertIn('"Read the brief: " + (l.id ? l.id + " \\u00B7 " : "") + l.title', src)
        self.assertIn("briefTitlesEnsure();\n    const b = el(\"button\", \"read-btn sb-link\"", src)

    def test_reader_header_text_follows_the_id(self):
        src = open(PAGES[0], encoding="utf-8").read()
        blk = re.search(r"/\* BEGIN docid \*/(.*?)/\* END docid \*/", src, re.S).group(1)
        body = """const nodes = { 'rd-id': el('button'), 'rd-idline': el('div'), 'rd-title': el('span') }; const $ = (i) => nodes[i];
        """ + blk + """
        const t = () => $("rd-title").textContent, out = [];
        rdSetTitle("T1c JIRA to Linear status"); out.push(t());      // loading: no id yet
        setDocId("MB0be9"); out.push(t());                           // id arrives
        setDocId(""); out.push(t());                                 // brief without an id
        console.log(JSON.stringify(out));"""
        self.assertNotRegex(blk, r"(?<![A-Za-z_])txt\(\s*[a-z]")   # the reader IIFE has no txt(); this block runs without FAKE's txt below
        stub = "function el(tag) { return { tagName: tag, textContent: '', hidden: false }; }\n"
        r = subprocess.run(["node", "-e", stub + body], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), ["T1c JIRA to Linear status", "MB0be9 \u00B7 T1c JIRA to Linear status", "T1c JIRA to Linear status"])


if __name__ == "__main__":
    unittest.main()

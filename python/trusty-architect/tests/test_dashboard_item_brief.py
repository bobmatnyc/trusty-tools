"""Dashboard briefs on decisions and asks (owner 2026-10-14 03:36Z), on both HTML files (node, no browser).

Why: long decisions link to a brief instead of carrying a paragraph; the standalone Briefs section is gone.
What: no sec-briefs section; "Read brief" / chip when `brief` is set; "Ask for a brief" writes brief_request;
the schema accepts both fields; pending-answers reports brief_request until `brief` is set; the fleet project
names resolve in the /doc allow-list.
Test: scripts/test_dashboard_item_brief.py
"""
import tempfile
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_derive as derive  # noqa: E402
import dashboard_schema as schema  # noqa: E402
from test_dashboard_progress import FAKE, PAGES, ROOT  # noqa: E402


def run(page, body):
    src = open(page, encoding="utf-8").read()
    brief = re.search(r"/\* BEGIN brief \*/(.*?)/\* END brief \*/", src, re.S).group(1)
    docof = re.search(r"^  function docOf\(ref\).*$", src, re.M).group(0)
    js = FAKE + """
const askKind = (it) => (/^([DAT])/.exec(it.id) || [])[1];
const readerButton = (label, fn) => { const b = el("button", "read-btn", label); b.listeners.click = fn; return b; };
let db = { collection: () => ({ doc: () => ({ get: async () => ({ version: 7 }) }) }) }, readOnly = false;
const userName = () => "owner";
let opened = null; const window = { DocReader: { open: (p, f) => { opened = [p, f]; } } };
let written = null; async function write(id, patch, opts) { written = { id, patch, opts }; return true; }
""" + docof + brief + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestItemBrief(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=page):
                yield run(page, body)

    def test_no_standalone_briefs_section(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertNotIn('id="sec-briefs"', src)
            self.assertNotIn("renderBriefs", src)
            self.assertNotIn('collection("briefs")', src)

    def test_brief_set_shows_read_button_that_opens_reader(self):
        body = """const body = el("div"); const it = { id: "D01", status: "open", brief: "mcp-services/docs/briefs/2191-x.md" };
        briefControl(body, it); const btns = body.find((n) => n.tagName === "button"); btns[0].listeners.click();
        console.log(JSON.stringify([btns.map((b) => b.textContent), opened, !!briefDoc(it)]));"""
        for out in self.each(body):
            self.assertEqual(out, [["Read brief"], ["mcp-services", "docs/briefs/2191-x.md"], True])

    def test_missing_brief_offers_request_and_writes_brief_request(self):
        body = """const body = el("div"); const it = { id: "A02", status: "open" };
        briefControl(body, it); const b = body.find((n) => n.tagName === "button")[0];
        const before = b.textContent; b.listeners.click(); await new Promise((r) => setTimeout(r, 20));
        console.log(JSON.stringify([before, b.textContent, b.disabled, written.id, Object.keys(written.patch).sort(),
          Object.keys(written.patch.brief_request).sort(), written.patch.brief_request.by, written.opts]));"""
        body = "(async () => {" + body + "})();"
        for out in self.each(body):
            self.assertEqual(out, ["Ask for a brief", "Brief requested", True, "A02", ["brief_request", "updated"], ["at", "by"], "owner", {"if_version": 7}])

    def test_no_button_when_requested_task_or_not_open(self):
        body = """const f = (it) => { const b = el("div"); briefControl(b, it); return b.find((n) => n.tagName === "button").map((x) => [x.textContent, x.disabled]); };
        console.log(JSON.stringify([f({ id: "D01", status: "open", brief_request: { at: "t", by: "owner" } }), f({ id: "T01", status: "open" }),
          f({ id: "D02", status: "done" }), f({ id: "D03", status: "open", brief: "bad path", brief_request: null })]));"""
        for out in self.each(body):
            self.assertEqual(out[0], [["Brief requested", True]])
            self.assertEqual(out[1:3], [[], []])
            self.assertEqual(out[3], [["Ask for a brief", False]])   # an invalid brief path counts as none

    def test_schema_accepts_brief_fields(self):
        base = {"session": "tm-x", "goal": None, "rank": 1, "kind": "question", "status": "open", "title": "t", "created": "2026-10-14T00:00:00Z", "updated": "2026-10-14T00:00:00Z"}
        ok = dict(base, brief="architect/docs/briefs/x.md", brief_request={"at": "2026-10-14T03:00:00Z", "by": "owner"})
        self.assertEqual(schema.validate("items", "D01", ok), [])
        self.assertTrue(schema.validate("items", "D01", dict(base, brief="nope.md")))
        self.assertTrue(schema.validate("items", "D01", dict(base, brief_request="yes")))

    def test_pending_reports_brief_request_until_brief_set(self):
        def docs(**extra):
            d = {"id": "D01", "status": "open", "title": "Pick one"}
            d.update(extra)
            return {"items": [{"id": "D01", "version": 1, "data": d}], "sessions": [], "goals": [], "effort_tunes": []}
        req = {"at": "2026-10-14T03:40:00Z", "by": "owner"}
        got = [p for p in self._pending(docs(brief_request=req)) if p["kind"] == "brief_request"]
        self.assertEqual(got, [{"id": "D01", "kind": "brief_request", "at": req["at"], "text": "Pick one"}])
        self.assertEqual([p for p in self._pending(docs(brief_request=req, brief="architect/docs/briefs/x.md")) if p["kind"] == "brief_request"], [])

    def _pending(self, snap):
        from datetime import datetime, timezone
        sig = derive.pending.__code__.co_varnames
        self.assertIn("now", sig)
        return derive.pending(self._as_snapshot(snap), datetime.now(timezone.utc))

    def _as_snapshot(self, snap):
        import tempfile
        import dashboard_store as ds
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        os.makedirs(os.path.join(tmp.name, "items"))
        for d in snap["items"]:
            with open(os.path.join(tmp.name, "items", d["id"] + ".json"), "w") as f:
                json.dump(dict(d["data"], version=1), f)
        return derive.snapshot(ds.Store(tmp.name), ("items", "sessions", "effort_tunes", "goals"))

    def test_fleet_project_names_resolve_for_the_reader(self):
        spec = importlib.util.spec_from_file_location("dashboard_serve", os.path.join(SCRIPTS, "dashboard-serve.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        names = ("architect", "mcp-services", "apex", "apex-companion", "trusty-tools", "mpm-utils", "cto")
        with tempfile.TemporaryDirectory() as tmp:
            os.makedirs(os.path.join(tmp, "local", "projects"))
            for n in names[1:]:
                os.makedirs(os.path.join(tmp, "roots", n))
                with open(os.path.join(tmp, "local", "projects", n + ".md"), "w") as f:
                    f.write("- Root: `" + os.path.join(tmp, "roots", n) + "`\n")
            projects = mod.load_projects(tmp)
        for name in names:
            self.assertIn(name, projects)


if __name__ == "__main__":
    unittest.main()

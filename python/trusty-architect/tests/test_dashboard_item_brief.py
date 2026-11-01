"""Dashboard briefs on decisions and asks (owner 2026-10-14 03:36Z), on both HTML files (node, no browser).

Why: long decisions link to a brief instead of carrying a paragraph; the standalone Briefs section is gone.
What: no sec-briefs section; "Brief" link next to a "Request update" button when `brief` is set; "Request brief" writes brief_request;
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
N.prototype.insertBefore = function (c) { this.children.unshift(c); }; N.prototype.classList = { add() {} };
Object.defineProperty(N.prototype, "firstChild", { get() { return this.children[0] || null; } });
const document = { createElementNS: (ns, t) => new N(t), querySelectorAll: () => [] };
N.prototype.querySelector = function (s) { return this.find((n) => n.className === s.slice(1))[0]; }; N.prototype.removeAttribute = function (k) { delete this.attrs[k]; };
const fetch = () => new Promise(() => {});
const userName = () => "owner"; const errFor = () => ({});
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

    def test_brief_set_shows_slot_button_that_opens_reader(self):
        body = """const body = el("div"); const it = { id: "D01", status: "open", brief: "mcp-services/docs/briefs/2191-x.md" };
        briefControl(body, it); const btns = body.find((n) => n.tagName === "button"); const slot = briefSlot(it.brief), sb = slot.find((n) => n.tagName === "button")[0]; sb.listeners.click({ preventDefault() {}, stopPropagation() {} });
        console.log(JSON.stringify([btns.map((b) => b.textContent), opened, !!briefDoc(it), sb.textContent, sb.attrs.title || sb.title, slot.className]));"""
        for out in self.each(body):
            self.assertEqual(out, [["Request update"], ["mcp-services", "docs/briefs/2191-x.md"], True, "2191-x", "mcp-services/docs/briefs/2191-x.md", "brief-slot"])

    def test_brief_link_carries_a_labelled_document_icon(self):
        body = """const slot = briefSlot("architect/local/briefs/x.md"); const b = slot.find((n) => n.tagName === "button")[0]; const svg = b.find((n) => n.tagName === "svg")[0];
        console.log(JSON.stringify([b.textContent, b.children[0].tagName, b.children[1].className, b.children[1].children.map((c) => c.className), svg.attrs["aria-hidden"], b.attrs["aria-label"], briefSlot("not-a-brief")]));"""
        for out in self.each(body):
            self.assertEqual(out, ["x", "svg", "brief-txt", ["brief-id", "brief-t"], "true", "Read the brief: x", None])

    def test_brief_slot_shows_id_above_title_only_when_present(self):
        body = """const slot = briefSlot("architect/local/briefs/x.md"), b = slot.find((n) => n.tagName === "button")[0];
        const idEl = b.find((n) => n.className === "brief-id")[0], tEl = b.find((n) => n.className === "brief-t")[0], tx = b.find((n) => n.className === "brief-txt")[0];
        const none = [idEl.textContent, idEl.hidden, b.title];
        briefTitles.set("architect/local/briefs/x.md", { title: "Nice", brief_id: "MB0b78" }); briefSlotFill(b);
        console.log(JSON.stringify([none, [idEl.textContent, idEl.hidden, tEl.textContent, b.title], tx.children.map((c) => c.className)]));"""
        for out in self.each(body):
            self.assertEqual(out[0], ["", True, "architect/local/briefs/x.md"])                     # no id: nothing shown
            self.assertEqual(out[1], ["MB0b78", False, "Nice", "architect/local/briefs/x.md \u00b7 MB0b78"])
            self.assertEqual(out[2], ["brief-id", "brief-t"])                                       # id above (before) the title

    def test_missing_brief_offers_request_and_writes_brief_request(self):
        body = """const body = el("div"); const it = { id: "A02", status: "open" };
        briefControl(body, it); const b = body.find((n) => n.tagName === "button")[0];
        const before = b.textContent; b.listeners.click(); await new Promise((r) => setTimeout(r, 20));
        console.log(JSON.stringify([before, b.textContent, b.disabled, written.id, Object.keys(written.patch).sort(),
          Object.keys(written.patch.brief_request).sort(), written.patch.brief_request.by, written.opts]));"""
        body = "(async () => {" + body + "})();"
        for out in self.each(body):
            self.assertEqual(out, ["Request brief", "Brief requested", True, "A02", ["brief_request", "status", "updated"], ["at", "by"], "owner", {"if_version": 7}])

    def test_no_button_when_requested_task_or_not_open(self):
        body = """const f = (it) => { const b = el("div"); briefControl(b, it); return b.find((n) => n.tagName === "button").map((x) => [x.textContent, x.disabled]); };
        console.log(JSON.stringify([f({ id: "D01", status: "open", brief_request: { at: "t", by: "owner" } }), f({ id: "T01", status: "open" }),
          f({ id: "D02", status: "done" }), f({ id: "D03", status: "open", brief: "bad path", brief_request: null })]));"""
        for out in self.each(body):
            self.assertEqual(out[0], [["Brief requested", True]])
            self.assertEqual(out[1:3], [[], []])
            self.assertEqual(out[3], [["Request brief", False]])   # an invalid brief path counts as none

    def test_refresh_and_goal_requests(self):
        body = """(async () => { const out = [];
        const it = { id: "D01", status: "open", brief: "mcp-services/docs/briefs/2191-x.md" };
        const b = el("div"); briefControl(b, it); const r = b.find((n) => n.tagName === "button")[0];
        r.listeners.click(); await new Promise((x) => setTimeout(x, 20));
        out.push([r.textContent, r.disabled, written.id, Object.keys(written.patch).sort(), written.patch.brief_request.refresh]);
        const done = el("div"); briefControl(done, Object.assign({}, it, { brief_request: { at: "t", by: "o", refresh: true } }));
        out.push(done.find((n) => n.tagName === "button").map((x) => [x.textContent, x.disabled]));
        let gw = null; db = { collection: (c) => ({ doc: (i) => ({ get: async () => ({ version: 3 }), update: async (p, o) => { gw = [c, i, Object.keys(p).sort(), o]; } }) }) };
        const bar = el("div"); briefControl(bar, { id: "G01", status: "active" }, bar, "directed");
        const gb = bar.find((n) => n.tagName === "button")[0]; const gl = gb.textContent; gb.listeners.click(); await new Promise((x) => setTimeout(x, 20));
        out.push([gl, gw]);
        console.log(JSON.stringify(out)); })();"""
        for out in self.each(body):
            self.assertEqual(out[0], ["Update requested", True, "D01", ["brief_request", "updated"], True])
            self.assertEqual(out[1], [["Update requested", True]])
            self.assertEqual(out[2], ["Request brief", ["goals", "G01", ["brief_request", "updated"], {"if_version": 3}]])

    def test_brief_set_beats_stray_request_object_and_local_briefs_resolve(self):
        # A2f 2026-10-29: brief_request {__delete__: true} (no `at`) is not a request; architect/local/briefs/ links open the reader.
        body = """const f = (it) => { const b = el("div"); briefControl(b, it); return b.find((n) => n.tagName === "button").map((x) => [x.textContent, x.disabled]); };
        console.log(JSON.stringify([f({ id: "A2f", status: "open", brief: "architect/local/briefs/x.md", brief_request: { __delete__: true } }),
          f({ id: "A2f", status: "open", brief_request: { __delete__: true } }), docOf("architect/local/briefs/x.md")]));"""
        for out in self.each(body):
            self.assertEqual(out[0], [["Request update", False]])
            self.assertEqual(out[1], [["Request brief", False]])
            self.assertEqual(out[2], {"project": "architect", "path": "local/briefs/x.md"})

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


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestReaderDocId(unittest.TestCase):
    """MB id in the reader header chip and above the title; nothing without an id (the owner 2026-10-29)."""
    def run_js(self, page, body):
        src = open(page, encoding="utf-8").read()
        blk = re.search(r"/\* BEGIN docid \*/(.*?)/\* END docid \*/", src, re.S).group(1)
        js = FAKE + "const nodes = { 'rd-id': el('button'), 'rd-idline': el('div'), 'rd-title': el('span') }; const $ = (i) => nodes[i];\n" + blk + "\n" + body
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        assert r.returncode == 0, r.stderr
        return json.loads(r.stdout)

    def test_chip_and_line_show_id_and_hide_without_one(self):
        body = """const lib = { briefs: [{ project: "p", path: "a.md", brief_id: "MB0b78" }, { project: "p", path: "b.md" }], media: [{ project: "p", path: "c.png", id: "MB0001" }] };
        const ids = [["p", "a.md"], ["p", "b.md"], ["p", "c.png"], ["p", "zz.md"]].map((x) => docIdFor(lib, x[0], x[1]));
        setDocId(ids[0]); const on = [$("rd-id").textContent, $("rd-id").hidden, $("rd-idline").textContent, $("rd-idline").hidden];
        setDocId(ids[1]); const off = [$("rd-id").textContent, $("rd-id").hidden, $("rd-idline").textContent, $("rd-idline").hidden];
        console.log(JSON.stringify([ids, on, off]));"""
        for page in PAGES:
            with self.subTest(page=page):
                ids, on, off = self.run_js(page, body)
                self.assertEqual(ids, ["MB0b78", "", "MB0001", ""])
                self.assertEqual(on, ["MB0b78", False, "MB0b78", False])
                self.assertEqual(off, ["", True, "", True])

    def test_markup_places_chip_in_bar_and_line_above_article(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertTrue(re.search(r'class="rd-bar">.*?id="rd-id".*?class="rd-title"', src, re.S))
            self.assertRegex(src, r'id="rd-idline" hidden></div><article class="md" id="rd-md"')

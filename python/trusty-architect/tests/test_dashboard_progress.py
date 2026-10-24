"""Dashboard page logic (run under node, skipped when node is absent), checked on both HTML files:
row progress fill, session rollup, owner cancel, update pulse, plans and links."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]

# A minimal DOM: enough for el(), planBlock(), linksRow() and linkedSteps().
FAKE = r"""
class N {
  constructor(tag) { this.tagName = tag; this.children = []; this.className = ""; this.dataset = {}; this.attrs = {}; this.listeners = {}; this._t = ""; }
  set textContent(v) { this._t = String(v); } get textContent() { return this._t + this.children.map((c) => c.textContent).join(""); }
  appendChild(c) { this.children.push(c); return c; }
  append(...cs) { cs.forEach((c) => this.children.push(typeof c === "string" ? Object.assign(new N("#text"), { _t: c }) : c)); }
  setAttribute(k, v) { this.attrs[k] = v; }
  addEventListener(ev, f) { this.listeners[ev] = f; }
  find(pred, out = []) { if (pred(this)) out.push(this); this.children.forEach((c) => c.find(pred, out)); return out; }
}
function el(tag, cls, text) { const e = new N(tag); if (cls) e.className = cls; if (text != null && text !== "") e.textContent = text; return e; }
function txt(v) { return v == null ? "" : typeof v === "string" ? v : String(v); }
function rich(tag, cls, text) { return el(tag, cls, txt(text)); }
function longField(p, text) { p.textContent = txt(text); return p; }
const tag = (root, t) => root.find((n) => n.tagName === t);
const isArchItem = () => false;   /* page helper, stubbed */
"""


def run(page, body, check):
    src = open(page, encoding="utf-8").read()
    parts = []
    for name in check:
        m = re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (name, name), src, re.S)
        assert m, name
        parts.append(m.group(1))
    js = FAKE + "\n".join(parts) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardProgress(unittest.TestCase):
    def each(self, body, check=("progressCore",)):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body, check)

    def test_percent_from_steps_then_explicit_progress(self):
        body = """
        const steps = [{ id: "a" }, { id: "b" }, { id: "c" }, { title: "no id" }];
        console.log(JSON.stringify([
          taskProgress({ steps }, { a: { done: true }, "4": { done: true } }),
          taskProgress({ steps, progress: 90 }, {}),
          taskProgress({ progress: 40 }, {}), taskProgress({ progress: 400 }, {}), taskProgress({}, {}), taskProgress({ progress: "" }, {})]));"""
        for r in self.each(body):
            self.assertEqual(r[0], {"done": 2, "total": 4, "pct": 50})
            self.assertEqual(r[1]["pct"], 0)          # steps win over an explicit progress
            self.assertEqual(r[2]["pct"], 40)
            self.assertEqual(r[3]["pct"], 100)        # clamped
            self.assertIsNone(r[4])
            self.assertIsNone(r[5])

    def test_session_rollup_summary_and_aggregate(self):
        body = """
        const t1 = { id: "T01", title: "Ship A", steps: [{ id: "1" }, { id: "2" }] }, t2 = { id: "T02", title: "Ship B", steps: [{ id: "1" }, { id: "2" }, { id: "3" }, { id: "4" }, { id: "5" }, { id: "6" }] };
        const maps = { T01: { "1": { done: true }, "2": { done: true } }, T02: { "1": { done: true } } };
        const mapOf = (id) => maps[id] || {};
        console.log(JSON.stringify([
          sessionRollup({}, [t1, t2], mapOf), sessionRollup({ summary: " Own line ", progress: 12.4 }, [t1, t2], mapOf),
          sessionRollup({}, [], mapOf), sessionRollup({ progress: 0 }, [t1], mapOf)]));"""
        for r in self.each(body):
            self.assertEqual(r[0], {"summary": "Ship A · Ship B", "pct": 38})   # (2+1)/(2+6)
            self.assertEqual(r[1], {"summary": "Own line", "pct": 12})
            self.assertEqual(r[2], {"summary": "", "pct": None})
            self.assertEqual(r[3]["pct"], 0)

    def test_cancel_writes_fields_and_moves_to_done_bucket(self):
        body = """
        const now = "2026-10-08T21:00:00.000Z";
        const patch = cancelPatch("  not needed  ", "owner", now);
        const list = [{ id: "T01", status: "open" }, { id: "T02", status: "cancelled", cancelled_at: "2026-10-08T20:00:00Z" },
                      { id: "T03", status: "done", done_at: "2026-10-08T19:00:00Z" }, { id: "T04", status: "answered", answered_at: "x" }];
        const b = bucketItems(list);
        console.log(JSON.stringify([patch, cancelPatch(null, null, now), b.open.map((i) => i.id), b.done.map((i) => i.id), b.answered.map((i) => i.id)]));"""
        for r in self.each(body):
            self.assertEqual(r[0], {"status": "cancelled", "cancelled_at": "2026-10-08T21:00:00.000Z", "cancel_note": "not needed",
                                    "cancelled_by": "owner", "updated": "2026-10-08T21:00:00.000Z"})
            self.assertEqual((r[1]["cancel_note"], r[1]["cancelled_by"]), ("", ""))
            self.assertEqual(r[2], ["T01"])             # counts exclude cancelled
            self.assertEqual(r[3], ["T02", "T03"])      # cancelled sits in the closed bucket, newest first
            self.assertEqual(r[4], ["T04"])

    def test_changed_doc_marks_row_unchanged_does_not(self):
        body = """
        const a = { id: "T01", status: "open", updated: "1" }, b = { id: "T02", status: "open", updated: "1" };
        const first = changedDocs(null, [a, b]);
        const same = changedDocs(first.next, [a, b]);
        const edit = changedDocs(first.next, [Object.assign({}, a, { updated: "2" }), b, { id: "T03", updated: "1" }]);
        console.log(JSON.stringify([first.changed, same.changed, edit.changed]));"""
        for r in self.each(body):
            self.assertEqual(r[0], [])                  # first load does not pulse
            self.assertEqual(r[1], [])                  # unchanged doc does not pulse
            self.assertEqual(r[2], ["T01", "T03"])      # changed and new docs do

    def test_hidden_row_pulses_outermost_closed_ancestor(self):
        body = """console.log(JSON.stringify([pulseIndex([]), pulseIndex([true, true]), pulseIndex([false, true]),
          pulseIndex([true, false, true]), pulseIndex([false, true, false])]));"""
        for r in self.each(body):
            self.assertEqual(r, [0, 0, 1, 2, 3])        # 0 = the row itself; k = k-th ancestor, outermost closed wins

    def test_plan_renders_as_ordered_list(self):
        body = """
        const box = el("div");
        planBlock(box, { plan_title: "Rollout", plan: [{ title: "First", detail: "why" }, "Second", { title: "Third", done: true }, { detail: "no title" }] });
        const ol = tag(box, "ol"), lis = tag(box, "li");
        console.log(JSON.stringify([ol.length, ol[0].className, lis.map((l) => l.className), lis[0].find((n) => n.tagName === "strong")[0].textContent,
          lis[0].find((n) => n.className === "plan-detail")[0].textContent, lis[2].textContent, box.children[0].children[0].textContent]));
        const empty = el("div"); planBlock(empty, { plan: [] }); console.log(JSON.stringify(empty.children.length));"""
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertEqual(src.count("    planExtras(body, it);"), 7, page)   # 5 item renderers + the cancelled and Won't do rows
            r = run_lines(page, body, ("progressCore", "docPaths", "planView"))
            self.assertEqual(r[0][:2], [1, "plan-list"])
            self.assertEqual(r[0][2], ["plan-step", "plan-step", "plan-step done"])
            self.assertEqual(r[0][3:5], ["First", "why"])
            self.assertIn("✓ done", r[0][5])
            self.assertEqual(r[0][6], "Rollout")
            self.assertEqual(r[1], 0)

    def test_links_resolve_and_linked_task_steps_render(self):
        body = """
        const BRIEFS_ABS = "/srv/architect/local/briefs/";
        const ids = new Set(["T06", "D3f"]);
        const links = [{ label: "Task", item: "T06" }, { item: "T99" }, { url: "https://example.com/x" }, { url: "javascript:alert(1)" },
                       { url: "local/briefs/a&b.md", label: "Brief" }, { url: "/srv/architect/local/briefs/sub/c.md" }, { url: "local/briefs/../x.md" }];
        const ls = linksOf({ links }, ids);
        const row = el("div"); let jumped = null;
        linksRow(row, { links }, ids, (id) => { jumped = id; });
        const btn = row.find((n) => n.tagName === "button")[0]; btn.listeners.click();
        const t = { id: "T06", title: "Do it", steps: [{ id: "1", title: "one" }, { id: "2", title: "two" }, { title: "three" }] };
        const out = el("div");
        linkedSteps(out, { links }, new Map([["T06", t]]), (id) => ({ "1": { done: true } }));
        const lis = tag(out, "li");
        console.log(JSON.stringify([ls.map((l) => [l.kind, l.item || l.href]), jumped, btn.textContent, out.children[0].children[0].textContent,
          lis.map((l) => l.className), tag(out, "progress").length, tag(out, "input").length]));
        const none = el("div"); linkedSteps(none, { links: [{ item: "D3f" }] }, new Map([["D3f", { id: "D3f" }]]), () => ({})); console.log(none.children.length);"""
        for page in PAGES:
            r = run_lines(page, body, ("progressCore", "docPaths", "planView"))
            kinds = r[0][0]
            self.assertEqual(kinds[0], ["item", "T06"])            # an item id resolves
            self.assertEqual(kinds[1], ["missing", "T99"])         # an unknown id is shown, not linked
            self.assertEqual(kinds[2], ["url", "https://example.com/x"])
            self.assertEqual(kinds[3], ["url", "/files/local/briefs/a%26b.md"])   # briefs go through /files
            self.assertEqual(kinds[4], ["url", "/files/local/briefs/sub/c.md"])
            self.assertEqual(len(kinds), 5)                        # javascript: and ../ are dropped
            self.assertEqual(r[0][1], "T06")                       # clicking jumps to the item
            self.assertEqual(r[0][2], "Task")
            self.assertEqual(r[0][3], "T06 · Do it")               # plain label, no count
            self.assertEqual(r[0][5:], [0, 0])                     # no progress element
            self.assertEqual(r[0][4], ["lstep done", "lstep", "lstep"])
            self.assertEqual(r[1], 0)                              # a linked item without steps adds nothing

    def test_grouped_link_renders_label_then_sources_in_parentheses(self):
        body = """
        const g = { label: "Deep analysis index", sources: [{ label: "localhost", url: "http://127.0.0.1:7890/#doc/x" }, { label: "bad", url: "javascript:alert(1)" }, { label: "gdoc", url: "https://docs.google.com/document/d/1" }] };
        const row = el("div");
        linksRow(row, { links: [g, { label: "empty", sources: [{ label: "bad", url: "ftp://x" }] }, { label: "Plain", url: "https://example.com/p" }] }, new Set(), () => {});
        const a = tag(row, "a");
        console.log(JSON.stringify([row.textContent, a.map((n) => [n.textContent, n.attrs.href || n.href])]));"""
        for page in PAGES:
            r = run_lines(page, body, ("progressCore", "docPaths", "planView"))
            self.assertEqual(r[0][0], "Linked:Deep analysis index (localhost, gdoc)Plain")   # invalid source dropped; group with none dropped
            self.assertEqual([x[0] for x in r[0][1]], ["localhost", "gdoc", "Plain"])

    def test_task_steps_are_read_only_and_ask_steps_keep_checkboxes(self):
        body = """
        const steps = [{ id: "1", title: "one" }, { id: "2", title: "two" }];
        const stepsOf = () => steps, stepMap = () => ({ "1": { done: true } });
        const doneCount = () => 1, stepRow = () => el("div", "step");
        const storedKind = (it) => it.id[0];
        const t = el("div"); stepsBlock(t, { id: "T01" });
        const a = el("div"); stepsBlock(a, { id: "A01" });
        console.log(JSON.stringify([t.textContent, tag(t, "progress").length, tag(t, "input").length, tag(t, "li").map((l) => l.className),
          a.textContent.includes("Steps ·"), a.find((n) => n.className === "step").length]));"""
        for page in PAGES[:1]:
            r = run_lines(page, body, ("progressCore", "stepsView"))[0]
            self.assertEqual(r[0], "onetwo")
            self.assertEqual(r[1:3], [0, 0])                       # no progress bar, no checkbox on a task
            self.assertEqual(r[3], ["tstep done", "tstep"])        # done step struck through
            self.assertTrue(r[4])                                  # an ask keeps its header
            self.assertEqual(r[5], 2)                              # and its step rows
        css = open(PAGES[0], encoding="utf-8").read()
        self.assertIn(".tstep.done { color: var(--muted); text-decoration: line-through; }", css)

    def test_rows_show_fill_only_no_count_or_percent_text(self):
        for page in PAGES:
            s = open(page, encoding="utf-8").read()
            self.assertNotIn('el("span", "pct"', s, page)        # no visible NN% text on task or session rows
            self.assertNotIn('"chip steps"', s, page)            # no visible N/M chip
            self.assertEqual(s.count("sum.appendChild(pbar("), 1, page)   # task row carries the label node; the session row bar is context use (test_dashboard_sessionbars)
            self.assertIn('"aria-label", "progress about "', s, page)
            self.assertEqual(s.count('classList.add("has-pct")'), 1, page)   # the left-to-right fill stays

    def test_both_pages_carry_the_features(self):
        for page in PAGES:
            s = open(page, encoding="utf-8").read()
            for needle in ("data-pid", "Cancel task", "Confirm cancel", "cancel_note", "has-pct", "sess-sum", "trackDocs(", "applyPulses();",
                           "@media (prefers-reduced-motion: reduce) { .pulse::after", "ownerState", "bucketItems(items)"):
                self.assertIn(needle if needle != "data-pid" else "dataset.pid", s, (page, needle))


def run_lines(page, body, check):
    """Like run(), but the body prints one JSON document per line."""
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in check]
    r = subprocess.run(["node", "-e", FAKE + "\n".join(parts) + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return [json.loads(x) for x in r.stdout.splitlines() if x.strip()]


if __name__ == "__main__":
    unittest.main()

"""Dashboard Won't do control (brief 2026-10-09-dashboard-wont-do.md), checked on both HTML files under node
(skipped when node is absent): the control renders on A and D but not T; Send with a reason writes the fields
pinned with if_version; empty sends persist the count and the 3rd closes with reason_missing; counts exclude
wont_do items; the reopened chip shows while wont_do_history is non-empty."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1)
             for n in ("askKind", "wontDo", "reminder", "completedPage")]
    bucket = re.search(r"  function bucketItems\(list\) \{.*?\n  \}\n", src, re.S).group(0)
    js = FAKE + """
let db = { collection: () => ({ doc: () => ({ get: async () => ({ version: 7 }) }) }) }, readOnly = false;
const userName = () => "owner"; const renders = []; const render = () => renders.push(1);
const claimForm = () => {}, recall = () => undefined, formStash = new Map();
const collapsed = []; const collapseItem = (c, id) => collapsed.push(id); const reopenItem = () => {}; const rowFor = () => null;
const writes = []; let failNext = false;
async function write(id, patch, opts) { writes.push({ id, patch, opts }); return !failNext; }
""" + "\n".join(parts) + bucket + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardWontDo(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body)

    def test_control_renders_on_decision_and_ask_and_task_renderer_does_not_call_it(self):
        body = """
        const out = {};
        for (const it of [{ id: "D01", status: "open" }, { id: "A02", status: "open" }, { id: "A04", status: "answered" }]) {
          const b = el("div"); wontDoControl(b, it);
          out[it.id] = b.find((n) => n.tagName === "button").map((n) => n.textContent);
        }
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, {"D01": ["Won't do"], "A02": ["Won't do"], "A04": []})
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            fn = lambda name: re.search(r"  function %s\(it\) \{.*?\n  \}\n" % name, src, re.S).group(0)
            self.assertIn("wontDoControl(body, it, tools)", fn("renderQuestion"))
            self.assertNotIn("wontDoControl", fn("renderAction"))   # actions carry Won't Do on the primary row
            self.assertNotIn("wontDoControl", fn("renderTask"))

    def test_primary_row_wont_do_writes_wont_do_with_note_and_version(self):
        body = """
        (async () => {
          const it = { id: "A55", status: "open" };
          const ok = await wontDoPrimarySend(it, "not needed");
          await wontDoPrimarySend(it, "");
          console.log(JSON.stringify({ ok, w: writes.map((x) => ({ id: x.id, opts: x.opts, status: x.patch.status, wd: x.patch.wont_do })) }));
        })();"""
        for r in self.each(body):
            self.assertTrue(r["ok"])
            a, b = r["w"]
            self.assertEqual((a["id"], a["opts"], a["status"]), ("A55", {"if_version": 7}, "wont_do"))
            self.assertEqual((a["wd"]["note"], a["wd"]["by"], a["wd"]["attempts"]), ("not needed", "owner", 1))
            self.assertTrue(a["wd"]["at"])
            self.assertNotIn("reason_missing", a["wd"])
            self.assertTrue(b["wd"]["reason_missing"])

    def test_form_has_prompt_textarea_send_and_back(self):
        body = """
        const it = { id: "A11", status: "open" }; const b = el("div");
        wontDoControl(b, it); b.find((n) => n.tagName === "button")[0].listeners.click();
        const b2 = el("div"); wontDoControl(b2, it);
        console.log(JSON.stringify({ lab: tag(b2, "label")[0].textContent, ta: tag(b2, "textarea").length,
          btns: b2.find((n) => n.tagName === "button").map((n) => n.textContent), msg: b2.find((n) => n.className.indexOf("wd-msg") >= 0)[0].textContent }));"""
        for r in self.each(body):
            self.assertEqual(r["lab"], "Why won't you? (the Architect acts on the reason)")
            self.assertEqual((r["ta"], r["btns"], r["msg"]), (1, ["Send", "Back"], ""))

    def test_send_with_reason_closes_with_if_version(self):
        body = """
        (async () => {
          await wontDoSend({ id: "A11", status: "open" }, { note: "", attempts: 0 }, "  Not needed  ");
          console.log(JSON.stringify(writes));
        })();"""
        for r in self.each(body):
            self.assertEqual(len(r), 1)
            self.assertEqual(r[0]["id"], "A11")
            self.assertEqual(r[0]["opts"], {"if_version": 7})
            p = r[0]["patch"]
            self.assertEqual(p["status"], "wont_do")
            self.assertEqual({k: p["wont_do"][k] for k in ("note", "by", "attempts")}, {"note": "Not needed", "by": "owner", "attempts": 1})
            self.assertTrue(p["wont_do"]["at"] and p["updated"] == p["wont_do"]["at"])
            self.assertNotIn("reason_missing", p["wont_do"])

    def test_empty_sends_count_up_persist_across_reload_and_third_closes_with_reason_missing(self):
        body = """
        (async () => {
          const it = { id: "D20", status: "open" }, st = { note: "", attempts: 0 };
          await wontDoSend(it, st, "   ");
          const first = writes[0].patch;
          const st2 = { note: "", attempts: 0 };                       /* reload: the count comes from the stored item */
          const reloaded = Object.assign({}, it, { wont_do_attempts: 1 });
          await wontDoSend(reloaded, st2, "");
          const second = writes[1].patch;
          const b = el("div"); wontDoOpen.set("D20", { note: "", attempts: 0 });
          wontDoControl(b, Object.assign({}, it, { wont_do_attempts: 2 }));
          const msg = b.find((n) => n.className.indexOf("wd-msg") >= 0)[0].textContent;
          await wontDoSend(Object.assign({}, it, { wont_do_attempts: 2 }), { note: "", attempts: 0 }, "");
          console.log(JSON.stringify({ first, second, msg, third: writes[2].patch, opts: writes.map((w) => w.opts), n: writes.length }));
        })();"""
        for r in self.each(body):
            self.assertEqual(r["first"]["wont_do_attempts"], 1)
            self.assertNotIn("status", r["first"])
            self.assertEqual(r["second"]["wont_do_attempts"], 2)
            self.assertEqual(r["msg"], "Add a reason so the Architect can act on it (attempt 2 of 3)")
            self.assertEqual(r["third"]["status"], "wont_do")
            self.assertEqual({k: r["third"]["wont_do"].get(k) for k in ("note", "reason_missing", "attempts")}, {"note": "", "reason_missing": True, "attempts": 3})
            self.assertEqual(r["opts"], [{"if_version": 7}] * 3)

    def test_wont_do_items_are_closed_not_counted_and_searchable(self):
        body = """
        const list = [{ id: "D01", status: "open" }, { id: "A02", status: "open" }, { id: "A03", status: "wont_do", wont_do: { note: "too costly", at: "2026-10-09T03:00:00Z" } },
                      { id: "D04", status: "done", done_at: "2026-10-09T01:00:00Z" }];
        const b = bucketItems(list);
        const page = completedPage(b.done, "costly", 0, 20);
        console.log(JSON.stringify({ open: b.open.map((i) => i.id), done: b.done.map((i) => i.id), undone: undoneCount(list), hit: page.rows.map((i) => i.id) }));"""
        for r in self.each(body):
            self.assertEqual(r, {"open": ["D01", "A02"], "done": ["A03", "D04"], "undone": 2, "hit": ["A03"]})

    def test_reopened_chip_only_while_open_with_history(self):
        body = """
        const h = [{ note: "x", at: "t" }];
        console.log(JSON.stringify([wasReopened({ status: "open", wont_do_history: h }), wasReopened({ status: "open", wont_do_history: [] }),
          wasReopened({ status: "open" }), wasReopened({ status: "wont_do", wont_do_history: h })]));"""
        for r in self.each(body):
            self.assertEqual(r, [True, False, False, False])

    def test_pages_render_the_wont_do_card_and_chips(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('if (it.status === "wont_do") return renderWontDo(it);', src)
            self.assertIn('"chip reopened", "reopened"', src)
            self.assertNotIn("confirm(", re.search(r"/\* BEGIN wontDo \*/(.*?)/\* END wontDo \*/", src, re.S).group(1))


if __name__ == "__main__":
    unittest.main()

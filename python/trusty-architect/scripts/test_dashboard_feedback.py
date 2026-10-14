"""Dashboard Feedback control (run under node, skipped when node is absent), checked on both HTML files:
the control renders on D, A and T; Send writes the feedback fields; a kind change moves the row."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "feedback", "park")]
    js = FAKE + """
const calls = []; let db = { collection: () => ({ doc: () => ({ get: async () => ({ version: 7 }) }) }) }, readOnly = false;
const document = {}; const $ = () => null; const userName = () => "Owner"; const utc = (v) => v; const addText = (p, t) => p.append(t); const renders = []; const render = () => renders.push(1);
let written = null; async function write(id, patch, opts) { written = { id, patch, opts }; return true; }
""" + "\n".join(parts) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardFeedback(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body)

    def test_control_renders_on_decision_ask_and_task_when_open(self):
        body = """
        const out = {};
        for (const it of [{ id: "D01", status: "open" }, { id: "A02", status: "open" }, { id: "T03", status: "open" }, { id: "A04", status: "answered" }]) {
          const b = el("div"); feedbackControl(b, it);
          out[it.id] = b.find((n) => n.tagName === "button").map((n) => n.textContent);
        }
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, {"D01": ["Feedback"], "A02": ["Feedback"], "T03": ["Feedback"], "A04": []})

    def test_open_form_has_kind_select_note_send_cancel_with_current_kind_preselected(self):
        body = """
        const it = { id: "A11", status: "open" }; const b = el("div");
        feedbackControl(b, it); b.find((n) => n.tagName === "button")[0].listeners.click();
        const b2 = el("div"); feedbackControl(b2, it);
        const sel = tag(b2, "select")[0];
        console.log(JSON.stringify({ sel: sel.value, opts: sel.children.map((o) => o.value + "=" + o.textContent), btns: b2.find((n) => n.tagName === "button").map((n) => n.textContent), inputs: b2.find((n) => n.tagName === "input").length }));"""
        for r in self.each(body):
            self.assertEqual(r["sel"], "action")
            self.assertEqual(r["opts"], ["question=Decision", "action=Ask", "task=Architect task"])
            self.assertEqual(r["btns"], ["Send", "Cancel"])
            self.assertEqual(r["inputs"], 1)

    def test_send_writes_feedback_with_if_version_and_kind_change_moves_the_row(self):
        body = """
        (async () => {
          const it = { id: "A11", status: "open" };
          await feedbackItem(it, { to: "question", note: "  I approve this  " });
          const f = written.patch.feedback;
          const pending = optimisticFeedback;
          const shown = withFeedback(it, pending);
          console.log(JSON.stringify({ id: written.id, opts: written.opts, f, upd: written.patch.updated === f.at,
            before: askKind(it), after: askKind(shown), stored: storedKind(shown) }));
        })();"""
        for r in self.each(body):
            self.assertEqual(r["id"], "A11")
            self.assertEqual(r["opts"], {"if_version": 7})
            self.assertEqual({k: r["f"][k] for k in ("to", "note", "by", "status")},
                             {"to": "question", "note": "I approve this", "by": "Owner", "status": "new"})
            self.assertTrue(r["f"]["at"] and r["upd"])
            self.assertEqual((r["before"], r["after"], r["stored"]), ("A", "D", "A"))

    def test_note_only_keeps_kind_and_applied_feedback_or_echo_clears_the_override(self):
        body = """
        const a = { id: "T05", status: "open", feedback: { to: "task", note: "wording", at: "2026-10-09T02:00:00Z", status: "new" } };
        const done = Object.assign({}, a, { feedback: Object.assign({}, a.feedback, { to: "question", status: "applied" }) });
        const pend = new Map([["T05", { to: "question", note: "", at: "2026-10-09T02:00:00Z", status: "new" }]]);
        const echoed = withFeedback({ id: "T05", status: "open", feedback: { to: "question", at: "2026-10-09T02:00:00Z", status: "applied" } }, pend);
        console.log(JSON.stringify({ note: askKind(a), applied: askKind(done), echoed: askKind(echoed), left: pend.size,
          patch: feedbackPatch("bogus", null, null, "t").feedback.to }));"""
        for r in self.each(body):
            self.assertEqual(r, {"note": "T", "applied": "T", "echoed": "T", "left": 0, "patch": "action"})

    def test_pages_wire_the_control_into_d_a_t_rows_and_keep_earlier_features(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            for fn in ("renderQuestion", "renderAction", "renderTask"):
                m = re.search(r"  function %s\(.*?\n  }\n" % fn, src, re.S)
                self.assertIn("feedbackControl(body, it, parkControl(body, it));", m.group(0), page + fn)
            self.assertIn("withFeedback(r, optimisticFeedback)", src)
            self.assertIn("cancelControl(", src)
            self.assertIn(".fb-form select", src)

    def test_park_choices_write_dates_and_fields_and_unpark_clears(self):
        body = """
        const wed = new Date(2026, 9, 14, 15, 30);   // Wednesday 14 Oct 2026 local
        const sun = new Date(2026, 9, 18, 8, 0), dec = new Date(2026, 11, 31, 9, 0);
        const loc = (iso) => { const d = new Date(iso); return [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes()].join("-"); };
        const p = parkPatch("nextweek", "Owner", wed);
        console.log(JSON.stringify({ tom: loc(parkUntil("tomorrow", wed)), mon: loc(parkUntil("nextweek", wed)), monSun: loc(parkUntil("nextweek", sun)),
          month: loc(parkUntil("nextmonth", wed)), year: loc(parkUntil("nextmonth", dec)),
          fields: [p.parked.choice, p.parked.by, p.parked.at === wed.toISOString(), p.updated === p.parked.at], bogus: parkPatch("x", "B", wed).parked.choice,
          unpark: unparkPatch(wed).parked }));"""
        for r in self.each(body):
            self.assertEqual((r["tom"], r["mon"], r["monSun"]), ("2026-10-15-9-0", "2026-10-19-9-0", "2026-10-19-9-0"))
            self.assertEqual((r["month"], r["year"]), ("2026-11-1-9-0", "2027-1-1-9-0"))
            self.assertEqual(r["fields"], ["nextweek", "Owner", True, True])
            self.assertEqual((r["bogus"], r["unpark"]), ("tomorrow", None))

    def test_parked_item_is_parked_only_while_open_unparked_after_and_updates_aggregate(self):
        body = """
        const pk = { until: "2026-10-15T13:00:00Z", choice: "tomorrow", at: "2026-10-14T20:00:00Z", by: "Owner" };
        const it = { id: "A12", status: "open", parked: pk, updates: [
          { at: "2026-10-14T19:00:00Z", text: "before" }, { at: "2026-10-14T21:00:00Z", by: "Architect", text: "older" },
          { at: "2026-10-14T23:00:00Z", source: "pm", text: "newer" }, { at: "x" }] };
        const b = el("div"); updatesBlock(b, it);
        const pend = new Map([["A12", null]]);
        console.log(JSON.stringify({ parked: isParked(it), answered: isParked(Object.assign({}, it, { status: "answered" })), unparked: isParked(Object.assign({}, it, { parked: null })),
          since: updatesSinceParked(it), order: updatesOf(it).map((u) => u.text), text: b.textContent.replace(/\d{4}-\d\d-\d\d[^:]*: /g, ""),
          optUnpark: isParked(withPark(it, pend)), kept: pend.size, echoed: (() => { const m = new Map([["A12", null]]); withPark(Object.assign({}, it, { parked: null }), m); return m.size; })() }));"""
        for r in self.each(body):
            self.assertEqual((r["parked"], r["answered"], r["unparked"], r["since"]), (True, False, False, 2))
            self.assertEqual(r["order"], ["newer", "older", "before"])
            self.assertTrue(r["text"].startswith("Updates (3)"))
            self.assertEqual((r["optUnpark"], r["kept"], r["echoed"]), (False, 1, 0))

    def test_pages_keep_parked_out_of_active_lists_and_show_a_parked_bucket(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('i.status === "open" && !(i.parked && i.parked.until)),', src)
            for x in "qat":
                self.assertIn('id="list-%sp"' % x, src)
            for fn in ("renderQuestion", "renderAction", "renderTask"):
                m = re.search(r"  function %s\(.*?\n  }\n" % fn, src, re.S)
                self.assertIn("parkControl(body, it)", m.group(0), page + fn)
                self.assertIn("updatesBlock(body, it);", m.group(0), page + fn)
            self.assertIn("renderParked();", src)
            self.assertIn("withPark(withFeedback(r, optimisticFeedback), optimisticPark)", src)



@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardAccounts(unittest.TestCase):
    def run_acc(self, page, body):
        src = open(page, encoding="utf-8").read()
        part = re.search(r"/\* BEGIN accounts \*/(.*?)/\* END accounts \*/", src, re.S).group(1)
        part = re.search(r"/\* BEGIN localTime \*/(.*?)/\* END localTime \*/", src, re.S).group(1) + part
        r = subprocess.run(["node", "-e", part + "\n" + body], capture_output=True, text=True, timeout=30)
        assert r.returncode == 0, r.stderr
        return json.loads(r.stdout)

    def test_rows_follow_the_data_sorted_tightest_first_with_stale_and_dashes(self):
        body = """
        const now = Date.parse("2026-10-09T02:00:00Z");
        const docs = [
          { account: "a@example.com", pct5h: 40, reset5h: "2026-10-09T03:12:00Z", pctwk: 62, resetwk: "2026-10-13T11:00:00Z", ts: "2026-10-09T01:50:00Z" },
          { account: "b@example.com", pct5h: 90, reset5h: "2026-10-09T02:30:00Z", pctwk: 10, resetwk: "2026-10-09T01:00:00Z", ts: "2026-10-09T01:20:00Z" },
          { account: "c@example.com", pct5h: 0, reset5h: null, pctwk: 93 },
          { account: "d@example.com" }, { nope: 1 }, null];
        const r = accountRows(docs, now);
        console.log(JSON.stringify({ n: r.length, order: r.map((x) => x.account), rem: r.map((x) => [x.use5, x.useWk]),
          in5: r[1].in5, passed: r[0].inWk, stale: r.map((x) => x.stale), blank: [r[3].use5, r[3].reset5, r[3].asOf, r[3].inWk], none: accountRows(null, now).length }));"""
        for page in PAGES:
            with self.subTest(page=page):
                r = self.run_acc(page, body)
                self.assertEqual(r["n"], 4)                                    # one row per valid account, no fixed count
                self.assertEqual(r["order"], ["b@example.com", "a@example.com", "c@example.com", "d@example.com"])   # tightest 5h first, unknown last
                self.assertEqual(r["rem"], [[90, 10], [40, 62], [0, 93], [None, None]])
                self.assertEqual((r["in5"], r["passed"]), ("in 1h 12m", "passed"))
                self.assertEqual(r["stale"], [True, False, True, True])      # 40 min old, 10 min old, no ts, no ts
                self.assertEqual(r["blank"], [None, "", "", ""])
                self.assertEqual(r["none"], 0)

    def test_pages_have_an_accounts_section_open_by_default_wired_to_the_collection(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('id="sec-accounts"', src)
            self.assertIn('bindOpen($("sec-accounts"), "sec:accounts", true);', src)
            self.assertIn('db.collection("accounts").onSnapshot', src)
            self.assertLess(src.index('id="sec-sessions"'), src.index('id="sec-accounts"'))
            self.assertLess(src.index('id="sec-accounts"'), src.index("<footer>"))


if __name__ == "__main__":
    unittest.main()

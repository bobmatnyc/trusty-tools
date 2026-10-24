"""Session context bar fills with use; beeps only for a new decision and for 5-hour window events (both HTML files). Runs under node, no sound."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def run(page, names, body, pre=""):
    src = open(page, encoding="utf-8").read()
    blocks = []
    for n in names:
        m = re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S)
        assert m, n
        blocks.append(m.group(1))
    js = pre + "\n".join(blocks) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestSessionBars(unittest.TestCase):
    def test_ctx_fill_rises_with_use_and_warns_near_full(self):
        body = """console.log(JSON.stringify([22, 50, 90, 250, 400].map((k) => ctxFill({ usage: { ctx_k: k } })).concat([
          ctxFill({ context: { pct: 22 }, usage: { ctx_k: 200 } }), ctxFill({ usage: {} }), ctxFill({})])));"""
        for page in PAGES:
            r = run(page, ["ctxFill"], body)
            self.assertEqual([x["pct"] for x in r[:5]], [9, 20, 36, 100, 100])   # grows with ctx_k, clamped
            self.assertEqual([x["hot"] for x in r[:5]], [False, False, False, True, True])
            self.assertEqual(r[5]["pct"], 22)                                     # context.pct wins; 22% -> 22% filled
            self.assertIsNone(r[6]); self.assertIsNone(r[7])

    def test_session_row_bar_is_ctx_and_progress_is_labelled(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('style.setProperty("--ctx"', src)
            self.assertNotRegex(src, r'd\.style\.setProperty\("--pct", roll\.pct')
            self.assertIn('"tasks " + roll.pct', src)

    def test_only_new_decision_beeps(self):
        body = """
        const D = (id, extra) => Object.assign({ id: id, status: 'open', kind: 'question', title: 't' }, extra);
        const A = (id, extra) => Object.assign({ id: id, status: 'open', kind: 'action', title: 't' }, extra);
        const first = attentionUpdate({ pending: new Set(), sigs: {} }, [D("D01")], [], true);
        const r = (docs, ch) => attentionUpdate(first, docs, ch, false).beep;
        console.log(JSON.stringify([r([D("D01"), D("D02")], ["D02"]), r([D("D01"), A("A01")], ["A01"]), r([D("D01"), { id: "T01", status: "open", kind: "task" }], ["T01"]),
          r([D("D01", { title: "edited" })], ["D01"]), r([D("D01"), D("D02", { status: "answered" })], ["D02"])]));"""
        pre = "const isParked = (it) => !!(it && it.parked);\n"
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            pk = re.search(r"  function isParked\(it\)[^\n]*\n", src).group(0)
            self.assertEqual(run(page, ["askKind", "attention"], body, pk), [True, False, False, False, False])

    def test_reminder_tick_and_flash_have_no_unconditional_beep(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertNotIn("if (r.fire) { remindPulse(); beepPlay", src)
            self.assertIn("if (beep) beepPlay(", src)

    def test_account_window_beeps_once_per_event(self):
        body = """
        const T = Date.parse("2026-10-09T14:00:00Z");
        const doc = (p, reset, ts) => ({ account: "a@x", pct5h: p, reset5h: reset, ts: ts || "2026-10-09T14:00:00Z" });
        const R1 = "2026-10-09T16:10:00Z";
        let st = { fired: [], last: null }, out = [];
        const step = (docs, now) => { const r = acctBeepStep(st, docs, now); st = r.st; out.push(r.beep); };
        step([doc(90, R1)], T);                       /* first evaluation seeds: no beep */
        step([doc(94, R1)], T + 60000);               /* below 95: no beep */
        step([doc(95, R1)], T + 120000);              /* crosses 95: beep */
        step([doc(96, R1)], T + 180000);              /* same window, polled again: no repeat */
        const reloaded = JSON.parse(JSON.stringify(st)); st = reloaded;
        step([doc(97, R1)], T + 240000);              /* after a reload: no repeat */
        step([doc(97, R1, "2026-10-09T14:05:00Z")], Date.parse(R1) + 1000);   /* reset5h passes: beep */
        step([doc(2, "2026-10-09T21:10:00Z", "2026-10-09T16:11:00Z")], Date.parse(R1) + 120000);   /* new window reading: no repeat */
        step([{ account: "b@x", pct5h: 10, reset5h: R1, ts: "2026-10-09T16:12:00Z" }, doc(2, "2026-10-09T21:10:00Z", "2026-10-09T16:11:00Z")], Date.parse(R1) + 180000);
        let s2 = { fired: [], last: { account: "a@x", reset5: "2026-10-09T20:00:00Z", pct: 80 } };
        const drop = acctBeepStep(s2, [doc(5, "2026-10-09T20:00:00Z")], T);   /* sharp drop, reset time not reached: beep */
        console.log(JSON.stringify([out, drop.beep, acctBeepStep(drop.st, [doc(5, "2026-10-09T20:00:00Z")], T + 1000).beep]));"""
        for page in PAGES:
            out, drop, again = run(page, ["accounts", "acctBeep"], body, 'function localTime(x){return x;}\n')
            self.assertEqual(out, [False, False, True, False, False, True, False, False])
            self.assertTrue(drop)
            self.assertFalse(again)


if __name__ == "__main__":
    unittest.main()

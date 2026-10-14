"""Dashboard 5-minute reminder pulse and quiet beep (both HTML files; brief 2026-10-09-dashboard-reminder-pulse-beep.md).
Runs the page's pure helpers under node (skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]


def run(page, body):
    src = open(page, encoding="utf-8").read()
    ask = re.search(r"/\* BEGIN askKind \*/(.*?)/\* END askKind \*/", src, re.S).group(1)
    reminder = re.search(r"/\* BEGIN reminder \*/(.*?)/\* END reminder \*/", src, re.S).group(1)
    bucket = re.search(r"  function bucketItems\(list\) \{.*?\n  \}\n", src, re.S).group(0)
    js = ask + bucket + reminder + "\nconst M = 60000;\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardReminder(unittest.TestCase):
    def test_fires_at_five_minutes_only_when_undone(self):
        body = """
        let st = { next: null, due: false }, fires = [];
        for (let m = 0; m <= 11; m++) { const r = reminderStep(st, m * M, 2, false); st = r.st; if (r.fire) fires.push(m); }
        let z = { next: null, due: false }, zf = 0;
        for (let m = 0; m <= 11; m++) { const r = reminderStep(z, m * M, 0, false); z = r.st; if (r.fire) zf++; }
        console.log(JSON.stringify([fires, zf]));"""
        for page in PAGES:
            with self.subTest(page=page):
                fires, zero_fires = run(page, body)
                self.assertEqual(fires, [5, 10])   # none at load, one every 5 min
                self.assertEqual(zero_fires, 0)

    def test_timer_stops_at_zero_and_restarts_on_rise(self):
        body = """
        let st = { next: null, due: false };
        st = reminderStep(st, 0, 1, false).st;
        st = reminderStep(st, 2 * M, 0, false).st;                       /* count hits 0: timer cleared */
        const cleared = [st.next, st.due];
        const rose = reminderStep(st, 3 * M, 1, false);                  /* rises again: 5 min from now */
        const early = reminderStep(rose.st, 7 * M, 1, false).fire;
        const on_time = reminderStep(rose.st, 8 * M, 1, false).fire;
        console.log(JSON.stringify([cleared, rose.fire, early, on_time]));"""
        for page in PAGES:
            with self.subTest(page=page):
                self.assertEqual(run(page, body), [[None, False], False, False, True])

    def test_hidden_tab_defers_and_fires_once_on_return(self):
        body = """
        let st = reminderStep({ next: null, due: false }, 0, 1, false).st;
        const hid = reminderStep(st, 6 * M, 1, true);                    /* due while hidden: no pulse */
        const hid2 = reminderStep(hid.st, 9 * M, 1, true);
        const back = reminderStep(hid2.st, 10 * M, 1, false);            /* visible again: fires once */
        const after = reminderStep(back.st, 10 * M + 1000, 1, false);
        console.log(JSON.stringify([hid.fire, hid2.fire, hid.st.due, back.fire, after.fire]));"""
        for page in PAGES:
            with self.subTest(page=page):
                self.assertEqual(run(page, body), [False, False, True, True, False])

    def test_tasks_and_parked_items_do_not_count(self):
        body = """
        const O = (id, extra) => Object.assign({ id: id, status: 'open' }, extra);
        const list = [O("D01"), O("A01"), O("T01"), O("D02", { parked: { until: "2099-01-01T00:00:00Z" } }),
                      O("A02", { status: "answered" }), O("D03", { status: "cancelled" }), O("A03", { status: "done" })];
        console.log(JSON.stringify([undoneCount(list), undoneCount([O("T01"), O("T02")]), undoneCount([])]));"""
        for page in PAGES:
            with self.subTest(page=page):
                self.assertEqual(run(page, body), [2, 0, 0])

    def test_beep_default_off_and_storage_failures(self):
        body = """
        const ok = (v) => ({ getItem: () => v });
        const boom = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); } };
        let saved = null; beepSave({ setItem: (k, v) => { saved = [k, v]; } }, true);
        beepSave(boom, true);                                                  /* must not throw */
        console.log(JSON.stringify([beepLoad(ok(null)), beepLoad(ok("0")), beepLoad(ok("1")), beepLoad(boom), beepLoad(undefined), saved]));"""
        for page in PAGES:
            with self.subTest(page=page):
                self.assertEqual(run(page, body), [False, False, True, False, False, ["aq-beep", "1"]])
            src = open(page, encoding="utf-8").read()
            self.assertIn("let beepOn = false", src)

    def test_beep_plays_only_when_enabled_visible_and_running(self):
        body = """
        function mock(state) {
          const log = [];
          const param = () => ({ value: 0, setValueAtTime: (v, t) => log.push(["set", v, t]), linearRampToValueAtTime: (v, t) => log.push(["ramp", v, t]) });
          return { log: log, state: state, currentTime: 1, destination: {},
            createOscillator: () => ({ type: "", frequency: { value: 0 }, connect() {}, start: (t) => log.push(["start", t]), stop: (t) => log.push(["stop", t]),
              set osc(v) {} }), createGain: () => ({ gain: param(), connect() {} }) };
        }
        const run = (on, ctx, hidden) => beepPlay(on, ctx);
        const c1 = mock("running"), c2 = mock("running"), c3 = mock("suspended"), c4 = mock("running");
        const res = [run(false, c1, false), run(true, c2, true), run(true, c3, false), run(true, null, false), run(true, c4, false)];
        console.log(JSON.stringify([res, c1.log.length, c2.log.length, c3.log.length, c4.log.length, c4.log.filter((x) => x[0] === "ramp").map((x) => x[1])]));"""
        for page in PAGES:
            with self.subTest(page=page):
                res, off, _hidden, suspended, played, ramps = run(page, body)
                self.assertEqual(res, [False, True, False, False, True])
                self.assertEqual((off, suspended), (0, 0))   # nothing scheduled
                self.assertGreater(played, 0)
                self.assertEqual(ramps, [0.05, 0])                       # fade in to 0.05, fade out to 0

    def test_markup_and_css(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('id="beep-toggle"', src)
            self.assertIn('aria-pressed="false"', src)
            self.assertIn("osc.frequency.value = 660", src)
            self.assertIn("body.attn-remind #attn-frame", src)
            self.assertRegex(src, r"prefers-reduced-motion: reduce\) \{ body\.attn-remind #attn-frame \{ animation: none")
            self.assertIn("document.addEventListener(\"visibilitychange\", remindTick)", src)


if __name__ == "__main__":
    unittest.main()

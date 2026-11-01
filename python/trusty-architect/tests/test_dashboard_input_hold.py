"""Dashboard input hold (node, no browser), on both HTML files: store updates wait while an input is open.

owner 2026-10-11 04:07Z: no refresh while an input is open, one open input at a time, a visible countdown, apply at 0.

Test: scripts/test_dashboard_input_hold.py
"""
import json
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body, names=("inputHold",), prelude=""):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in names]
    js = FAKE + prelude + "\n".join(parts) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestInputHold(unittest.TestCase):
    def each(self, body, **kw):
        return [run(page, body, **kw) for page in PAGES]

    def test_update_applies_at_once_when_no_input_is_open(self):
        body = """let open = false, t = 0, got = [];
        const h = makeHold(() => open, () => t, 60000);
        const r = h.offer(() => got.push("a"));
        console.log(JSON.stringify([r, got, h.pending()]));"""
        for r in self.each(body):
            self.assertEqual(r, [{"ran": True, "render": True}, ["a"], False])

    def test_update_waits_while_open_then_lands_in_order_when_closed(self):
        body = """let open = true, t = 1000, got = [];
        const h = makeHold(() => open, () => t, 60000);
        const r1 = h.offer(() => got.push("a")), r2 = h.offer(() => got.push("b"), true);
        const held = [got.slice(), h.pending(), h.secsLeft(), holdText(h.secsLeft())];
        t = 31000; const mid = h.secsLeft(); const r3 = h.settle();
        open = false; const r4 = h.settle();
        console.log(JSON.stringify({ r1, r2, held, mid, r3, r4, got, pending: h.pending() }));"""
        for r in self.each(body):
            self.assertEqual(r["r1"], {"ran": False, "render": False})
            self.assertEqual(r["r2"], {"ran": False, "render": False})
            self.assertEqual(r["held"], [[], True, 60, "Update waiting: applies in 60 s"])
            self.assertEqual(r["mid"], 30)
            self.assertFalse(r["r3"]["ran"])
            self.assertEqual(r["r4"], {"ran": True, "render": True})
            self.assertEqual(r["got"], ["a", "b"])
            self.assertFalse(r["pending"])

    def test_update_lands_at_zero_even_if_the_input_stays_open(self):
        body = """let t = 0, got = [];
        const h = makeHold(() => true, () => t, 60000);
        h.offer(() => got.push("a"), true);
        t = 59999; const before = h.settle(); t = 60000; const at = h.settle();
        console.log(JSON.stringify({ before, at, got, secs: holdSecs(60000, 60000) }));"""
        for r in self.each(body):
            self.assertEqual(r["before"], {"ran": False, "render": False})
            self.assertEqual(r["at"], {"ran": True, "render": False})   # quiet entries ask for no re-render
            self.assertEqual([r["got"], r["secs"]], [["a"], 0])

    def test_recent_keystroke_keeps_update_waiting_past_the_deadline_until_idle_or_blur(self):
        body = """let open = true, typed = true, t = 0, got = [];
        const h = makeHold(() => open, () => t, 60000, () => typed);
        h.offer(() => got.push("a"), true);
        t = 61000; const typing = h.settle();
        typed = false; const idle = h.settle();
        open = true; typed = true; h.offer(() => got.push("b"), true); t = 130000; const again = h.settle();
        open = false; const blur = h.settle();
        console.log(JSON.stringify({ typing, idle, again, blur, got }));"""
        for r in self.each(body):
            self.assertEqual(r["typing"], {"ran": False, "render": False})
            self.assertTrue(r["idle"]["ran"])
            self.assertFalse(r["again"]["ran"])
            self.assertTrue(r["blur"]["ran"])
            self.assertEqual(r["got"], ["a", "b"])

    def test_a_throwing_update_does_not_lose_the_rest(self):
        body = """let got = [];
        const h = makeHold(() => false, () => 0, 1); console.error = () => {};
        h.offer(() => { throw new Error("x"); }); h.offer(() => got.push("b"));
        console.log(JSON.stringify(got));"""
        for r in self.each(body):
            self.assertEqual(r, ["b"])

    def test_opening_a_second_form_closes_the_first_and_keeps_its_draft(self):
        body = """const a = new Map([["D1", { note: "typed" }]]), b = new Map(), stash = new Map();
        const slots = [
          { kind: "fb", keys: () => Array.from(a.keys()), ids: (k) => ["fb-note-" + k], take: (k) => a.get(k), close: (k) => a.delete(k) },
          { kind: "cmd", keys: () => Array.from(b.keys()), ids: (k) => ["cmd-note-" + k], take: (k) => b.get(k), close: (k) => b.delete(k) },
        ];
        closeOthers(slots, "cmd", "S1", stash); b.set("S1", { note: "" });
        const open = openForms(slots).map((f) => f.kind + ":" + f.key);
        const back = recall(stash, "fb", "D1"), again = recall(stash, "fb", "D1");
        closeOthers(slots, "cmd", "S1", stash);   // the same form stays open
        console.log(JSON.stringify({ open, back, again: again === undefined, still: Array.from(b.keys()) }));"""
        for r in self.each(body):
            self.assertEqual(r, {"open": ["cmd:S1"], "back": {"note": "typed"}, "again": True, "still": ["S1"]})

    def test_steer_field_is_state_backed_so_a_render_keeps_its_draft(self):
        prelude = """const steerOpen = new Map(), formStash = new Map(); const claimForm = () => {}, recall = () => undefined, $ = () => null; let renders = 0; const render = () => { renders++; };
        const userName = () => "owner"; let db = {}, readOnly = false;\n"""
        body = """steerOpen.set("D1", { text: "keep me" });
        const row = el("div"); steerControl(row, { id: "D1" });
        const inp = row.find((n) => n.tagName === "input")[0], form = row.find((n) => n.className === "steer-form")[0], btn = row.find((n) => n.tagName === "button")[0];
        const closed = el("div"); steerControl(closed, { id: "D2" });
        console.log(JSON.stringify({ id: inp.id, value: inp.value, formHidden: form.hidden, btnHidden: btn.hidden, closedHidden: closed.find((n) => n.className === "steer-form")[0].hidden }));"""
        for page in PAGES:
            with self.subTest(page=page):
                r = run(page, body, names=("askKind", "markers", "park"), prelude=prelude)
                self.assertEqual(r, {"id": "steer-D1", "value": "keep me", "formHidden": False, "btnHidden": True, "closedHidden": True})

    def test_page_wiring(self):
        for page in PAGES:
            with self.subTest(page=page):
                src = open(page, encoding="utf-8").read()
                self.assertNotRegex(src, r"onSnapshot\(\(snap\)")   # every store snapshot goes through live()
                self.assertEqual(len(re.findall(r"\.onSnapshot\(live\(", src)), 14)
                for needle in ("hold.settle();", "paintHold();", "setInterval(tickHold, 1000)", 'claimForm("cancel"', 'claimForm("wd"', 'claimForm("fb"',
                               'claimForm("cmd"', 'claimForm("note"', 'claimForm("steer"', 'claimForm("", "")', ".hold-note"):
                    self.assertIn(needle, src)


if __name__ == "__main__":
    unittest.main()

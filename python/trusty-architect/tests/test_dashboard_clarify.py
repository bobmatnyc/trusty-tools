"""Dashboard decision markers, spec links and clarify-park (node, no browser), on both HTML files.

Test: scripts/test_dashboard_clarify.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "markers", "park")]
    js = FAKE + """
N.prototype.querySelectorAll = function () { return this.find((n) => n.tagName === "button"); };
let db = {}, readOnly = false; const userName = () => "owner"; const utc = (v) => v; const addText = (p, t) => p.append(t); const render = () => {};
const steerOpen = new Map(), formStash = new Map(), claimForm = () => {}, recall = () => undefined, $ = () => null;
let written = null; async function write(id, patch, opts) { written = { id, patch, opts }; return true; }
""" + "\n".join(parts) + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestClarify(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=page):
                yield run(page, body)

    def test_marker_text_clamps_hides_zero_and_uses_minus_sign(self):
        body = """console.log(JSON.stringify([
          markersOf({ markers: { work: 2, scope: -1, risk: 3 } }).map((m) => m.text),
          markersOf({ markers: { work: 0, scope: 9, risk: "x" } }).map((m) => m.text),
          markersOf({}).length, markersOf(null).length]));"""
        for r in self.each(body):
            self.assertEqual(r[0], ["Work ++", "Scope −", "Risk +++"])
            self.assertEqual(r[1], ["Scope +++"])
            self.assertEqual(r[2:], [0, 0])

    def test_clarify_patch_shape_and_parked_without_until(self):
        body = """
        const now = new Date("2026-10-11T04:00:00Z");
        const p = clarifyPatch("Option A", "risk", "owner", now), g = clarifyPatch("ignored", "bogus", "owner", now);
        const it = { id: "D01", status: "open", parked: p.parked };
        console.log(JSON.stringify({ p, g: g.parked.clarify, parked: isParked(it), date: isParked({ status: "open", parked: { until: "2026-10-12T09:00:00Z" } }),
          none: isParked({ status: "open", parked: { until: null, choice: "tomorrow" } }), text: clarifyText(it), gtext: clarifyText({ parked: g.parked }) }));"""
        for r in self.each(body):
            self.assertEqual(r["p"]["parked"], {"until": None, "choice": "clarify", "clarify": {"option": "Option A", "dimension": "risk"}, "at": "2026-10-11T04:00:00.000Z", "by": "owner"})
            self.assertEqual(r["p"]["updated"], "2026-10-11T04:00:00.000Z")
            self.assertEqual(r["g"], {"option": None, "dimension": "general"})
            self.assertEqual([r["parked"], r["date"], r["none"]], [True, True, False])
            self.assertEqual(r["text"], "clarify")   # chip label; the question is in the body (clarifyDetail)
            self.assertEqual(r["gtext"], "clarify")

    def test_explain_button_only_on_decisions_and_writes_a_general_clarify_park(self):
        body = """
        (async () => {
          const names = {};
          for (const id of ["D01", "A02", "T03"]) { const b = el("div"); parkControl(b, { id, status: "open" }); names[id] = b.find((n) => n.tagName === "button").map((n) => n.textContent); }
          const b = el("div"); parkControl(b, { id: "D01", status: "open" });
          const x = b.find((n) => n.tagName === "button").filter((n) => n.textContent === "Explain")[0];
          x.listeners.click(); await new Promise((r) => setTimeout(r, 10));
          console.log(JSON.stringify({ names, id: written.id, parked: written.patch.parked }));
        })();"""
        for r in self.each(body):
            self.assertEqual(r["names"]["D01"][-1], "Explain")
            self.assertNotIn("Explain", r["names"]["A02"] + r["names"]["T03"])
            self.assertEqual(r["id"], "D01")
            self.assertEqual((r["parked"]["choice"], r["parked"]["until"], r["parked"]["clarify"]), ("clarify", None, {"option": None, "dimension": "general"}))

    def test_updates_since_parked_returns_a_clarify_item_and_buckets_hide_it(self):
        body = """
        const it = { id: "D01", status: "open", parked: { until: null, choice: "clarify", at: "2026-10-11T04:00:00Z" }, updates: [{ at: "2026-10-11T05:00:00Z", text: "more" }, { at: "2026-10-11T03:00:00Z", text: "old" }] };
        console.log(JSON.stringify(updatesSinceParked(it)));"""
        for r in self.each(body):
            self.assertEqual(r, 1)

    def test_page_wiring_chips_outside_option_button_specs_row_and_no_next_ids(self):
        for page in PAGES:
            src = open(page, encoding="utf-8").read()
            self.assertIn('box.appendChild(b);\n      const mk = markersOf(o);', src)   # chip row is a sibling of the option button
            self.assertIn('"Ask the PM to clarify " + m.dim', src)
            self.assertIn('el("div", "specs-row")', src)
            self.assertIn('i.parked.choice === "clarify"', src)                       # open filter accepts until: null
            self.assertNotIn("Next ids", src)
            self.assertNotIn("nextIds", src)
            self.assertNotIn("asks-note", src)


if __name__ == "__main__":
    unittest.main()

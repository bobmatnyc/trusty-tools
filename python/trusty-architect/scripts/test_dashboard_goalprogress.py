"""Dashboard: completion percent and ETA on task rows; relative effort (24 h token spend) on goal rows (the owner 2026-10-09 03:44Z-03:50Z), checked on both HTML files.
Runs the etaProgress and goalEffort helpers under node with TZ=America/New_York (skipped when node is absent)."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PAGES = [os.path.join(ROOT, p) for p in ("templates/dashboard/index.html",)]

FAKE = r"""
class N {
  constructor(tag) { this.tagName = tag; this.children = []; this.className = ""; this.attrs = {}; this.style = {}; this._t = ""; }
  set textContent(v) { this._t = String(v); } get textContent() { return this._t + this.children.map((c) => c.textContent).join(""); }
  appendChild(c) { this.children.push(c); return c; }
  append(...cs) { cs.forEach((c) => this.children.push(typeof c === "string" ? Object.assign(new N("#text"), { _t: c }) : c)); }
  setAttribute(k, v) { this.attrs[k] = v; }
  find(pred, out = []) { if (pred(this)) out.push(this); this.children.forEach((c) => c.find(pred, out)); return out; }
}
function el(tag, cls, text) { const e = new N(tag); if (cls) e.className = cls; if (text != null && text !== "") e.textContent = text; return e; }
function txt(v) { return v == null ? "" : typeof v === "string" ? v : String(v); }
const NOW = Date.parse("2026-10-09T12:00:00Z");
const prog = (o) => Object.assign({ pct: 40, eta: "2026-10-14T22:00:00Z", confidence: "medium", basis: "3 of 7 milestones", estimated_at: "2026-10-09T10:00:00Z" }, o || {});
"""


def run(body):
    out = None
    for page in PAGES:
        src = open(page, encoding="utf-8").read()
        parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("localTime", "etaProgress", "goalEffort")]
        r = subprocess.run(["node", "-e", FAKE + "\n".join(parts) + "\n" + body], capture_output=True, text=True, timeout=30,
                           env=dict(os.environ, TZ="America/New_York"))
        assert r.returncode == 0, r.stderr
        res = json.loads(r.stdout)
        assert out is None or out == res, "pages differ: %r vs %r" % (out, res)
        out = res
    return out


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestGoalProgress(unittest.TestCase):
    def test_task_vs_non_task_and_absent(self):
        out = run("""const d = etaProgress({ progress: prog() }, true, NOW), s = etaProgress({ progress: prog() }, false, NOW);
        console.log(JSON.stringify([!!d, s, etaProgress({}, true, NOW), etaProgress({ progress: {} }, true, NOW), etaProgress({ progress: { pct: "x" } }, true, NOW)]));""")
        self.assertEqual(out, [True, None, None, None, None])

    def test_collapsed_row_percent_in_bar_and_est_eta(self):
        out = run("""const w = etaSummary(etaProgress({ progress: prog() }, true, NOW)), bar = w.find((n) => n.className === "gbar")[0];
        console.log(JSON.stringify([bar.attrs.role, bar.attrs["aria-valuenow"], bar.find((n) => n.className === "gpct")[0].textContent, w.find((n) => n.className.indexOf("geta") === 0)[0].textContent]));""")
        self.assertEqual(out, ["progressbar", "40", "40%", "est. Oct 14 18:00"])

    def test_pct_clamped(self):
        out = run("""console.log(JSON.stringify([150, -5, 33.6].map((p) => etaProgress({ progress: prog({ pct: p }) }, true, NOW).pct)));""")
        self.assertEqual(out, [100, 0, 34])

    def test_stale_after_24h(self):
        out = run("""const f = (at) => etaProgress({ progress: prog({ estimated_at: at }) }, true, NOW);
        console.log(JSON.stringify([f("2026-10-08T11:59:00Z").stale, f("2026-10-08T12:01:00Z").stale, f("2026-10-06T12:00:00Z").estText, f("2026-10-09T11:40:00Z").estText]));""")
        self.assertEqual(out, [True, False, "3d ago", "20m ago"])

    def test_overdue_unless_closed(self):
        out = run("""const p = prog({ eta: "2026-10-09T08:00:00Z" });
        const a = etaProgress({ progress: p }, true, NOW), b = etaProgress({ status: "done", progress: p }, true, NOW), c = etaProgress({ progress: prog() }, true, NOW);
        console.log(JSON.stringify([a.overdue, b.overdue, c.overdue, etaChips(a).map((x) => x.textContent)]));""")
        self.assertEqual(out, [True, False, False, ["overdue"]])

    def test_range_rendering(self):
        out = run("""const a = etaProgress({ progress: prog({ eta_low: "2026-10-13T22:00:00Z", eta_high: "2026-10-16T22:00:00Z" }) }, true, NOW);
        const b = etaProgress({ progress: prog({ eta_low: "2026-10-13T22:00:00Z" }) }, true, NOW);
        console.log(JSON.stringify([a.range, b.range, etaDetailRows(a).map((r) => r[0] + "=" + r[1])]));""")
        self.assertEqual(out[0], "Oct 13 18:00 – Oct 16 18:00")
        self.assertEqual(out[1], "")
        self.assertIn("ETA range=Oct 13 18:00 – Oct 16 18:00", out[2])
        self.assertIn("Estimated=estimated 2h ago", out[2])
        self.assertIn("Confidence=medium", out[2])

    def test_eta_class_per_confidence(self):
        out = run("""const cls = (c) => { const w = etaSummary(etaProgress({ progress: prog({ confidence: c }) }, true, NOW)); const e = w.find((n) => n.className.indexOf("geta") === 0)[0]; return [e.className, e.attrs["aria-label"], e.title]; };
        console.log(JSON.stringify([cls("low"), cls("medium"), cls("high"), cls("odd")]));""")
        self.assertEqual([o[0] for o in out], ["geta conf-low", "geta conf-medium", "geta conf-high", "geta"])
        self.assertEqual(out[0][1], "estimated finish Oct 14 18:00, low confidence")
        self.assertEqual(out[0][1], out[0][2])
        self.assertEqual(out[3][1], "estimated finish Oct 14 18:00, odd confidence")

    def test_expanded_rows_colored_and_word_kept(self):
        out = run("""const r = etaDetailRows(etaProgress({ progress: prog({ confidence: "high" }) }, true, NOW));
        console.log(JSON.stringify(r.filter((x) => x[2] && x[1]).map((x) => x[0] + "|" + x[1] + "|" + x[2])));""")
        self.assertEqual(out, ["Estimated finish|est. Oct 14 18:00|conf-high", "Confidence|high|conf-high"])

    def test_tokens_defined_light_and_dark(self):
        for page in PAGES:
            with open(page, encoding="utf-8") as f:
                src = f.read()
            self.assertEqual(len(re.findall(r"--conf-low:", src)), 3, page)
            self.assertIn(".conf-medium { color: var(--conf-med); }", src)

    def test_collapsed_eta_confidence_rule_outranks_geta(self):
        for page in PAGES:
            with open(page, encoding="utf-8") as f:
                src = f.read()
            for c, tok in (("low", "low"), ("medium", "med"), ("high", "high")):
                self.assertIn(".gprog .geta.conf-%s { color: var(--conf-%s); }" % (c, tok), src, page)

    def test_goals_ignore_progress_tasks_use_it(self):
        for page in PAGES:
            with open(page, encoding="utf-8") as f:
                src = f.read()
            goal = src[src.index("function renderGoal("):src.index("function renderGoals()")]
            self.assertNotIn("etaProgress", goal)
            self.assertNotIn("etaSummary", goal)
            self.assertIn("effortSummary(ef, ", goal)
            shell = src[src.index("function shell(it, cls)"):src.index("function shell(it, cls)") + 3500]
            self.assertIn('askKind(it) === "T" ? etaProgress(it, true', shell)
            self.assertNotIn("sessionRollup", src[src.index("function etaProgress("):src.index("/* END etaProgress */")])

    def test_task_overdue_open_vs_done(self):
        out = run("""const p = prog({ eta: "2026-10-09T08:00:00Z" });
        console.log(JSON.stringify(["open", "done", "cancelled", "wont_do"].map((st) => etaProgress({ status: st, progress: p }, true, NOW).overdue)));""")
        self.assertEqual(out, [True, False, False, False])

    def test_effort_per_category_split_fallback_unassigned(self):
        out = run("""const S = (name, usage) => ({ name: name, usage: usage });
        const sess = [S("a", { units_24h: 10, out_tokens_24h: 999 }), S("b", { out_tokens_24h: 30 }), S("c", { units_24h: 60 }), S("d", {})];
        const dir = [{ id: "G1", sessions: ["a", "c"] }, { id: "G3", sessions: [] }], std = [{ id: "G2", sessions: ["c", "c", "zz"] }, { id: "G4", sessions: [] }];
        const r = effortShares(dir, std, sess), g = (id) => r.byGoal.get(id);
        console.log(JSON.stringify([g("G1").spend, g("G2").spend, g("G1").share, g("G3").share, g("G2").share, g("G4").share, r.pools.directed.total, r.pools.standing.total, r.unassigned,
          g("G1").unit, g("G1").rows.map((x) => x.name + ":" + x.n + ":" + x.split), g("G2").rows.length]));""")
        # a: units win over tokens (10); c splits 30/30 across G1 and G2; b (30 tokens) is listed by no goal
        self.assertEqual(out, [40, 30, 100, 0, 100, 0, 40, 30, {"n": 30, "unit": "tokens"}, "units", ["a:10:1", "c:30:2"], 1])

    def test_effort_pool_shares_sum_to_100(self):
        out = run("""const sess = [1, 1, 1].map((n, i) => ({ name: "s" + i, usage: { units_24h: n } }));
        const r = effortShares([{ id: "A", sessions: ["s0"] }, { id: "B", sessions: ["s1"] }, { id: "C", sessions: ["s2"] }], [], sess);
        console.log(JSON.stringify(["A", "B", "C"].map((id) => r.byGoal.get(id).share)));""")
        self.assertEqual(sum(out), 100)
        self.assertEqual(sorted(out), [33, 33, 34])

    def test_effort_zero_pool_no_nan(self):
        out = run("""const r = effortShares([{ id: "G1", sessions: ["a"] }], [], [{ name: "a", usage: {} }]), n = effortShares([{ id: "G1" }], null, undefined);
        console.log(JSON.stringify([r.byGoal.get("G1").share, r.pools.directed.total, r.pools.standing, r.unassigned.n, n.byGoal.get("G1").share, poolText("Standing", r.pools.standing), JSON.stringify(effortDetailRows(r.byGoal.get("G1"), 0))]));""")
        self.assertEqual(out[:6], [0, 0, {"total": 0, "unit": "", "n": 0}, 0, 0, "Standing · 24 h spend 0"])
        self.assertNotIn("NaN", out[6])

    def test_effort_row_text_and_pool_header(self):
        out = run("""const e = effortShares([{ id: "G1", sessions: ["a", "b"] }, { id: "G2", sessions: ["a"] }], [], [{ name: "a", usage: { units_24h: 4 } }, { name: "b", usage: { out_tokens_24h: 12000 } }]);
        const bar = effortSummary(e.byGoal.get("G2"), 0, false).find((n) => n.attrs.role === "progressbar")[0];
        console.log(JSON.stringify([effortDetailRows(e.byGoal.get("G1"), 2), bar.className, bar.textContent, poolText("Directed", e.pools.directed)]));""")
        self.assertEqual(out[0][0], ["Effort (24 h token spend)", "100% · 12k mixed"])
        self.assertEqual(out[0][1], ["Per session", "a 2 units (split 2 ways) · b 12k tokens"])
        self.assertEqual(out[0][2], ["Effort bias", "+2"])
        self.assertEqual(out[1:], ["gbar effort", "effort 0%", "Directed · 24 h spend 12k mixed"])

    def test_bias_clamp_and_text(self):
        out = run("""console.log(JSON.stringify([[5, -9, 2.4, "x", null, 3].map(clampBias), [2, -1, 0].map(biasText)]));""")
        self.assertEqual(out, [[3, -3, 2, 0, 0, 3], ["+2", "\u22121", ""]])

    def test_tune_payloads(self):
        out = run("""const m = tunePayloads("G01", "more", 2, "Owner", "2026-10-09T04:00:00Z"), l = tunePayloads("G01", "less", -3, "Owner", "t");
        console.log(JSON.stringify([m, l, tunePayloads("G01", "more", 3, "Owner", "t"), tunePayloads("G01", "less", 9, "Owner", "t").patch.effort_bias]));""")
        self.assertEqual(out[0], {"patch": {"effort_bias": 3, "effort_bias_at": "2026-10-09T04:00:00Z", "effort_bias_by": "Owner"},
                                  "doc": {"goal": "G01", "direction": "more", "bias_after": 3, "at": "2026-10-09T04:00:00Z", "by": "Owner", "status": "new"}})
        self.assertIsNone(out[1])   # already at -3: nothing to write
        self.assertIsNone(out[2])
        self.assertEqual(out[3], 2)   # a stored 9 clamps to +3 first, then one step down

    def test_pending_chip_and_untrusted_click(self):
        out = run("""const t = [{ goal: "G1", status: "new" }, { goal: "G2", status: "done" }];
        const chip = (p) => effortSummary(effortShares([{ id: "G1" }], [], []).byGoal.get("G1"), 0, p).find((n) => n.className === "chip gstale").length;
        console.log(JSON.stringify([tunePending(t, "G1"), tunePending(t, "G2"), tunePending(t, "G3"), tunePending(null, "G1"), chip(true), chip(false),
          tuneTrusted({ isTrusted: true }), tuneTrusted({ isTrusted: false }), tuneTrusted({}), tuneTrusted(null)]));""")
        self.assertEqual(out, [True, False, False, False, 1, 0, True, False, False, False])

    def test_tune_buttons_guard_untrusted(self):
        for page in PAGES:
            with open(page, encoding="utf-8") as f:
                src = f.read()
            self.assertIn("if (tuneTrusted(ev)) tuneEffort(g, p[0], kind);", src)
            self.assertIn('db.collection("effort_tunes").add(p.doc)', src)
            self.assertIn("await ref.update(p.patch, opts)", src)


if __name__ == "__main__":
    unittest.main()

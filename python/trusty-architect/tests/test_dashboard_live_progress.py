"""Live page session pies: shared progress functions, colour assignment, progress reduction, summary card wiring (ar-G01, the owner 2026-10-29).

Why: the Live page must show the State page's task progress, not a second definition, and give each session a stable colour.
What: (1) every function the Live page copies from templates/dashboard/index.html is byte-identical after dedent; (2) in node, taskSlots colours a pie's tasks
by id (stable, distinct inside a pie, never equal neighbours) and buildProgress makes one slice per open task like the State page (own steps or progress.pct);
(3) the page wires the card (hover, tap pin, Esc) and the palette tokens exist in both themes.
Run: python3 -m unittest tests/test_dashboard_live_progress.py
Test: tests/test_dashboard_live_progress.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DASH = os.path.join(ROOT, "templates", "dashboard")
LIVE = open(os.path.join(DASH, "live.html"), encoding="utf-8").read()
INDEX = open(os.path.join(DASH, "index.html"), encoding="utf-8").read()
TOKENS = open(os.path.join(DASH, "tokens.css"), encoding="utf-8").read()
SHARED = ["txt", "clampPct", "taskProgress", "sessionRollup", "shortDate", "agoText", "confClass", "taskClosed", "etaProgress", "isParked"]


def fn_src(src, name):
    """Source of `function name(` at any indent: a one-liner, or up to the closing brace at the same indent; dedented."""
    lines = src.split("\n")
    i = next(k for k, ln in enumerate(lines) if re.match(r"\s*function %s\(" % name, ln))
    ind = re.match(r"\s*", lines[i]).group(0)
    if lines[i].count("{") == lines[i].count("}"):
        body = [lines[i]]
    else:
        j = next(k for k in range(i + 1, len(lines)) if lines[k] == ind + "}")
        body = lines[i:j + 1]
    return "\n".join(ln[len(ind):] if ln.startswith(ind) else ln for ln in body)


def live_block():
    m = re.search(r"/\* BEGIN liveProgress.*?\*/\n(.*?)/\* END liveProgress \*/", LIVE, re.S)
    assert m
    return m.group(1)


def node(body):
    parts = [live_block()]
    for name in ("hash32", "taskSlots", "buildProgress"):
        parts.append(fn_src(LIVE, name))
    parts.append("const PAL_N = 12, SLICE_MAX = 12;\n" + body)
    r = subprocess.run(["node", "-e", "\n".join(parts)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


class TestLiveProgress(unittest.TestCase):
    def test_shared_functions_match_the_state_page(self):
        for name in SHARED:
            self.assertEqual(fn_src(LIVE, name), fn_src(INDEX, name), name)
        self.assertIn("const ETA_STALE_MS = 24 * 3600 * 1000;", INDEX)
        self.assertIn("const ETA_STALE_MS = 24 * 3600 * 1000;", live_block())

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_task_colours_are_stable_by_id_and_distinct_inside_a_pie(self):
        r = node("""
const ids = ["T1", "T2", "T3", "ar-T91", "x"], a = taskSlots(ids), again = taskSlots(ids);
const twelve = []; for (let i = 0; i < 12; i++) twelve.push("id" + i);
const twenty = []; for (let i = 0; i < 20; i++) twenty.push("task-" + i);
const t20 = taskSlots(twenty);
console.log(JSON.stringify({ stable: a.join() === again.join(), distinct5: new Set(a).size, distinct12: new Set(taskSlots(twelve)).size,
  neighbours: t20.every((s, i) => i === 0 || s !== t20[i - 1]) && t20[0] !== t20[19], range: t20.every((s) => s >= 0 && s < 12), single: taskSlots(["only"]).length }));""")
        self.assertEqual(r, {"stable": True, "distinct5": 5, "distinct12": 12, "neighbours": True, "range": True, "single": 1})

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_progress_gives_one_slice_per_open_task_like_the_state_page(self):
        r = node("""
const now = Date.parse("2026-10-29T12:00:00Z");
const sessions = [{ name: "s-own", progress: 60, updated: "2026-10-29T11:00:00Z", summary: "sum" }, { name: "s-steps" }, { name: "s-none" }, { name: "s-one", progress: 40 }];
const items = [{ id: "T1", kind: "task", status: "open", session: "s-steps", title: "second", steps: [{ id: "a" }, { id: "b" }] },
  { id: "T0", kind: "task", status: "open", session: "s-steps", title: "first", steps: [{ id: "a" }, { id: "b" }], status_note: "note", status_at: "2026-10-29T11:30:00Z" },
  { id: "T2", kind: "task", status: "open", session: "s-steps", progress: { pct: 80 }, title: "explicit" },
  { id: "T3", kind: "task", status: "open", session: "s-steps", parked: { until: "2026-11-01T00:00:00Z" }, title: "parked" },
  { id: "O1", kind: "task", status: "open", session: "s-one", title: "lone, no figure" },
  { id: "D1", kind: "decision", status: "open", session: "s-none" }];
const by = buildProgress(sessions, items, [{ id: "T0", data: { steps: { a: { done: true } } } }], now);
console.log(JSON.stringify({ own: by["s-own"], steps: by["s-steps"].slices.map((s) => [s.id, s.pct, s.done, s.total]), note: by["s-steps"].slices[0].note, none: by["s-none"], one: by["s-one"].slices }));""")
        self.assertEqual(r["own"]["slices"], [])   # no open task: the page draws one slice from the session figure
        self.assertEqual(r["own"]["pct"], 60)
        self.assertEqual(r["steps"], [["T0", 50, 1, 2], ["T1", 0, 0, 2], ["T2", 80, None, None]])   # sorted by id; parked task left out
        self.assertEqual(r["note"], "note")
        self.assertIsNone(r["none"]["pct"])   # not tracked is null, never 0
        self.assertEqual(r["one"][0]["pct"], 40)   # a lone task with no figure inherits the session's
        self.assertTrue(r["one"][0]["inherited"])

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_fan_rank_follows_token_usage_with_a_stability_band(self):
        src = "const tokRank = " + re.search(r"const tokRank = (.*?);\n", LIVE).group(1) + ";\n" + fn_src(LIVE, "rankBy")
        js = src + """
const l = [{ n: "a", t: 1000 }, { n: "b", t: 90000 }, { n: "c", t: 5000 }, { n: "d", t: 5050 }];
const r1 = rankBy(l, (o) => tokRank(o.t)).map((o) => o.n).join("");
l[3].t = 4950; l[2].t = 5050;   // c and d trade places inside the 10% band: nobody moves
const r2 = rankBy(l, (o) => tokRank(o.t)).map((o) => o.n).join("");
console.log(JSON.stringify([r1, r2]));"""
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), ["bcda", "bcda"])
        self.assertIn("rankBy(PMS, (p) => tokRank(p.h.tok1h))", LIVE)

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_active_fan_reads_top_to_bottom_by_rank_and_reorders(self):
        """the owner 2026-10-29: biggest at top, re-order when one passes the one above. Rank r sits strictly lower than rank r-1 (y = -cos(offset))."""
        src = "const tokRank = " + re.search(r"const tokRank = (.*?);\n", LIVE).group(1) + ";\n" + fn_src(LIVE, "rankBy") + "\n" + fn_src(LIVE, "fanOffset") + "\n" + fn_src(LIVE, "fanStagger") + "\n" + fn_src(LIVE, "fanPlan") + "\nconst ACT_SPAN = 2.27, INACT_STEP = 0.05, INACT_HALF = 0.3;"
        js = src + """
const ys = (n, nI) => fanPlan(n, nI).act.map((o) => -Math.cos(o));
const mono = [2, 3, 4, 5, 6, 9].every((n) => [0, 2].every((nI) => ys(n, nI).every((y, i, a) => i === 0 || y > a[i - 1] + 1e-9)));
const l = [{ n: "a", t: 1000 }, { n: "b", t: 90000 }, { n: "c", t: 5000 }];
const r1 = rankBy(l, (o) => tokRank(o.t)).map((o) => o.n).join("");
l[0].t = 200000;   // a passes b: a takes the top slot
const r2 = rankBy(l, (o) => tokRank(o.t)).map((o) => o.n).join("");
console.log(JSON.stringify([mono, r1, r2]));"""
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), [True, "bca", "abc"])

    def test_page_wires_slice_card_tap_esc_and_palette(self):
        for hook in ('id="sumcard"', "function paintProgress(p)", "showCard(p, e.clientX, e.clientY, true, ang)", "function sliceAt(p, ang)", "no open task item; session figure and summary below",
                     'e.key === "Escape" && CARD.pin', "setInterval(loadProgress, PROG_MS)", "min(300px,calc(100vw - 16px))"):
            self.assertIn(hook, LIVE)
        for i in range(1, 13):   # light, OS dark, data-theme dark
            self.assertEqual(len(re.findall(r"--pal-%d: #[0-9a-f]{6};" % i, TOKENS)), 3, i)


if __name__ == "__main__":
    unittest.main()

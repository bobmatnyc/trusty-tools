"""Dashboard pane view (#11065 phase 4): Decision row "Last output" and the page "Show session" control over GET /api/pane.

Why: pane text is untrusted terminal output; it must reach the DOM as text, never be stored, and a late answer for one
session must never paint into a view that now shows another.
What: runs the page's paneTail block under node against a fake DOM and a scripted fetch; parses the page CSS for the
360 px and 44 px rules.
Test: tests/test_dashboard_pane_view.py
"""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_mobile import css_rules
from test_dashboard_progress import FAKE, PAGES

DOM = r"""
N.prototype.replaceChildren = function (...cs) { this.children = []; this.append(...cs); };
const _app = N.prototype.appendChild;
N.prototype.appendChild = function (c) { if (c.parentNode) c.parentNode.children = c.parentNode.children.filter((x) => x !== c); c.parentNode = this; return _app.call(this, c); };
N.prototype.append = function (...cs) { cs.forEach((c) => this.appendChild(typeof c === "string" ? Object.assign(new N("#text"), { _t: c }) : c)); };
N.prototype.fire = function (ev) { if (this.listeners[ev]) return this.listeners[ev](); };
const calls = [];
let routes = {};   /* session -> {status, body, delay} or "throw" */
global.fetch = async (url) => {
  calls.push(url);
  const s = decodeURIComponent(/session=([^&]*)/.exec(url)[1]), r = routes[s];
  if (r === "throw") throw new TypeError("network down");
  await new Promise((ok) => setTimeout(ok, (r && r.delay) || 1));
  return { ok: r.status < 400, status: r.status, json: async () => r.body };
};
const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));
const SESSION = false;
let fleet = null, sessions = [];
const view = (v) => ({ name: v.name, state: v.msg.dataset.state, msg: v.msg.textContent, lines: v.pre.textContent, age: v.age.textContent, trunc: !v.trunc.hidden });
const ok = (lines, extra) => Object.assign({ status: 200, body: { session: "x", lines: lines, captured_at: new Date().toISOString(), truncated: false } }, extra || {});
"""


def block(src):
    m = re.search(r"/\* BEGIN paneTail \*/(.*?)/\* END paneTail \*/", src, re.S)
    assert m, "paneTail block missing"
    return m.group(1)


def run(page, body):
    src = open(page, encoding="utf-8").read()
    js = FAKE + DOM + block(src) + "\n(async () => {\n" + body + "\n})().catch((e) => { console.error(e); process.exit(1); });"
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


class TestPaneViewStatic(unittest.TestCase):
    def test_pane_text_reaches_the_dom_as_text_and_is_never_stored_or_logged(self):
        for page in PAGES:
            b = re.sub(r"/\*.*?\*/", "", block(open(page, encoding="utf-8").read()), flags=re.S)   # code only
            for bad in ("innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "localStorage", "sessionStorage",
                        "console.", "history.", "location", "openState", "saveOpen"):
                self.assertFalse(bad in b, bad)
            self.assertFalse("`" in b, "no template strings in the pane block")


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestPaneView(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=page):
                yield run(page, body)

    def test_markup_in_pane_lines_stays_text(self):
        body = """
        routes = { "tm-a": ok(["<img src=x onerror=alert(1)>", "<b>bold</b>"], { body: { session: "<i>x</i>", lines: ["<img src=x onerror=alert(1)>", "<b>bold</b>"], captured_at: new Date().toISOString(), truncated: true } }) };
        const v = paneView(); await v.show("tm-a");
        const tags = v.box.find((n) => !["div", "span", "button", "p", "pre", "#text"].includes(n.tagName)).map((n) => n.tagName);
        console.log(JSON.stringify(Object.assign(view(v), { tags })));"""
        for r in self.each(body):
            self.assertEqual(r["lines"], "<img src=x onerror=alert(1)>\n<b>bold</b>")
            self.assertEqual(r["tags"], [])
            self.assertEqual(r["name"], "tm-a")   # the requested name, not the response's
            self.assertTrue(r["trunc"])
            self.assertEqual(r["state"], "ok")

    def test_404_503_and_network_failure_clear_the_previous_lines_and_differ(self):
        body = """
        const out = [], v = paneView();
        routes = { "tm-a": ok(["secret-ish line"]) }; await v.show("tm-a"); out.push(view(v));
        routes = { "tm-a": { status: 404, body: { error: "not_running" } } }; await v.refresh(); out.push(view(v));
        routes = { "tm-a": ok(["again"]) }; await v.refresh();
        routes = { "tm-a": { status: 503, body: { error: "capture_failed" } } }; await v.refresh(); out.push(view(v));
        routes = { "tm-a": ok(["again"]) }; await v.refresh();
        routes = { "tm-a": "throw" }; await v.refresh(); out.push(view(v));
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r[0]["lines"], "secret-ish line")
            for x in r[1:]:
                self.assertEqual(x["lines"], "")
                self.assertNotIn("Loading", x["msg"])
                self.assertEqual(x["age"], "")
            self.assertEqual(r[1]["state"], "none")
            self.assertIn("No live pane", r[1]["msg"])
            self.assertEqual([r[2]["state"], r[3]["state"]], ["error", "error"])
            self.assertEqual(len({r[1]["msg"], r[2]["msg"], r[3]["msg"]}), 3)

    def test_slow_answer_for_a_never_paints_into_a_view_showing_b(self):
        body = """
        routes = { "tm-a": ok(["A line"], { delay: 120 }), "tm-b": ok(["B line"], { delay: 5 }) };
        const v = paneView();
        const pa = v.show("tm-a"); await sleep(10);
        await v.show("tm-b"); const mid = view(v);
        await pa; await sleep(150);
        console.log(JSON.stringify([mid, view(v), calls.length]));"""
        for r in self.each(body):
            self.assertEqual(r[0]["lines"], "B line")
            self.assertEqual(r[1]["lines"], "B line")
            self.assertEqual(r[1]["name"], "tm-b")
            self.assertEqual(r[1]["state"], "ok")

    def test_stale_age_label_follows_the_clock(self):
        body = """
        const t = Date.parse("2026-11-14T10:00:00Z");
        console.log(JSON.stringify([paneAge("2026-11-14T10:00:00Z", t + 2000), paneAge("2026-11-14T10:00:00Z", t + 42000),
          paneAge("2026-11-14T10:00:00Z", t + 7 * 60000), paneAge("2026-11-14T10:00:00Z", t + 3 * 3600000), paneAge("junk", t)]));
        """
        for r in self.each(body):
            self.assertEqual(r, ["captured just now", "captured 42 s ago", "captured 7 min ago", "captured 3 h ago", "capture time unknown"])
        body = """
        routes = { "tm-a": { status: 200, body: { lines: ["x"], captured_at: new Date(Date.now() - 1000).toISOString(), truncated: false } } };
        const v = paneView(); await v.show("tm-a"); const a = v.age.textContent; v.tick(Date.now() + 120000);
        console.log(JSON.stringify([a, v.age.textContent]));"""
        for r in self.each(body):
            self.assertEqual(r, ["captured just now", "captured 2 min ago"])

    def test_item_session_resolves_to_its_own_tmux_name_or_no_pane(self):
        body = """
        const f = { sessions: { "tm-a": { managed: true }, "tm-b": { managed: true }, "tm-x": { managed: false } } };
        const docs = [{ id: "S0a", name: "tm-a" }, { id: "tm-b", name: "tm-c" }];
        console.log(JSON.stringify([paneName("tm-a", f, docs), paneName("S0a", f, docs), paneName("tm-b", f, docs),
          paneName("tm-gone", f, docs), paneName("tm-x", f, docs), paneName("S0a", null, docs), paneName("", f, docs)]));"""
        for r in self.each(body):
            # "tm-b" is both a live tmux name and another doc's id: the name wins, so it never maps to tm-c's pane
            self.assertEqual(r, ["tm-a", "tm-a", "tm-b", "", "", "tm-a", ""])

    def test_last_output_fetches_only_when_opened_and_survives_rerender(self):
        body = """
        fleet = { sessions: { "tm-a": { managed: true } } };
        routes = { "tm-a": ok(["row line"]) };
        const it = { id: "D01", session: "tm-a" };
        const d1 = lastOutput(it); await sleep(20); const before = calls.length;
        d1.open = true; d1.fire("toggle"); await sleep(20);
        const d2 = lastOutput(it); await sleep(20);
        const pre = d2.find((n) => n.tagName === "pre")[0];
        const gone = lastOutput({ id: "D02", session: "tm-off" }); gone.open = true; gone.fire("toggle"); await sleep(20);
        const msg = gone.find((n) => n.className === "pane-msg")[0];
        console.log(JSON.stringify([before, calls.length, d2.open, pre && pre.textContent, msg.dataset.state, lastOutput({ id: "D03" })]));"""
        for r in self.each(body):
            self.assertEqual(r[:4], [0, 1, True, "row line"])
            self.assertEqual(r[4], "none")    # not a live managed session: no fetch, "no live pane"
            self.assertIsNone(r[5])

    def test_page_control_lists_managed_sessions_and_shows_on_click(self):
        body = """
        routes = { "tm-b": ok(["page line"]) };
        const c = paneControl(); c.sync({ sessions: { "tm-b": { managed: true }, "tm-a": { managed: true }, "me": { managed: false } } });
        const sel = c.find((n) => n.tagName === "select")[0], go = c.find((n) => n.tagName === "button" && n.textContent === "Show")[0];
        const opts = sel.children.map((o) => o.value); const n0 = calls.length;
        sel.value = "tm-b"; go.fire("click"); await sleep(20);
        console.log(JSON.stringify([opts, n0, view(c.view)]));"""
        for r in self.each(body):
            self.assertEqual(r[0], ["tm-a", "tm-b"])
            self.assertEqual(r[1], 0)
            self.assertEqual(r[2]["lines"], "page line")


class TestPaneViewMobile(unittest.TestCase):
    def test_new_targets_are_44px_and_nothing_is_wider_than_360px(self):
        for page in PAGES:
            rules = css_rules(open(page, encoding="utf-8").read())
            touch = {s for sels, d in rules if d.get("min-height") in ("44px", "var(--h-touch)") for s in sels}
            for sel in (".pane-last > summary", ".pane-out .btn", ".pane-pick .btn", ".pane-pick select", ".sub > summary"):
                self.assertIn(sel, touch, sel)
            for sels, d in rules:
                if any(s.startswith(".pane-") for s in sels):
                    for prop in ("width", "min-width", "flex-basis"):
                        m = re.fullmatch(r"(\d+)px", d.get(prop, ""))
                        self.assertFalse(m and int(m.group(1)) > 360, (sels, prop))

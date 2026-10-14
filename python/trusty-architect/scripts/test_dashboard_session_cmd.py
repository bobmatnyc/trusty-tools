"""Dashboard session controls (run under node, skipped when node is absent), checked on both HTML files:
the buttons depend on the state (idle: Park, Activate, Prioritize; parked: Activate, Prioritize;
exited: Restart, Park; working or waiting: Prioritize; the Architect's own rows: no Park or Restart); Send writes
sessions/<S>.command pinned with if_version; the row shows "sent to the Architect" until applied."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE, PAGES


def run(page, body):
    src = open(page, encoding="utf-8").read()
    part = re.search(r"/\* BEGIN sessionCmd \*/(.*?)/\* END sessionCmd \*/", src, re.S).group(1)
    js = FAKE + """
const calls = []; let readOnly = false; const renders = []; const render = () => renders.push(1);
const $ = () => null; const userName = () => "Owner";
let updated = null;
let db = { collection: (c) => ({ doc: (id) => ({ get: async () => ({ version: 4 }), update: async (patch, opts) => { updated = { c, id, patch, opts }; } }) }) };
""" + part + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardSessionCmd(unittest.TestCase):
    def each(self, body):
        for page in PAGES:
            with self.subTest(page=os.path.basename(os.path.dirname(page))):
                yield run(page, body)

    def test_controls_render_by_state_and_hide_when_read_only(self):
        body = """
        const out = {};
        for (const st of ["idle", "parked", "exited", "waiting-owner", "waiting-ci", "working"]) {
          const b = el("div"); sessionCommandControl(b, { id: "S01", name: "tm-x", state: st }, [{ id: "G01", title: "t" }]);
          out[st] = b.find((n) => n.tagName === "button").map((n) => n.textContent);
        }
        readOnly = true; const b = el("div"); sessionCommandControl(b, { id: "S01", state: "idle" }, []); out.ro = b.children.length;
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, {"idle": ["Park", "Activate", "Prioritize"], "parked": ["Activate", "Prioritize"],
                                 "exited": ["Restart", "Park"], "waiting-owner": ["Prioritize"], "waiting-ci": ["Prioritize"],
                                 "working": ["Prioritize"], "ro": 0})

    def test_architect_rows_have_no_park_or_restart(self):
        body = """
        const out = {};
        for (const name of ["tm-architect", "tm-architect-poll"]) for (const st of ["idle", "exited"]) {
          const b = el("div"); sessionCommandControl(b, { id: "S01", name, state: st }, []);
          out[name + "/" + st] = b.find((n) => n.tagName === "button").map((n) => n.textContent);
        }
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r, {"tm-architect/idle": ["Activate", "Prioritize"], "tm-architect/exited": [],
                                 "tm-architect-poll/idle": ["Activate", "Prioritize"], "tm-architect-poll/exited": []})

    def test_forms_have_goal_picker_and_optional_text(self):
        body = """
        const s = { id: "S01", name: "tm-x", state: "idle" }; const goals = [{ id: "G01", title: "one" }, { id: "G02", title: "two" }];
        const out = {};
        for (const [label, idx] of [["Prioritize", 2], ["Activate", 1], ["Park", 0]]) {
          cmdOpen.clear(); const b = el("div"); sessionCommandControl(b, s, goals); b.find((n) => n.tagName === "button")[idx].listeners.click();
          const b2 = el("div"); sessionCommandControl(b2, s, goals);
          out[label] = { sel: tag(b2, "select").map((x) => x.children.map((o) => o.value)), inputs: tag(b2, "input").length,
                         btns: b2.find((n) => n.tagName === "button").map((n) => n.textContent) };
        }
        console.log(JSON.stringify(out));"""
        for r in self.each(body):
            self.assertEqual(r["Prioritize"], {"sel": [["G01", "G02"]], "inputs": 1, "btns": ["Send", "Cancel"]})
            self.assertEqual(r["Activate"], {"sel": [], "inputs": 1, "btns": ["Send", "Cancel"]})
            self.assertEqual(r["Park"], {"sel": [], "inputs": 1, "btns": ["Send", "Cancel"]})

    def test_send_writes_command_with_if_version_then_shows_sent_until_applied(self):
        body = """
        (async () => {
          const s = { id: "S03", name: "tm-memory", state: "idle" };
          await sendSessionCommand(s, { type: "activate", text: "  #9451  " });
          const a = updated;
          await sendSessionCommand(s, { type: "prioritize", goal: "G02", note: " first " });
          const p = updated;
          const shown = withCommand({ id: "S03" }, optimisticCmd);
          const b = el("div"); sessionCommandControl(b, shown, []);
          const echoed = withCommand({ id: "S03", command: p.patch.command }, optimisticCmd);
          const applied = Object.assign({}, echoed, { command: Object.assign({}, echoed.command, { status: "applied", reply: "relayed" }) });
          const b2 = el("div"); sessionCommandControl(b2, applied, []);
          console.log(JSON.stringify({ a: a.patch.command, ac: a.c, aid: a.id, aopts: a.opts, p: p.patch.command, popts: p.opts,
            sent: b.textContent, sentBtns: b.find((n) => n.tagName === "button").length, sentFlag: commandSent(shown),
            cleared: optimisticCmd.size, applied: b2.textContent, appliedFlag: commandSent(applied) }));
        })();"""
        for r in self.each(body):
            self.assertEqual((r["ac"], r["aid"], r["aopts"], r["popts"]), ("sessions", "S03", {"if_version": 4}, {"if_version": 4}))
            ac = r["a"]
            self.assertEqual({k: ac[k] for k in ("type", "text", "by", "status")}, {"type": "activate", "text": "#9451", "by": "Owner", "status": "new"})
            self.assertTrue(ac["at"])
            pc = r["p"]
            self.assertEqual({k: pc[k] for k in ("type", "goal", "note", "status")}, {"type": "prioritize", "goal": "G02", "note": "first", "status": "new"})
            self.assertIn("sent to the Architect", r["sent"])
            self.assertEqual((r["sentBtns"], r["sentFlag"], r["cleared"], r["appliedFlag"]), (0, True, 0, False))
            self.assertIn("Last command applied: relayed", r["applied"])

    def test_park_and_restart_write_the_command_and_show_requested_until_applied(self):
        body = """
        (async () => {
          const s = { id: "S03", name: "tm-memory", state: "exited" };
          await sendSessionCommand(s, { type: "restart", note: "  after build " });
          const r = updated;
          const shown = withCommand({ id: "S03" }, optimisticCmd);
          const b = el("div"); sessionCommandControl(b, shown, []);
          await sendSessionCommand(s, { type: "park", note: "" });
          const p = updated;
          const b2 = el("div"); sessionCommandControl(b2, withCommand({ id: "S03" }, optimisticCmd), []);
          console.log(JSON.stringify({ r: r.patch.command, ropts: r.opts, p: p.patch.command, restart: b.textContent, park: b2.textContent,
            btns: b.find((n) => n.tagName === "button").length }));
        })();"""
        for r in self.each(body):
            self.assertEqual(r["ropts"], {"if_version": 4})
            rc = r["r"]
            self.assertEqual({k: rc[k] for k in ("type", "note", "by", "status")}, {"type": "restart", "note": "after build", "by": "Owner", "status": "new"})
            self.assertNotIn("goal", rc)
            self.assertEqual((r["p"]["type"], r["p"]["note"], r["p"]["status"]), ("park", "", "new"))
            self.assertIn("Restart requested", r["restart"])
            self.assertIn("Park requested", r["park"])
            self.assertEqual(r["btns"], 0)

    def test_failed_write_restores_form_and_drops_optimistic_command(self):
        body = """
        (async () => {
          db = { collection: () => ({ doc: () => ({ get: async () => ({ version: 1 }), update: async () => { throw new Error("nope"); } }) }) };
          const s = { id: "S03", name: "tm-memory", state: "idle" };
          const ok = await sendSessionCommand(s, { type: "activate", text: "x" });
          console.log(JSON.stringify({ ok, pending: optimisticCmd.size, open: cmdOpen.has("S03") }));
        })();"""
        for r in self.each(body):
            self.assertEqual(r, {"ok": False, "pending": 0, "open": True})


if __name__ == "__main__":
    unittest.main()

"""Dashboard "Respond interactively" control (the owner 2026-10-26): on every open D and A it closes the item with status
`interactive`; the Architect sees it as pending kind `interactive` and asks the owner in the terminal chat.

Why: some decisions and actions are easier to talk through than to click.
What: the control renders on open D/A only; the press writes status/answer/answered_at with if_version; the item is
closed (not counted open); pending-answers lists "INTERACTIVE: <id> <title>" until relayed or acknowledged; the poller
raises owner_interactive (not owner_answer); the hook line carries the label.
Test: tests/test_dashboard_interactive.py
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

from test_dashboard_progress import FAKE, PAGES

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name, fname):
    spec = importlib.util.spec_from_file_location(name, os.path.join(os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"), fname))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def run(page, body):
    src = open(page, encoding="utf-8").read()
    parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "interactive")]
    bucket = re.search(r"  function bucketItems\(list\) \{.*?\n  \}\n", src, re.S).group(0)
    js = FAKE + """
let db = { collection: () => ({ doc: () => ({ get: async () => ({ version: 7 }) }) }) }, readOnly = false;
const userName = () => "owner"; const render = () => {}; const collapsed = [];
const collapseItem = (c, id) => collapsed.push(id); const reopenItem = () => {}; const rowFor = () => null;
N.prototype.closest = () => ({});
const writes = []; async function write(id, patch, opts) { writes.push({ id, patch, opts }); return true; }
""" + "\n".join(parts) + bucket + "\n" + body
    r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestPage(unittest.TestCase):
    def test_control_on_open_decision_and_action_only(self):
        body = """
        const out = {};
        for (const it of [{ id: "D01", status: "open" }, { id: "A02", status: "open" }, { id: "A04", status: "answered" }]) {
          const b = el("div"); interactiveControl(b, it);
          out[it.id] = b.find((n) => n.tagName === "button").map((n) => n.textContent);
        }
        console.log(JSON.stringify(out));"""
        for page in PAGES:
            self.assertEqual(run(page, body), {"D01": ["Respond interactively"], "A02": ["Respond interactively"], "A04": []})
            src = open(page, encoding="utf-8").read()
            fn = lambda name: re.search(r"  function %s\(it\) \{.*?\n  \}\n" % name, src, re.S).group(0)
            self.assertIn("interactiveControl(body, it, tools)", fn("renderQuestion"))
            self.assertIn("interactiveControl(body, it, tools)", fn("renderAction"))
            self.assertNotIn("interactiveControl", fn("renderTask"))

    def test_press_closes_with_fields_and_if_version_and_is_not_open(self):
        body = """
        (async () => {
          const it = { id: "D07", status: "open", title: "T" }; const b = el("div");
          interactiveControl(b, it); b.find((n) => n.tagName === "button")[0].listeners.click();
          await new Promise((r) => setTimeout(r, 10));
          const closed = Object.assign({}, it, writes[0].patch);
          const bk = bucketItems([closed, { id: "D08", status: "open" }]);
          console.log(JSON.stringify({ writes, collapsed, open: bk.open.map((i) => i.id), done: bk.done.map((i) => i.id) }));
        })();"""
        for page in PAGES:
            r = run(page, body)
            self.assertEqual(len(r["writes"]), 1)
            w = r["writes"][0]
            self.assertEqual((w["id"], w["opts"]), ("D07", {"if_version": 7}))
            p = w["patch"]
            self.assertEqual((p["status"], p["answer"]), ("interactive", "Respond interactively"))
            self.assertTrue(p["answered_at"] and p["updated"] == p["answered_at"])
            self.assertEqual((r["open"], r["done"], r["collapsed"]), (["D08"], ["D07"], ["D07"]))


class TestPending(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        os.makedirs(os.path.join(self.tmp, "items"))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.pa = load("pa", "pending-answers.py")
        self.fp = load("fp", "fleet-poll.py")

    def put(self, item_id, **data):
        with open(os.path.join(self.tmp, "items", item_id + ".json"), "w") as f:
            json.dump(dict(data, version=1), f)

    def test_pending_lists_interactive_until_relayed_or_acknowledged(self):
        import owner_answers
        at = "2026-10-26T16:10:00Z"
        self.put("D90", status="interactive", title="Pick a thing", answer="Respond interactively", answered_at=at)
        self.put("D91", status="interactive", title="x", answered_at=at, answer_note="Acknowledged 16:20Z")
        self.put("D92", status="interactive", title="x", answered_at=at, relayed="2026-10-26T16:30:00Z s")
        self.put("D93", status="open", title="open one")
        got = [g for g in owner_answers.pending(self.tmp)]
        self.assertEqual([(g["id"], g["kind"], g["at"], g["text"]) for g in got], [("D90", "interactive", at, "Pick a thing")])

    def test_pending_answers_cli_label_and_exit_code(self):
        import io
        from contextlib import redirect_stdout
        self.put("D90", status="interactive", title="Pick a thing", answered_at="2026-10-26T16:10:00Z")
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = self.pa.main(["--root", self.tmp])
        self.assertEqual(rc, 1)
        self.assertIn("INTERACTIVE: D90 Pick a thing", buf.getvalue())


    def test_hook_line_carries_label(self):
        d = load("d", "pending_hook_dedupe.py")
        items = [{"id": "D90", "kind": "interactive", "at": "t", "text": "Pick a thing"}]
        line = d.decide(items, os.path.join(self.tmp, "st.json"), "s1", now=1000.0)
        self.assertIn("INTERACTIVE: D90 Pick a thing", line)


if __name__ == "__main__":
    unittest.main()

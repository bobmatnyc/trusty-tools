"""Tasks (T) render no note control; asks (A) and decisions (D) keep theirs (owner 2026-10-09)."""
import json
import os
import re
import shutil
import subprocess
import unittest

from test_dashboard_progress import FAKE

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEMPLATE = os.path.join(ROOT, "templates/dashboard/index.html")


def fn_body(src, name):
    m = re.search(r"  function %s\(.*?\n  }\n" % name, src, re.S)
    assert m, name
    return m.group(0)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestDashboardTaskNotes(unittest.TestCase):
    def step_controls(self, item, prog):
        src = open(TEMPLATE, encoding="utf-8").read()
        parts = [re.search(r"/\* BEGIN %s \*/(.*?)/\* END %s \*/" % (n, n), src, re.S).group(1) for n in ("askKind", "stepNotes")]
        js = FAKE + "const noteOpen = new Set(), noteDrafts = new Map(); const addText = (p, t) => p.append(txt(t)); const utc = (v) => v; const render = () => {}; const saveStep = () => {};\n" \
            + "\n".join(parts) + """
        const body = el("div");
        stepNotes(body, %s, { id: "1" }, %s, true);
        console.log(JSON.stringify({ buttons: body.find((n) => n.tagName === "button").map((n) => n.textContent), text: body.textContent }));""" % (json.dumps(item), json.dumps(prog))
        r = subprocess.run(["node", "-e", js], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_task_step_has_no_note_control_even_with_a_stored_note(self):
        for prog in ({}, {"note": "old note", "note_at": "2026-10-08T00:00:00Z"}):
            out = self.step_controls({"id": "T12", "kind": "task"}, prog)
            self.assertEqual(out, {"buttons": [], "text": ""})

    def test_ask_step_keeps_the_note_control(self):
        out = self.step_controls({"id": "A20", "kind": "action"}, {})
        self.assertEqual(out["buttons"], ["Add note"])

    def test_task_card_has_no_note_input_and_ask_card_keeps_it(self):
        for page in (TEMPLATE,):
            src = open(page, encoding="utf-8").read()
            task = fn_body(src, "renderTask")
            self.assertNotIn("Optional note", task, page)
            self.assertNotIn("Add note", task, page)
            self.assertIn("cancelControl(", task, page)
            self.assertIn("Optional note", fn_body(src, "renderAction"), page)


if __name__ == "__main__":
    unittest.main()

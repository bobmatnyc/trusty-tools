"""Steer on Intent Ledger task rows (owner 2026-10-21: "Also let's add a 'Steer' input for tasks").

An IL task has no dashboard item, so the Architect IL pane POSTs one il_steers doc {status, at, session, task, text, by}.
owner_answers.pending() lists each status-new doc as kind "steer" with task and session, and fleet-poll turns it into a
`steer` alert that names the IL task and its session. Task-section Steer (items.steer_requests) is unchanged.
Run: python3 -m unittest tests/test_dashboard_il_steer.py
Test: tests/test_dashboard_il_steer.py
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, HERE)
import dashboard_schema as dsch  # noqa: E402
import owner_answers  # noqa: E402
from test_dashboard_store import ServerCase, serve  # noqa: E402

_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(SCRIPTS, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

PAGE = serve.PAGE
DOC = {"status": "new", "at": "2026-10-21T08:50:00Z", "session": "tm-apex", "task": "ap-T12", "text": "Skip the flaky test", "by": "owner"}


def put(root, coll, doc_id, **data):
    d = os.path.join(root, coll)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, doc_id + ".json"), "w") as f:
        json.dump(dict(data, version=1), f)


class TestDerive(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = self._tmp.name

    def test_new_il_steer_is_pending_with_task_and_session(self):
        put(self.root, "il_steers", "abc", **DOC)
        got = [p for p in owner_answers.pending(self.root) if p["kind"] == "steer"]
        self.assertEqual(got, [{"id": "abc", "kind": "steer", "at": DOC["at"], "text": DOC["text"], "task": "ap-T12", "session": "tm-apex"}])

    def test_relayed_il_steer_is_quiet(self):
        put(self.root, "il_steers", "abc", **dict(DOC, status="relayed", relayed_at="2026-10-21T09:00:00Z"))
        self.assertEqual(owner_answers.pending(self.root), [])



class TestSchemaAndEndpoint(ServerCase):
    def test_schema_accepts_the_doc_and_refuses_a_missing_task(self):
        self.assertEqual(dsch.validate("il_steers", "abc", DOC), [])
        self.assertTrue(dsch.validate("il_steers", "abc", {k: v for k, v in DOC.items() if k != "task"}))

    def test_post_creates_the_doc_pending_sees_it(self):
        s, b = self.call("POST", "/api/il_steers", {"data": DOC})
        self.assertEqual(s, 201)
        self.assertEqual(json.loads(self.call("GET", "/api/il_steers/" + json.loads(b)["id"])[1])["data"]["task"], "ap-T12")


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestPage(unittest.TestCase):
    def setUp(self):
        self.src = open(PAGE, encoding="utf-8").read()
        self.il = self.src[self.src.index("/* BEGIN ilLedgers"):self.src.index("/* END ilLedgers */")]

    def test_task_rows_carry_the_steer_control_and_post_to_il_steers(self):
        self.assertIn('steerFor(li, s.session, t); }));', self.il)
        self.assertIn('send("POST", "/api/il_steers"', self.il)
        self.assertNotIn("steerFor", re.search(r'list\("Items open/answered".*', self.il).group(0))   # tasks only
        self.assertNotIn("steerFor", re.search(r'list\("Goals".*', self.il).group(0).split("\n")[0])

    def test_poll_does_not_repaint_over_an_open_steer_field(self):
        self.assertIn('rows.querySelector(".steer-form:not([hidden])")', self.il)
        self.assertIn("if (dirty) paint(true);", self.il)

    def test_steer_doc_matches_the_schema(self):
        line = re.search(r"  function steerDoc\(.*\n", self.il).group(0)
        r = subprocess.run(["node", "-e", line + 'console.log(JSON.stringify(steerDoc("tm-apex", "ap-T12", "Skip it", "owner", new Date("2026-10-21T08:50:00.123Z"))));'],
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        doc = json.loads(r.stdout)
        self.assertEqual(doc, {"status": "new", "at": "2026-10-21T08:50:00Z", "session": "tm-apex", "task": "ap-T12", "text": "Skip it", "by": "owner"})
        self.assertEqual(dsch.validate("il_steers", "x", doc), [])

    def test_task_section_rows_keep_their_steer(self):
        # renderTask -> parkControl -> steerControl, for parked and unparked open tasks
        self.assertEqual(self.src.count('if (askKind(it) === "T") steerControl(row, it);'), 2)


if __name__ == "__main__":
    unittest.main()

"""Goal Mark complete (owner 2026-10-21 14:24Z, 14:26Z): the pure goalDone region runs under node (skipped when node is absent);
the batch it builds is applied to a real store, pinned with if_version and all-or-nothing.

Why: completing a goal also closes its open tasks, in one write, without touching decisions or asks, and a stale version
anywhere must leave every doc unchanged.
What: patch fields, standing goals refused, the cascade set, the ops (goal first, then tasks, pinned), reopen leaves tasks
done, and the batch against the store (success and a version conflict).
Test: this file.
"""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

import dashboard_store as ds

PAGE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "templates", "dashboard", "index.html")


def node(body):
    src = open(PAGE, encoding="utf-8").read()
    region = re.search(r"/\* BEGIN goalDone \*/(.*?)/\* END goalDone \*/", src, re.S).group(1)
    r = subprocess.run(["node", "-e", region + "\n" + body], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


ITEMS = [
    {"id": "T01", "goal": "G01", "status": "open", "kind": "task"},
    {"id": "T02", "goal": "G01", "status": "open", "kind": "task"},
    {"id": "T03", "goal": "G01", "status": "done", "kind": "task"},
    {"id": "T04", "goal": "G02", "status": "open", "kind": "task"},
    {"id": "D01", "goal": "G01", "status": "open", "kind": "decision"},
    {"id": "A01", "goal": "G01", "status": "open", "kind": "ask"},
]


@unittest.skipUnless(shutil.which("node"), "node not installed")
class TestGoalDone(unittest.TestCase):
    def test_patch_and_standing_refusal(self):
        r = node('console.log(JSON.stringify({p: goalDonePatch("  shipped ", "owner", "2026-10-21T14:30:00.000Z"), q: goalDonePatch("", "owner", "x"),'
                 ' s: goalCanComplete({lifecycle: "active"}, "standing"), d: goalCanComplete({lifecycle: "done"}, "directed"), f: goalCanComplete({}, "factory"),'
                 ' re: goalReopenPatch("owner", "t")}))')
        self.assertEqual(r["p"], {"lifecycle": "done", "parked": None, "done_at": "2026-10-21T14:30:00.000Z", "done_by": "owner", "done_ack": None,
                                  "updated": "2026-10-21T14:30:00.000Z", "done_note": "shipped"})
        self.assertNotIn("done_note", r["q"])
        self.assertEqual((r["s"], r["d"], r["f"]), (False, False, True))   # standing and already-done goals get no control
        self.assertEqual(r["re"]["lifecycle"], "active")
        self.assertNotIn("status", r["re"])                                # reopening writes nothing to the tasks

    def test_cascade_closes_open_tasks_only_and_lists_the_rest(self):
        r = node("const items = %s; const isTask = (i) => i.kind === 'task'; const w = goalOpenWork('G01', items, isTask);"
                 "console.log(JSON.stringify({t: w.tasks.map(i => i.id), o: w.others.map(i => i.id)}))" % json.dumps(ITEMS))
        self.assertEqual(r, {"t": ["T01", "T02"], "o": ["D01", "A01"]})

    def _ops(self, versions):
        return node("const g = {id: 'G01'}; const tasks = [{id: 'T01'}, {id: 'T02'}];"
                    "console.log(JSON.stringify(goalDoneOps(g, tasks, %s, 'ok', 'owner', '2026-10-21T14:30:00.000Z')))" % json.dumps(versions))

    def test_ops_goal_first_then_tasks_each_pinned(self):
        ops = self._ops({"G01": 3, "T01": 5, "T02": 7})
        self.assertEqual([(o["collection"], o["id"], o["if_version"]) for o in ops], [("goals", "G01", 3), ("items", "T01", 5), ("items", "T02", 7)])
        self.assertEqual(ops[1]["data"], {"status": "done", "done_at": "2026-10-21T14:30:00.000Z", "done_by": "owner",
                                          "outcome": "Closed with goal G01", "updated": "2026-10-21T14:30:00.000Z"})
        self.assertEqual(ops[0]["data"]["lifecycle"], "done")

    def test_batch_applies_to_the_store_and_a_conflict_changes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            st = ds.Store(tmp)
            st.set("goals", "G01", {"title": "Ship", "lifecycle": "active", "kind": "directed"}, if_version=0)
            for t in ("T01", "T02"):
                st.set("items", t, {"status": "open", "goal": "G01"}, if_version=0)
            st.set("items", "T04", {"status": "open", "goal": "G02"}, if_version=0)
            v = lambda c, i: st.get(c, i)["version"]
            stale = self._ops({"G01": v("goals", "G01"), "T01": v("items", "T01"), "T02": v("items", "T02") + 5})
            with self.assertRaises(ds.StoreError):
                st.batch(stale)
            self.assertEqual((st.get("goals", "G01")["data"]["lifecycle"], st.get("items", "T01")["data"]["status"]), ("active", "open"))
            fresh = self._ops({"G01": v("goals", "G01"), "T01": v("items", "T01"), "T02": v("items", "T02")})
            st.batch(fresh)
            self.assertEqual(st.get("goals", "G01")["data"]["lifecycle"], "done")
            self.assertEqual([st.get("items", t)["data"]["status"] for t in ("T01", "T02", "T04")], ["done", "done", "open"])
            self.assertEqual(st.get("items", "T01")["data"]["outcome"], "Closed with goal G01")


if __name__ == "__main__":
    unittest.main()

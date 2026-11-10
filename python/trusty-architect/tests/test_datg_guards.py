"""#10800 G1-G3 (DOC-87, scope D0c): policy-file conflicts, work-card binding, delivery displacement.

G1: a file CLAUDE.md imports restates a threshold the owner changed; the resume digest flags it.
G2: a dispatch names its work card; a card closes on an outcome that cites evidence. Shadow by default.
G3: idle ready work, checker/repair work holding every running slot, and a repair loop are flagged.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402
import datg_guards  # noqa: E402

DATG = os.path.join(SCRIPTS, "datg.py")
HOOK = os.path.join(SCRIPTS, "datg-hook.sh")
TURN = {"at": "2026-11-06T20:48:30Z", "session": "tm-architect",
        "text": "Run four helpers until the Monday reset, then two helpers at most."}
OWNER = {"by": "owner", "cite": "chat 2026-11-06T20:48Z", "quote": "Run four helpers until the Monday reset"}


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.p = self._tmp.name
        datg.op_init(self.p, "th")
        datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "ship the export"},
                                {"op": "add", "kind": "G", "title": "other work"},
                                {"op": "add", "kind": "T", "title": "write the export", "goal": "th-G01"}])
        self.sp = datg.paths(self.p)[1]

    def edit_state(self, fn):
        with open(self.sp) as f:
            st = json.load(f)
        fn(st)
        with open(self.sp, "w") as f:
            json.dump(st, f)

    def enforce(self):
        self.edit_state(lambda st: st["config"].update(guard_mode="enforce"))

    def guards_log(self):
        try:
            with open(os.path.join(datg.paths(self.p)[0], "guards.jsonl")) as f:
                return [json.loads(x) for x in f]
        except FileNotFoundError:
            return []

    def cli(self, *args, stdin=None):
        return subprocess.run([sys.executable, DATG, "--project", self.p, *args], input=stdin,
                              capture_output=True, text=True)

    def hook(self, prompt):
        r = subprocess.run([HOOK], input=json.dumps({"hook_event_name": "PreToolUse", "tool_name": "Agent",
                                                     "tool_input": {"prompt": prompt}}),
                           capture_output=True, text=True, env=dict(os.environ, CLAUDE_PROJECT_DIR=self.p))
        return json.loads(r.stdout)["hookSpecificOutput"] if r.stdout.strip() else None


class CardCase(Base):
    def test_dispatch_without_a_card_warns_in_shadow_and_is_recorded_10800(self):
        out = self.hook("Goal: th-G01\nimplement the export")
        self.assertIn("names no work card", out["additionalContext"])
        self.assertNotIn("permissionDecision", out)
        self.assertEqual([r["guard"] for r in self.guards_log()], ["card"])

    def test_dispatch_with_an_open_card_under_its_goal_passes_10800(self):
        self.assertIsNone(self.hook("Goal: th-G01 Card: th-T01\nimplement the export"))
        self.assertEqual(self.guards_log(), [])

    def test_card_under_another_goal_or_unknown_is_flagged_10800(self):
        self.assertIn("under th-G01, not th-G02", self.hook("Goal: th-G02 Card: th-T01 x")["additionalContext"])
        self.assertIn("not an open task", self.hook("Goal: th-G01 Card: th-T7f x")["additionalContext"])

    def test_enforce_denies_a_dispatch_without_a_card_10800(self):
        self.enforce()
        self.assertEqual(self.hook("Goal: th-G01 go")["permissionDecision"], "deny")
        self.assertIsNone(self.hook("Goal: th-G01 Card: th-T01 go"))
        r = self.cli("check-dispatch", "--prompt", "Goal: th-G01 go")
        self.assertEqual((r.returncode, r.stdout.split(":")[0]), (2, "block"))

    def test_rules_name_the_card_and_the_evidence_10800(self):
        self.assertIn("Card: {code}-Txx", datg.RULES)
        self.assertIn("evidence", datg.RULES)


class EvidenceCase(Base):
    def test_close_without_evidence_lands_in_shadow_and_is_recorded_10800(self):
        r = self.cli("close", "th-T01", "--outcome", "done, looks good")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("closed with no evidence", r.stderr)
        self.assertEqual([(x["guard"], x["ref"]) for x in self.guards_log()], [("evidence", "th-T01")])
        with open(datg.paths(self.p)[2]) as f:
            self.assertNotIn("evidence", f.read())   # log.jsonl bytes stay those of the Rust writer

    def test_enforce_refuses_a_close_without_evidence_and_writes_nothing_10800(self):
        self.enforce()
        before = open(self.sp).read()
        r = self.cli("close", "th-T01", "--outcome", "done")
        self.assertEqual(r.returncode, 2)
        self.assertIn("no evidence", r.stderr)
        self.assertEqual(open(self.sp).read(), before)

    def test_outcomes_with_evidence_pass_10800(self):
        for outcome in ("merged in #10836", "PR https://example.com/pull/9", "pushed 89a40fb99c",
                        "wrote docs/specs/DOC-87.md", "CI run 40960263341 green"):
            self.assertEqual(datg_guards.evidence_gaps([("task", {"id": "th-T01", "outcome": outcome})]), [],
                             outcome)
        self.assertEqual(datg_guards.evidence_gaps([("item", {"id": "th-D01", "outcome": "ok"})]), [])


class PolicyCase(Base):
    def setUp(self):
        super().setUp()
        with open(os.environ["ARCHITECT_OWNER_TURNS"], "a") as f:
            f.write(json.dumps(TURN) + "\n")
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "helpers until Monday", "goal": "th-G01"},
                                {"op": "update", "id": "th-D01", "answer": "four helpers", "source": OWNER}])
        os.makedirs(os.path.join(self.p, "docs"))
        self.write("CLAUDE.md", "# Project\n\nSee @docs/policy.md for the staffing rules.\n")

    def write(self, rel, text):
        with open(os.path.join(self.p, rel), "w") as f:
            f.write(text)

    def test_imported_policy_file_that_restates_an_old_cap_is_flagged_10800(self):
        self.write("docs/policy.md", "# Staffing\nRun at most 2 helpers at a time.\n")
        dig = datg.build_digest(self.p, 3000)
        self.assertIn("FLAG policy conflict: docs/policy.md:2", dig)
        self.assertIn("owner ruling th-D01", dig)
        r = self.cli("guards")
        self.assertEqual(r.returncode, 1)
        self.assertIn("docs/policy.md:2", r.stdout)

    def test_matching_relative_fenced_or_expired_thresholds_are_not_flagged_10800(self):
        self.write("docs/policy.md", "Run 4 helpers.\nAdd two more helpers when idle.\n")
        self.write("CLAUDE.md", "@docs/policy.md\n```\n@docs/old.md\n```\n")
        self.write("docs/old.md", "Run 2 helpers.\n")
        self.assertEqual(datg_guards.policy_flags(self.p, datg.load_state(self.p)), [])
        self.write("docs/policy.md", "Run 2 helpers.\n")
        later = datetime(2026, 11, 9, 10, tzinfo=timezone.utc)
        self.edit_state(lambda st: st["items"][0]["source"].update(until="2026-11-09T09:00Z"))
        self.assertEqual(datg_guards.policy_flags(self.p, datg.load_state(self.p), later), [])
        self.assertEqual(len(datg_guards.policy_flags(self.p, datg.load_state(self.p),
                                                      later - timedelta(days=1))), 1)

    def test_nested_import_is_followed_10800(self):
        self.write("docs/policy.md", "Details: @limits.md\n")
        self.write("docs/limits.md", "Cap: 3 helpers.\n")
        flags = datg_guards.policy_flags(self.p, datg.load_state(self.p))
        self.assertEqual(len(flags), 1)
        self.assertIn("docs/limits.md:1", flags[0])


class DeliveryCase(Base):
    NOW = datetime(2026, 11, 9, 12, tzinfo=timezone.utc)

    def st(self, **ses):
        st = datg.load_state(self.p)
        st["session"].update({"written_epoch": int((self.NOW - timedelta(minutes=30)).timestamp()), **ses})
        return st

    def test_idle_ready_work_is_flagged_unless_an_agent_runs_or_a_hold_is_set_10800(self):
        flags = datg_guards.delivery_flags(self.st(), [], self.NOW)
        self.assertEqual(len(flags), 1)
        self.assertIn("idle ready work: th-T01", flags[0])
        self.assertEqual(datg_guards.delivery_flags(self.st(agents=[{"id": "a1"}]), [], self.NOW), [])
        self.assertEqual(datg_guards.delivery_flags(self.st(hold="owner hold"), [], self.NOW), [])
        fresh = self.st(written_epoch=int(self.NOW.timestamp()) - 60)
        self.assertEqual(datg_guards.delivery_flags(fresh, [], self.NOW), [])

    def test_repair_work_holding_every_running_slot_is_flagged_10800(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "fix the CI gate", "goal": "th-G01",
                                 "status": "running"}])
        flags = datg_guards.delivery_flags(self.st(), [], self.NOW)
        self.assertEqual(len(flags), 1)
        self.assertIn("repair displacing delivery: running th-T02", flags[0])
        self.assertIn("delivery cards th-T01", flags[0])
        datg.write_ops(self.p, [{"op": "update", "id": "th-T01", "status": "running"}])
        self.assertEqual(datg_guards.delivery_flags(self.st(), [], self.NOW), [])

    def test_repair_loop_without_a_delivery_close_is_flagged_10800(self):
        t = self.NOW - timedelta(hours=1)
        recs = [(t, "done", {"id": f"th-T0{n}", "note": title}) for n, title in
                ((2, "retry the flaky test"), (3, "repair the hook"), (4, "lint fixes"))]
        flags = datg_guards.delivery_flags(self.st(agents=[{"id": "a"}]), recs, self.NOW)
        self.assertEqual(len(flags), 1)
        self.assertIn("repair loop: 3", flags[0])
        recs.append((t, "done", {"id": "th-T05", "note": "ship the export page"}))
        self.assertEqual(datg_guards.delivery_flags(self.st(agents=[{"id": "a"}]), recs, self.NOW), [])

    def test_digest_carries_the_delivery_flag_10800(self):
        self.edit_state(lambda st: st["session"].update(written_epoch=int(time.time()) - 3600))
        self.assertIn("FLAG idle ready work", datg.build_digest(self.p, 3000))


if __name__ == "__main__":
    unittest.main()

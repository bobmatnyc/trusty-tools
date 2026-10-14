"""datg.py and datg-hook.sh on temp project directories.

Covers: atomic write, log append, malformed state refused and left intact, id collision,
warn vs block switch by date and by config.mode, fail open on a broken state file, hook
output size, hook JSON shape, read-only commands.
Run: python3 -m unittest scripts/test_datg.py
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import datg  # noqa: E402

HOOK = os.path.join(HERE, "datg-hook.sh")
BEFORE = datetime(2026, 10, 10, tzinfo=timezone.utc)
AFTER = datetime(2026, 10, 13, tzinfo=timezone.utc)


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.p = self._tmp.name
        datg.op_init(self.p, "th")
        datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "mobile UI", "maps": "G0c"}])
        self.sp = datg.paths(self.p)[1]
        self.lp = datg.paths(self.p)[2]

    def state(self):
        with open(self.sp) as f:
            return json.load(f)

    def log(self):
        with open(self.lp) as f:
            return [json.loads(x) for x in f]

    def hook(self, payload, env=None):
        e = dict(os.environ, CLAUDE_PROJECT_DIR=self.p)
        e.update(env or {})
        r = subprocess.run([HOOK], input=json.dumps(payload), capture_output=True, text=True, env=e)
        return r


class WriteCase(Base):
    def test_atomic_write_leaves_no_temp_and_bumps_version(self):
        v = self.state()["v"]
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "x", "goal": "th-G01"}])
        self.assertEqual(self.state()["v"], v + 1)
        self.assertEqual([f for f in os.listdir(os.path.dirname(self.sp)) if ".tmp." in f], [])

    def test_failed_replace_keeps_old_state(self):
        before = open(self.sp).read()
        with mock.patch("os.replace", side_effect=OSError("boom")):
            with self.assertRaises(OSError):
                datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "x", "goal": "th-G01"}])
        self.assertEqual(open(self.sp).read(), before)
        self.assertEqual([f for f in os.listdir(os.path.dirname(self.sp)) if ".tmp." in f], [])

    def test_log_append_one_line_per_change(self):
        n = len(self.log())
        datg.write_ops(self.p, [
            {"op": "add", "kind": "D", "title": "which db", "goal": "th-G01", "opts": datg._opts("a*,b")},
            {"op": "update", "id": "th-D01", "esc": "architect"}])
        new = self.log()[n:]
        self.assertEqual([(x["id"], x["ev"]) for x in new], [("th-D01", "open"), ("th-D01", "escalated")])

    def test_malformed_state_refused_and_untouched(self):
        with open(self.sp, "w") as f:
            f.write("{not json")
        n = len(self.log())
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "x", "goal": "th-G01"}])
        self.assertEqual(open(self.sp).read(), "{not json")
        self.assertEqual(len(self.log()), n)

    def test_id_collision_refused(self):
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "dup", "id": "th-G01"}])
        # counter skips an id that was seeded by hand
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t", "goal": "th-G01", "id": "th-T01"}])
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t2", "goal": "th-G01"}])
        ids = [t["id"] for t in self.state()["tasks"]]
        self.assertEqual(ids, ["th-T01", "th-T02"])

    def test_batch_is_all_or_nothing(self):
        before = open(self.sp).read()
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "ok", "goal": "th-G01"},
                                    {"op": "add", "kind": "T", "title": "bad", "goal": "th-G99"}])
        self.assertEqual(open(self.sp).read(), before)

    def test_done_needs_outcome_and_close_works(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t", "goal": "th-G01"}])
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "update", "id": "th-T01", "status": "done"}])
        datg.write_ops(self.p, [{"op": "close", "id": "th-T01", "outcome": "merged"}])
        self.assertEqual(self.state()["tasks"][0]["status"], "done")

    def test_read_only_commands_do_not_write(self):
        before = (open(self.sp).read(), open(self.lp).read())
        datg.cmd_show(self.p, True)
        datg.check_dispatch(datg.load_state(self.p), "x")
        self.assertEqual(before, (open(self.sp).read(), open(self.lp).read()))

    def test_cli_exit_codes(self):
        r = subprocess.run([sys.executable, os.path.join(HERE, "datg.py"), "--project", self.p,
                            "add", "T", "--title", "t", "--goal", "th-G09"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)


class DispatchCase(Base):
    def test_warn_before_date_block_after(self):
        st = datg.load_state(self.p)
        self.assertEqual(datg.check_dispatch(st, "do it", BEFORE)[0], "warn")
        self.assertEqual(datg.check_dispatch(st, "do it", AFTER)[0], "block")
        self.assertEqual(datg.check_dispatch(st, "Goal: th-G01\ndo it", AFTER)[0], "allow")

    def test_config_mode_overrides_date(self):
        st = datg.load_state(self.p)
        st["config"]["mode"] = "warn"
        self.assertEqual(datg.check_dispatch(st, "x", AFTER)[0], "warn")
        st["config"] = {"mode": "block"}
        self.assertEqual(datg.check_dispatch(st, "x", BEFORE)[0], "block")

    def test_parked_goal_counts_as_violation(self):
        datg.write_ops(self.p, [{"op": "update", "id": "th-G01", "status": "parked"}])
        datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "other"}])
        st = datg.load_state(self.p)
        self.assertEqual(datg.check_dispatch(st, "Goal: th-G01", AFTER)[0], "block")
        self.assertEqual(datg.check_dispatch(st, "Goal: th-G02", AFTER)[0], "allow")


class HookCase(Base):
    def test_pretooluse_warn_output_small_and_shaped(self):
        r = self.hook({"hook_event_name": "PreToolUse", "tool_name": "Agent",
                       "tool_input": {"prompt": "no goal here"}},
                      env={})
        self.assertEqual(r.returncode, 0)
        if datetime.now(timezone.utc) < datg.parse_iso(datg.DEFAULT_BLOCK_AFTER):
            out = json.loads(r.stdout)["hookSpecificOutput"]
            self.assertEqual(out["hookEventName"], "PreToolUse")
            self.assertIn("additionalContext", out)
        self.assertLess(len(r.stdout.encode()), 400)

    def test_pretooluse_block_uses_documented_decision(self):
        st = self.state()
        st["config"]["mode"] = "block"
        with open(self.sp, "w") as f:
            json.dump(st, f)
        r = self.hook({"hook_event_name": "PreToolUse", "tool_name": "Task",
                       "tool_input": {"prompt": "no goal"}})
        out = json.loads(r.stdout)["hookSpecificOutput"]
        self.assertEqual(out["permissionDecision"], "deny")
        self.assertLess(len(r.stdout.encode()), 400)
        ok = self.hook({"hook_event_name": "PreToolUse", "tool_name": "Task",
                        "tool_input": {"prompt": "Goal: th-G01 go"}})
        self.assertEqual(ok.stdout, "")

    def test_other_tools_and_subagents_silent(self):
        self.assertEqual(self.hook({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                                    "tool_input": {"command": "ls"}}).stdout, "")
        self.assertEqual(self.hook({"hook_event_name": "PreToolUse", "tool_name": "Agent", "agent_id": "x",
                                    "tool_input": {"prompt": "p"}}).stdout, "")

    def test_sessionstart_injects_rules_and_summary(self):
        r = self.hook({"hook_event_name": "SessionStart"})
        ctx = json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("Goal: th-Gxx", ctx)
        self.assertIn("TOP th-G01", ctx)

    def test_fail_open_on_broken_or_missing_state(self):
        for payload in ({"hook_event_name": "SessionStart"},
                        {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": {"prompt": "x"}}):
            with open(self.sp, "w") as f:
                f.write("garbage")
            r = self.hook(payload)
            self.assertEqual((r.returncode, r.stdout), (0, ""))
            os.unlink(self.sp)
            r = self.hook(payload)
            self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_fail_open_on_bad_stdin(self):
        r = subprocess.run([HOOK], input="not json", capture_output=True, text=True,
                           env=dict(os.environ, CLAUDE_PROJECT_DIR=self.p))
        self.assertEqual((r.returncode, r.stdout), (0, ""))


if __name__ == "__main__":
    unittest.main()

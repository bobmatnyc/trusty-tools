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
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402

HOOK = os.path.join(SCRIPTS, "datg-hook.sh")
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
        self.assertEqual(self.state()["tasks"], [])  # closed items move to archive.jsonl
        self.assertEqual(json.loads(datg.archive_lookup(self.p, "th-T01"))["item"]["status"], "done")

    def test_read_only_commands_do_not_write(self):
        before = (open(self.sp).read(), open(self.lp).read())
        datg.cmd_show(self.p, True)
        datg.check_dispatch(datg.load_state(self.p), "x")
        self.assertEqual(before, (open(self.sp).read(), open(self.lp).read()))

    def test_cli_exit_codes(self):
        r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", self.p,
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

    @unittest.skipUnless(sys.version_info >= (3, 11), "-P / PYTHONSAFEPATH need Python 3.11")
    def test_safe_path_mode_still_imports_the_split_modules(self):
        # #10161: under -P the script dir is not on sys.path; the hook must still deny, not fail open.
        r = subprocess.run([sys.executable, "-P", os.path.join(SCRIPTS, "datg.py"), "--help"],
                           capture_output=True, text=True, cwd=self.p)
        self.assertEqual(r.returncode, 0, r.stderr)
        st = self.state()
        st["config"]["mode"] = "block"
        with open(self.sp, "w") as f:
            json.dump(st, f)
        r = self.hook({"hook_event_name": "PreToolUse", "tool_name": "Task", "tool_input": {"prompt": "no goal"}},
                      env={"PYTHONSAFEPATH": "1"})
        self.assertEqual(json.loads(r.stdout or "{}").get("hookSpecificOutput", {}).get("permissionDecision"), "deny")


class ProjectArgCase(Base):
    """datg-hook.sh <session> <project>: arg 2 selects the ledger dir (the Architect's IL is local/il)."""

    def test_project_arg_overrides_session_cwd(self):
        sub = os.path.join(self.p, "il")
        bare = os.path.join(self.p, "bare")
        os.makedirs(sub)
        os.makedirs(bare)
        subprocess.run(["python3", os.path.join(SCRIPTS, "datg.py"), "--project", sub, "init", "--code", "ar"],
                       check=True, capture_output=True, text=True)
        e = {k: v for k, v in os.environ.items() if k not in ("CLAUDE_PROJECT_DIR", "DATG_SESSION", "TMUX_PANE")}
        payload = json.dumps({"hook_event_name": "SessionStart", "source": "startup", "cwd": bare})
        none = subprocess.run([HOOK, "", bare], input=payload, capture_output=True, text=True, env=e, timeout=60)
        self.assertEqual(none.stdout, "")  # bare has no ledger of its own
        got = subprocess.run([HOOK, "", sub], input=payload, capture_output=True, text=True, env=e, timeout=60)
        self.assertIn("additionalContext", got.stdout)


class SessionFilterCase(Base):
    """datg-hook.sh <session>: output only when the TMUX_PANE session is exactly that name."""
    EVENTS = ({"hook_event_name": "SessionStart"},
              {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": {"prompt": "no goal"}})

    def run_hook(self, payload, session, arg="tm-dogfood", env_name=None):
        bindir = os.path.join(self.p, ".stubbin")
        os.makedirs(bindir, exist_ok=True)
        stub = os.path.join(bindir, "tmux")
        if not os.path.exists(stub):  # write once: each fresh executable costs a macOS first-exec scan
            with open(stub, "w") as f:
                f.write('#!/bin/sh\n[ -n "$STUB_SESSION" ] && echo "$STUB_SESSION"\nexit 0\n')
            os.chmod(stub, 0o755)
        e = dict(os.environ, CLAUDE_PROJECT_DIR=self.p, PATH=bindir + ":" + os.environ["PATH"])
        e.pop("TMUX_PANE", None)
        e.pop("DATG_SESSION", None)
        if session is not None:
            e.update(TMUX_PANE="%9", STUB_SESSION=session)
        if env_name:
            e["DATG_SESSION"] = env_name
        cmd = [HOOK] + ([arg] if arg else [])
        return subprocess.run(cmd, input=json.dumps(payload), capture_output=True, text=True, env=e, timeout=60)

    def test_only_named_session_gets_output(self):
        for ev in self.EVENTS:
            on = self.run_hook(ev, "tm-dogfood")
            self.assertEqual(on.returncode, 0)
            self.assertNotEqual(on.stdout, "", ev)
            for sess in ("tm-secrets-01", "tm-dogfood-02", "tm-dogfood ", ""):
                off = self.run_hook(ev, sess)
                self.assertEqual((off.returncode, off.stdout, off.stderr), (0, "", ""), (ev, sess))
            off = self.run_hook(ev, None)  # no tmux at all
            self.assertEqual((off.returncode, off.stdout, off.stderr), (0, "", ""), ev)

    def test_env_name_works_and_no_name_means_unfiltered(self):
        ev = self.EVENTS[0]
        self.assertNotEqual(self.run_hook(ev, "tm-dogfood", arg=None, env_name="tm-dogfood").stdout, "")
        self.assertEqual(self.run_hook(ev, "tm-other", arg=None, env_name="tm-dogfood").stdout, "")
        self.assertNotEqual(self.run_hook(ev, "tm-other", arg=None).stdout, "")  # things-01 style

    def test_agent_id_skipped_even_in_named_session(self):
        for ev in self.EVENTS:
            self.assertEqual(self.run_hook(dict(ev, agent_id="a1"), "tm-dogfood").stdout, "")


class EscalationNotifyCase(Base):
    """update --esc / batch esc also queue a `decision` through the D10 intake (pm-notify)."""

    def setUp(self):
        super().setUp()
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "which db", "goal": "th-G01"}])
        self.rec = os.path.join(self.p, "notify-args.txt")
        self.stub = os.path.join(self.p, "stub-notify.sh")

    def stub_cmd(self, body):
        with open(self.stub, "w") as f:
            f.write("#!/bin/sh\n" + body)
        os.chmod(self.stub, 0o755)
        return dict(os.environ, DATG_NOTIFY_CMD=self.stub, DATG_SESSION="tm-pilot")

    def run_datg(self, args, env, stdin=None):
        return subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", self.p] + args,
                              input=stdin, capture_output=True, text=True, env=env)

    def test_esc_update_queues_decision_with_id_and_title(self):
        env = self.stub_cmd('printf "%s\\n" "$@" > "' + self.rec + '"\necho queued\n')
        r = self.run_datg(["update", "th-D01", "--esc", "architect"], env)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        args = open(self.rec).read().split("\n")
        self.assertEqual(args[:4], ["--kind", "decision", "--session", "tm-pilot"])
        self.assertIn("th-D01", args[4])
        self.assertIn("which db", args[4])
        self.assertEqual(self.state()["items"][0]["esc"], "architect")

    def test_batch_esc_also_notifies_and_plain_update_does_not(self):
        env = self.stub_cmd('echo called >> "%s"\n' % self.rec)
        self.run_datg(["update", "th-D01", "--title", "which database"], env)
        self.assertFalse(os.path.exists(self.rec))
        self.run_datg(["update", "th-D01", "--esc", ""], env)
        self.assertFalse(os.path.exists(self.rec))
        r = self.run_datg(["batch"], env, stdin=json.dumps([{"op": "update", "id": "th-D01", "esc": "architect"}]))
        self.assertEqual(r.returncode, 0)
        self.assertEqual(open(self.rec).read(), "called\n")

    def test_notify_failure_does_not_fail_update_and_is_logged(self):
        for body in ("echo boom >&2\nexit 3\n", None):
            env = self.stub_cmd(body or "")
            if body is None:
                env["DATG_NOTIFY_CMD"] = os.path.join(self.p, "no-such-notify")
            r = self.run_datg(["update", "th-D01", "--esc", "architect"], env)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("pm-notify", r.stderr)
            self.assertEqual(self.state()["items"][0]["esc"], "architect")
            fails = [x for x in self.log() if x.get("kind") == "notify_failure"]
            self.assertEqual(fails[-1]["session"], "tm-pilot")
            self.assertIn("th-D01", fails[-1]["text"])
            self.datg_reset_esc()

    def datg_reset_esc(self):
        datg.write_ops(self.p, [{"op": "update", "id": "th-D01", "esc": ""}])

    def test_real_pm_notify_queues_a_wake_decision(self):
        inbox = os.path.join(self.p, "inbox")
        env = dict(os.environ, SUPERVISOR_INBOX_DIR=inbox, DATG_SESSION="tm-pilot")
        env.pop("DATG_NOTIFY_CMD", None)
        r = self.run_datg(["update", "th-D01", "--esc", "architect"], env)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        lines = [json.loads(x) for x in open(os.path.join(inbox, "pm-messages.jsonl"))]
        self.assertEqual([(m["kind"], m["session"]) for m in lines], [("decision", "tm-pilot")])
        self.assertIn("th-D01", lines[0]["text"])


class HookWarningLogCase(Base):
    """warn-mode hook warnings are appended to log.jsonl so the pilot can count them."""

    def set_mode(self, mode):
        st = self.state()
        st["config"]["mode"] = mode
        with open(self.sp, "w") as f:
            json.dump(st, f)

    PAYLOAD = {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": {"prompt": "no goal"}}

    def test_warn_is_logged_with_session_and_text(self):
        self.set_mode("warn")
        n = len(self.log())
        r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "hook"], input=json.dumps(self.PAYLOAD),
                           capture_output=True, text=True,
                           env=dict(os.environ, CLAUDE_PROJECT_DIR=self.p, DATG_SESSION="tm-pilot"))
        self.assertEqual(r.returncode, 0)
        new = self.log()[n:]
        self.assertEqual(len(new), 1)
        rec = new[0]
        self.assertEqual((rec["kind"], rec["session"]), ("hook_warning", "tm-pilot"))
        self.assertEqual(sorted(rec), ["kind", "session", "text", "ts"])
        self.assertIn("names no goal id", rec["text"])
        self.assertEqual(self.state()["v"], 2)  # the hook still writes no state

    def test_no_log_for_allow_block_and_session_start(self):
        self.set_mode("warn")
        n = len(self.log())
        self.hook({"hook_event_name": "SessionStart"})
        self.hook(dict(self.PAYLOAD, tool_input={"prompt": "Goal: th-G01 go"}))
        self.set_mode("block")
        self.hook(self.PAYLOAD)
        self.assertEqual(self.log()[n:], [])

    def test_log_failure_still_prints_the_warning(self):
        self.set_mode("warn")
        os.unlink(self.lp)
        os.mkdir(self.lp)  # appending to a directory fails
        r = self.hook(self.PAYLOAD)
        self.assertEqual(r.returncode, 0)
        self.assertIn("warn mode", json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"])

    def test_session_filter_script_exports_the_matched_session(self):
        self.set_mode("warn")
        n = len(self.log())
        SessionFilterCase.run_hook(self, self.PAYLOAD, "tm-dogfood")
        self.assertEqual(self.log()[n:][0]["session"], "tm-dogfood")


def run_cli(project, args, stdin=None, env=None, session=None):
    cmd = [sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", project] + (["--session", session] if session else [])
    return subprocess.run(cmd + args, input=stdin, capture_output=True, text=True,
                          env=dict(os.environ, **(env or {})))


class SessionRecordCase(Base):
    """B1: session record, limits, batch."""

    def test_session_set_cli_roundtrip_and_stamps(self):
        r = run_cli(self.p, ["session", "set", "--task", "th-T01 ship it", "--next", "merge PR 5",
                             "--wait", "owner:ruling on D01", "--owe", "main sha after merge=abc1234@https://x/pull/5",
                             "--agent", "a1:th-G01:review", "--cron", "c1=*/5 * * * *=check ci", "--boundary",
                             "--ctx-use", "40:60:150000:the PR diff"])
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        ses = self.state()["session"]
        self.assertEqual(ses["owed"], [{"check": "main sha after merge", "expect": "abc1234", "ref": "https://x/pull/5"}])
        self.assertEqual(ses["crons"][0]["schedule"], "*/5 * * * *")
        self.assertEqual(ses["waiting_on"][0]["who"], "owner")
        self.assertEqual((ses["ctx_use"]["stale"], ses["ctx_use"]["ctx"]), (60, 150000))
        self.assertTrue(ses["written_at"] and ses["boundary_at"] and ses["owed_none"] is False)

    def test_over_limit_field_refused_files_untouched(self):
        before = (open(self.sp).read(), open(self.lp).read())
        for args in (["--task", "x" * 161], ["--next", "y" * 201], ["--owe", "c=" + "z" * 81 + "@r"],
                     ["--wait", "nobody:what"], ["--ctx-use", "101:0:1:k"], ["--owe", "main sha=latest@r"],
                     ["--cron", "a=b=" + "p" * 201]):
            r = run_cli(self.p, ["session", "set"] + args)
            self.assertEqual(r.returncode, 2, args)
        self.assertEqual(before, (open(self.sp).read(), open(self.lp).read()))

    def test_session_record_over_2kb_refused(self):
        ops = [{"op": "session", "waiting_on": [{"who": "ci", "what": "w" * 120}] * 5,
                "owed": [{"check": "c" * 80, "expect": "e" * 80, "ref": "r" * 80}] * 8}]
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, ops)

    def test_session_plus_item_op_is_one_commit_and_none_states_empty(self):
        v = self.state()["v"]
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t", "goal": "th-G01"},
                                {"op": "session", "task": "th-T01", "owed": [], "crons": [], "agents": []}])
        st = self.state()
        self.assertEqual(st["v"], v + 1)
        self.assertEqual((st["session"]["owed"], st["session"]["owed_none"], st["session"]["crons"]), ([], True, []))


class ArchiveCase(Base):
    """B2: close/prune/archive."""

    def add_items(self, n=3):
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": f"q{i}", "goal": "th-G01"} for i in range(n)])

    def test_close_moves_item_to_archive_and_archive_show_finds_it(self):
        self.add_items()
        datg.write_ops(self.p, [{"op": "close", "id": "th-D02", "outcome": "done and verified"}])
        self.assertEqual([i["id"] for i in self.state()["items"]], ["th-D01", "th-D03"])
        r = run_cli(self.p, ["archive-show", "th-D02"])
        rec = json.loads(r.stdout)
        self.assertEqual((r.returncode, rec["item"]["outcome"], rec["kind"]), (0, "done and verified", "item"))
        self.assertEqual(run_cli(self.p, ["archive-show", "th-D01"]).returncode, 2)

    def test_crash_between_archive_and_state_leaves_item_in_exactly_one_file(self):
        self.add_items()
        with mock.patch("os.replace", side_effect=OSError("crash")):
            with self.assertRaises(OSError):
                datg.write_ops(self.p, [{"op": "close", "id": "th-D01", "outcome": "ok"}])
        self.assertIn("th-D01", [i["id"] for i in self.state()["items"]])
        with self.assertRaises(datg.DatgError):  # orphan archive line is invisible to readers
            datg.archive_lookup(self.p, "th-D01")
        datg.write_ops(self.p, [{"op": "close", "id": "th-D02", "outcome": "ok"}])  # reconcile drops the orphan
        arch = open(os.path.join(os.path.dirname(self.sp), "archive.jsonl")).read()
        self.assertNotIn("th-D01", arch)
        self.assertIn("th-D01", [i["id"] for i in self.state()["items"]])
        datg.write_ops(self.p, [{"op": "close", "id": "th-D01", "outcome": "ok"}])
        self.assertEqual(open(os.path.join(os.path.dirname(self.sp), "archive.jsonl")).read().count('"th-D01"'), 1)
        self.assertNotIn("th-D01", [i["id"] for i in self.state()["items"]])

    def test_prune_migrates_done_and_old_answered_keeps_open_and_recent(self):
        self.add_items(4)
        st = self.state()
        st["items"][0].update(status="done", outcome="x")
        st["items"][1].update(status="answered", answer="a")      # no log entry: old
        st["items"][2].update(status="answered", answer="a")      # answered just now
        with open(self.sp, "w") as f:
            json.dump(st, f)
        with open(self.lp, "a") as f:
            f.write(json.dumps({"t": datg.now_iso(), "id": "th-D03", "ev": "answered"}) + "\n")
        dry = run_cli(self.p, ["prune", "--dry-run"])
        self.assertIn("would archive 2", dry.stdout)
        self.assertEqual(len(self.state()["items"]), 4)
        r = run_cli(self.p, ["prune"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([i["id"] for i in self.state()["items"]], ["th-D03", "th-D04"])
        self.assertEqual(run_cli(self.p, ["prune"]).stdout.strip(), "nothing to prune")

    def test_title_limit_is_120(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t" * 120, "goal": "th-G01"}])
        with self.assertRaises(datg.DatgError):
            datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": "t" * 121, "goal": "th-G01"}])

    def test_live_shaped_state_prunes_under_8kb(self):
        goals = [{"id": "th-G01", "rank": 1, "title": "g", "finish": "", "status": "active", "maps": ""}]
        items = [{"id": f"th-D{i:02x}", "kind": "question", "status": "done" if i % 2 else "answered", "goal": "th-G01",
                  "title": "t" * 110, "opts": [{"l": "o" * 150, "rec": True}] * 3, "answer": "a" * 280,
                  "outcome": "o" * 280, "esc": None} for i in range(1, 80)]
        st = dict(self.state(), items=items, goals=goals)
        with open(self.sp, "w") as f:
            json.dump(st, f)
        self.assertGreater(os.path.getsize(self.sp), 60000)
        out = datg.op_prune(self.p)
        self.assertIn("archived 79", out)
        self.assertLess(os.path.getsize(self.sp), 8192)


class SizeAndRotateCase(Base):
    """B3: size bar and log rotation."""

    def test_write_over_16kb_is_refused_with_largest_items_named(self):
        before = open(self.sp).read()
        ops = [{"op": "add", "kind": "D", "title": "q", "goal": "th-G01",
                "opts": [{"l": "o" * 160, "rec": False}] * 4} for _ in range(120)]
        with self.assertRaises(datg.DatgError) as cm:
            datg.write_ops(self.p, ops)
        self.assertIn("largest", str(cm.exception))
        self.assertEqual(open(self.sp).read(), before)
        self.assertEqual(run_cli(self.p, ["batch"], stdin=json.dumps(ops)).returncode, 2)

    def test_crossing_8kb_logs_size_warning_once(self):
        # #10571: the bars are caps (default 32 KB warn / 64 KB refuse, D74); land between them, whatever they are
        n = (datg.SIZE_WARN + datg.SIZE_REFUSE) // 2 // 880
        ops = [{"op": "add", "kind": "D", "title": "q", "goal": "th-G01", "opts": [{"l": "o" * 160, "rec": False}] * 4}
               for _ in range(n)]
        datg.write_ops(self.p, ops)
        self.assertGreater(os.path.getsize(self.sp), datg.SIZE_WARN)
        datg.write_ops(self.p, [{"op": "update", "id": "th-D01", "title": "again"}])
        self.assertEqual(len([x for x in self.log() if x.get("kind") == "size_warning"]), 1)

    def test_rotate_moves_old_lines_of_both_shapes(self):
        now = datetime(2026, 10, 17, 12, 0, tzinfo=timezone.utc)
        old = [{"t": "2026-10-01T00:00Z", "id": "th-G01", "ev": "open"}, {"ts": "2026-10-02T00:00Z", "kind": "hook_warning"}]
        with open(self.lp, "w") as f:
            for r in old + [{"t": "2026-10-16T00:00Z", "id": "x", "ev": "open"}, {"ts": "2026-10-17T00:00Z", "kind": "digest"}]:
                f.write(json.dumps(r) + "\n")
            f.write("not json\n")
        self.assertIn("rotated 2", datg.op_rotate(self.p, now=now))
        d = os.path.dirname(self.lp)
        self.assertEqual([json.loads(x) for x in open(os.path.join(d, "log-20261017.jsonl"))], old)
        self.assertEqual(len(open(self.lp).read().splitlines()), 3)
        self.assertIn("nothing to rotate", datg.op_rotate(self.p, now=now))
        self.assertEqual(sorted(k for _, k, _ in datg.read_log(self.p)), ["digest", "open"])


class DigestFreshCase(Base):
    """B4: digest and fresh."""

    def fill(self, n_tasks):
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": f"task {i} " + "w" * 100, "goal": "th-G01"}
                                for i in range(n_tasks)] + [
            {"op": "add", "kind": "D", "title": "which db see local/briefs/2026-10-17-x.md", "goal": "th-G01",
             "opts": datg._opts("pg*,mysql")},
            {"op": "session", "task": "th-T01 go", "next": "merge", "owed": [{"check": "sha", "expect": "abc", "ref": "u"}]}])

    def test_digest_has_sections_and_logs_a_digest_event(self):
        self.fill(2)
        r = run_cli(self.p, ["digest"])
        self.assertEqual(r.returncode, 0, r.stderr)
        for want in ("IL DIGEST", "task: th-T01 go", "owed: sha = abc @ u", "th-G01 r1 mobile UI", "rec: pg", "th-T02",
                     "brief: local/briefs/2026-10-17-x.md", "archive:"):
            self.assertIn(want, r.stdout)
        self.assertNotIn("TRUNCATED", r.stdout)
        self.assertEqual(self.log()[-1]["kind"], "digest")
        n = len(self.log())
        run_cli(self.p, ["digest", "--no-log"])
        self.assertEqual(len(self.log()), n)

    def test_refs_set_replace_clear_and_limits(self):
        def refs():
            return self.state()["session"].get("refs")
        r = run_cli(self.p, ["session", "set", "--task", "th-T01 x", "--ref", "doc=https://docs/x", "--ref", "marker=[A 1Z]"])
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertEqual(refs(), [{"key": "doc", "value": "https://docs/x"}, {"key": "marker", "value": "[A 1Z]"}])
        run_cli(self.p, ["session", "set", "--ref", "doc=https://docs/y"])
        self.assertEqual(refs()[0], {"key": "doc", "value": "https://docs/y"})
        self.assertEqual(len(refs()), 2)
        before = open(self.sp).read()
        for bad in ("Bad Key=v", "k=" + "v" * 201, "nokey", "k="):
            self.assertEqual(run_cli(self.p, ["session", "set", "--ref", bad]).returncode, 2, bad)
        many = [x for i in range(13) for x in ("--ref", f"k{i}=v")]
        self.assertEqual(run_cli(self.p, ["session", "set"] + many).returncode, 2)
        self.assertEqual(before, open(self.sp).read())
        run_cli(self.p, ["session", "set", "--ref", "none"])
        self.assertEqual(refs(), [])

    def test_ref_key_none_removes_only_that_key(self):
        def refs():
            return self.state()["session"].get("refs")
        run_cli(self.p, ["session", "set", "--task", "th-T01 x", "--ref", "doc=u1", "--ref", "marker=m1"])
        r = run_cli(self.p, ["session", "set", "--ref", "doc=none"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(refs(), [{"key": "marker", "value": "m1"}])
        r = run_cli(self.p, ["session", "set", "--ref", "absent=none"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(refs(), [{"key": "marker", "value": "m1"}])

    def test_digest_prints_refs_first_and_keeps_them_past_the_pointer_cut(self):
        run_cli(self.p, ["session", "set", "--task", "th-T01 x", "--next", "n", "--owe", "none", "--ref", "doc=https://docs/x"])
        full = datg.build_digest(self.p)
        lines = full.splitlines()
        self.assertEqual(lines[lines.index("POINTERS") + 1], "  ref doc: https://docs/x")
        self.assertIn("  archive:", full)
        datg.write_ops(self.p, [{"op": "add", "kind": "T", "title": f"task {i} " + "w" * 100, "goal": "th-G01"}
                                for i in range(12)])
        full = datg.build_digest(self.p)
        # budget that fits the digest with no tasks and no archive/brief pointers, but not the archive line
        keep = [x for x in full.splitlines() if not x.startswith(("  th-T0", "  th-T1", "  archive:", "TASKS", "TRUNCATED"))]
        cut = datg.build_digest(self.p, max_tokens=(len("\n".join(keep)) + 60 + 20) // 4 + 1)
        self.assertIn("ref doc: https://docs/x", cut)
        self.assertNotIn("  archive:", cut)

    def test_over_budget_drops_log_then_oldest_tasks_and_ends_with_truncated(self):
        self.fill(12)
        full = datg.build_digest(self.p)
        self.assertIn("RECENT", full)
        out = datg.build_digest(self.p, max_tokens=len(full) // 4 - 100)
        self.assertNotIn("RECENT", out)
        self.assertLessEqual(len(out) / 4, len(full) // 4 - 100)
        self.assertRegex(out.splitlines()[-1], r"^TRUNCATED: \d+ bytes dropped$")
        self.assertNotIn("task 0 ", out)
        self.assertIn("task 11 ", out)
        tiny = datg.build_digest(self.p, max_tokens=100)
        self.assertLessEqual(len(tiny) / 4, 100)
        self.assertTrue(tiny.splitlines()[-1].startswith("TRUNCATED"))

    def test_fresh_exit_codes(self):
        self.assertEqual(run_cli(self.p, ["fresh"]).returncode, 0)
        self.assertEqual(run_cli(self.p, ["fresh", "--since", "2999-01-01T00:00Z"]).returncode, 1)
        old = self.state()
        old["session"]["written_epoch"] = 1000
        old["session"]["written_at"] = "2020-01-01T00:00Z"
        with open(self.sp, "w") as f:
            json.dump(old, f)
        self.assertEqual(run_cli(self.p, ["fresh", "--max-age", "15"]).returncode, 1)
        with open(self.sp, "w") as f:
            f.write("{broken")
        self.assertEqual(run_cli(self.p, ["fresh"]).returncode, 2)


def transcript(path, tools, user_text="go"):
    """A minimal transcript: one user turn, then an assistant turn with the given (name, input) tool uses."""
    rows = [{"type": "user", "message": {"role": "user", "content": "earlier"}},
            {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Edit", "input": {}}]}},
            {"type": "user", "message": {"role": "user", "content": user_text}}]
    rows += [{"type": "assistant", "message": {"content": [{"type": "tool_use", "name": n, "input": i}]}} for n, i in tools]
    with open(path, "w") as f:
        f.write("".join(json.dumps(r) + "\n" for r in rows))


class StopHookCase(Base):
    """B5: Stop stamp, SessionStart digest, per-PM directory."""

    def stop(self, tools, env=None):
        tp = os.path.join(self.p, "t.jsonl")
        transcript(tp, tools)
        return self.hook({"hook_event_name": "Stop", "transcript_path": tp}, env=env)

    def test_stale_turn_logs_stale_il_and_makes_fresh_fail_until_a_write(self):
        r = self.stop([("Bash", {"command": "git commit -m x"})])
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        rec = [x for x in self.log() if x.get("kind") == "stale_il"]
        self.assertEqual((len(rec), rec[0]["n"]), (1, 1))
        self.assertEqual(run_cli(self.p, ["fresh"]).returncode, 1)
        self.stop([("Write", {})])
        self.assertEqual([x["n"] for x in self.log() if x.get("kind") == "stale_il"], [1, 2])
        datg.write_ops(self.p, [{"op": "session", "task": "t"}])
        self.assertEqual(run_cli(self.p, ["fresh"]).returncode, 0)
        n = len(self.log())
        self.stop([("Edit", {}), ("Bash", {"command": "python3 scripts/datg.py batch"})])
        self.assertEqual(len([x for x in self.log()[n:] if x.get("kind") == "stale_il"]), 0)

    def test_read_only_turn_and_bad_transcript_are_silent_and_fail_open(self):
        self.stop([("Read", {}), ("Bash", {"command": "ls; git status"})])
        r = self.hook({"hook_event_name": "Stop", "transcript_path": "/nonexistent"})
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        self.assertEqual([x for x in self.log() if x.get("kind") == "stale_il"], [])
        os.unlink(self.sp)
        self.assertEqual(self.stop([("Write", {})]).returncode, 0)

    def test_sessionstart_carries_digest(self):
        datg.write_ops(self.p, [{"op": "session", "task": "th-T09 resume me", "next": "do x"}])
        ctx = json.loads(self.hook({"hook_event_name": "SessionStart"}).stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("IL DIGEST", ctx)
        self.assertIn("next: do x", ctx)

    def test_clear_and_compact_carry_the_digest(self):
        # #10109: a /clear or compaction resumes from the ledger with no typed prompt.
        datg.write_ops(self.p, [{"op": "session", "task": "resume me", "next": "do x"}])
        for src in ("clear", "compact"):
            r = self.hook({"hook_event_name": "SessionStart", "source": src})
            self.assertEqual((r.returncode, r.stderr), (0, ""), src)
            ctx = json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]
            self.assertIn("IL DIGEST", ctx)
            self.assertIn("next: do x", ctx)
            self.assertNotIn("FULL DIGEST:", ctx)  # nothing was cut

    def test_large_digest_stays_under_the_hook_limit(self):
        # #10109: Claude Code caps additionalContext at 10,000 chars; a cut digest points at the full one.
        datg.write_ops(self.p, [{"op": "session", "task": "big", "next": "keep going"}])
        for kind, n in (("D", 40), ("T", 30)):  # well under the 64 KB state bar
            datg.write_ops(self.p, [{"op": "add", "kind": kind, "title": f"{kind} {i:02d} " + "w" * 105,
                                     "goal": "th-G01"} for i in range(n)])
        self.assertGreater(len(datg.RULES) + len(datg.build_digest(self.p)), 10000)  # uncut, over the limit
        r = self.hook({"hook_event_name": "SessionStart", "source": "clear"})
        ctx = json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertLess(len(ctx), 10000)
        self.assertIn("next: keep going", ctx)
        self.assertTrue(ctx.endswith(os.path.join(SCRIPTS, "datg.py") + " digest"), ctx[-200:])

    def test_no_ledger_is_silent_and_an_unreadable_one_is_reported(self):
        # #10109: no ledger -> no output, no error; an unreadable ledger fails open but says so on stderr.
        os.unlink(self.sp)
        r = self.hook({"hook_event_name": "SessionStart", "source": "clear"})
        self.assertEqual((r.returncode, r.stdout, r.stderr), (0, "", ""))
        with open(self.sp, "w") as f:
            f.write("garbage")
        r = self.hook({"hook_event_name": "SessionStart", "source": "clear"})
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        self.assertIn("datg hook: failed open", r.stderr)

    def test_per_pm_directory_and_legacy_coexist(self):
        # legacy layout untouched; a --session init creates datg/<session>/ and hooks select it via DATG_SESSION
        r = run_cli(self.p, ["init", "--code", "pm"], session="tm-pm-a")
        self.assertEqual(r.returncode, 0, r.stderr)
        base = os.path.join(self.p, ".trusty-mpm", "datg")
        self.assertTrue(os.path.isfile(os.path.join(base, "tm-pm-a", "state.json")))
        legacy_v = self.state()["v"]
        run_cli(self.p, ["add", "G", "--title", "per pm goal"], session="tm-pm-a")
        self.assertEqual(self.state()["v"], legacy_v)
        self.assertEqual(json.load(open(os.path.join(base, "tm-pm-a", "state.json")))["goals"][0]["title"], "per pm goal")
        ev = {"hook_event_name": "SessionStart"}
        out = SessionFilterCase.run_hook(self, ev, "tm-pm-a", arg="tm-pm-a").stdout
        self.assertIn("pm-G01", out)
        self.assertIn("--session tm-pm-a", out)
        out2 = SessionFilterCase.run_hook(self, ev, "tm-other", arg="tm-other").stdout
        self.assertIn("TOP th-G01", out2)  # no per-PM dir for that name: the legacy layout answers
        self.assertEqual(run_cli(self.p, ["init", "--code", "bad"], session="../x").returncode, 2)


class NextIdCase(unittest.TestCase):
    """_next_id wraps past ff and recycles free low ids."""

    @staticmethod
    def st(used, nxt):
        return {"code": "th", "goals": [], "items": [],
                "tasks": [{"id": f"th-T{i:02x}"} for i in used], "next": {"T": nxt}}

    def test_wraps_to_free_low_id_and_pointer_follows(self):
        st = self.st(range(0x10, 0x100), "ff")
        self.assertEqual(datg._next_id(st, "T"), "th-T01")
        self.assertEqual(st["next"]["T"], "02")

    def test_scans_up_before_wrapping(self):
        st = self.st([0xfe], "fe")
        self.assertEqual(datg._next_id(st, "T"), "th-Tff")
        self.assertEqual(st["next"]["T"], "01")  # ff wraps to 01

    def test_error_only_when_all_255_used(self):
        st = self.st(range(1, 256), "80")
        with self.assertRaises(datg.DatgError):
            datg._next_id(st, "T")
        st = self.st([i for i in range(1, 256) if i != 0x05], "ff")
        self.assertEqual(datg._next_id(st, "T"), "th-T05")


if __name__ == "__main__":
    unittest.main()

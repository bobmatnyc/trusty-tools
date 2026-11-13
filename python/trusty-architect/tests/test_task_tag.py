"""task_tag.py grammar, the relay check, relay.sh's refusal path and pm_intake's tag stripping (ar-Tf5, D09).

Run: python3 -m pytest tests/test_task_tag.py
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
# #11101: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import pm_intake  # noqa: E402
import task_tag  # noqa: E402

# #11101: relay.sh's task-assign refusal path is not shipped (the shipped relay.sh gates on the
# Architect marker and owner provenance instead), so its three relay.sh tests are not ported.
GOOD = "Task: ar-T0b@1110 | done: wired the hook | est: 2h | remaining: tests | pct: 40"


class ParseCase(unittest.TestCase):
    def test_strict_tag_on_last_line_parses_all_fields(self):
        kind, tag = task_tag.parse_pulse("Shipped the hook.\n" + GOOD + "\n\n")
        self.assertEqual(kind, "ok")
        self.assertEqual((tag["id"], tag["stamp"], tag["done"], tag["est"], tag["remaining"], tag["pct"]),
                         ("ar-T0b", "1110", "wired the hook", "2h", "tests", 40))

    def test_ids_of_any_session_prefix_and_dashboard_ids(self):
        for line, want in (("Task: ms-T12@0102 | pct: 5", "ms-T12"), ("Task: T93 | done: x", "T93"), ("Task: df-T0a@1109", "df-T0a")):
            self.assertEqual(task_tag.parse_pulse(line)[1]["id"], want)

    def test_pct_percent_sign_and_clamp(self):
        self.assertEqual(task_tag.parse_pulse("Task: T93 | pct: 40%")[1]["pct"], 40)
        self.assertEqual(task_tag.parse_pulse("Task: T93 | pct: 140")[1]["pct"], 100)

    def test_malformed_variants_are_rejected_not_dropped(self):
        for text in ("Task: ar-T0b | done: x",                      # IL id without @MMDD
                     "Task: ar-T0b@1110 done: x",                   # dropped pipe
                     "Task: ar-T0b@1110 | pct: 40 | done: x",       # wrong key order
                     'Task: ar-T0b@1110 | done: "x" | pct: abc',    # bad pct
                     "Task: new:fire-drill | done: x",              # hand-backs never create tasks
                     "Task: ar-T0b@1110 | done: x\nand then more"): # not the last line
            self.assertEqual(task_tag.parse_pulse(text)[0], "reject", text)

    def test_text_without_a_tag_is_none(self):
        self.assertEqual(task_tag.parse_pulse("Done. Shipped it.")[0], "none")
        self.assertEqual(task_tag.parse_pulse("")[0], "none")

    def test_loose_token_in_prompts(self):
        self.assertTrue(task_tag.has_task("Goal: ar-G01\nTask: ar-T0b@1110\ndo x"))
        self.assertTrue(task_tag.has_task("Task: T93"))
        for t in ("Task: <id>", "subtask: T93", "Task: new:abc", "Task: ar-T0b@11", "no tag"):
            self.assertFalse(task_tag.has_task(t), t)


class RelayCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.env = {**os.environ, "ARCHITECT_INBOX_DIR": self.tmp.name}
        self.env.pop("TASK_TAG_MODE", None)
        p = mock.patch.dict(os.environ, {"ARCHITECT_INBOX_DIR": self.tmp.name})
        p.start()
        self.addCleanup(p.stop)
        os.environ.pop("TASK_TAG_MODE", None)

    def test_warn_is_default_and_logs_a_miss(self):
        code, msg = task_tag.relay_check("do the thing", kind="task-assign", session="tm-x")
        self.assertEqual((code, msg[:4]), (0, "WARN"))
        rec = json.loads(open(os.path.join(self.tmp.name, "task-compliance.jsonl")).readline())
        self.assertEqual((rec["kind"], rec["who"]), ("relay", "tm-x"))

    def test_refuse_mode_refuses_only_untagged_task_assign(self):
        r = lambda text, **kw: task_tag.relay_check(text, mode_override="refuse", **kw)[0]
        self.assertEqual(r("do it", kind="task-assign"), 1)
        self.assertEqual(r("do it Task: T93", kind="task-assign"), 0)
        self.assertEqual(r("do it", kind="task-assign", no_task=True), 0)
        self.assertEqual(r("do it", kind="task-assign", urgent=True), 0)
        self.assertEqual(r("URGENT prod down", kind="task-assign"), 0)
        self.assertEqual(r("[the owner via Codex 22:50Z] do it", kind="task-assign"), 0)
        self.assertEqual(r("ack", kind=""), 0)                       # rulings, acks, pointers: not checked

    def test_pulse_tag_text_does_not_trip_the_ruling_test(self):
        msg = {"kind": "progress", "text": "working\nTask: ar-T0b@1110 | done: chose which path | remaining: approve? no"}
        self.assertEqual(pm_intake.classify(msg)[0], "hold")
        self.assertEqual(pm_intake.classify({"kind": "progress", "text": "which path?"})[0], "wake")

    def test_task_line_with_an_ask_is_kept_and_wakes(self):
        text = "Task: ar-T0b@1110 can you approve deploying to prod?"
        self.assertIn("approve", task_tag.strip_tags(text))
        self.assertEqual(pm_intake.classify({"kind": "progress", "text": text})[0], "wake")
        tagged_ask = "status\nTask: T93 which branch should I merge?"
        self.assertEqual(pm_intake.classify({"kind": "progress", "text": tagged_ask})[0], "wake")

    def test_only_pure_tag_lines_are_stripped(self):
        self.assertEqual(task_tag.strip_tags("work\n" + GOOD), "work")
        self.assertEqual(task_tag.strip_tags("Task: T93"), "")

    def test_refusal_exits_3_and_other_failures_do_not(self):
        cli = [sys.executable, os.path.join(SCRIPTS, "task_tag.py"), "relay-check"]
        env = dict(self.env, TASK_TAG_MODE="refuse")
        self.assertEqual(subprocess.run(cli + ["--kind", "task-assign", "--", "do it"], env=env, capture_output=True).returncode, 3)
        self.assertEqual(subprocess.run(cli + ["--bogus", "x"], env=env, capture_output=True).returncode, 2)


if __name__ == "__main__":
    unittest.main()

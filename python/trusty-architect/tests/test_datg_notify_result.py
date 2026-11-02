"""Saved ledger and notification delivery have separate outcomes (CLI exit 3)."""
import contextlib
import io
import json
import os
import subprocess
import sys
from unittest import mock

from test_datg import Base, SCRIPTS, datg


class NotificationResultCase(Base):
    def setUp(self):
        super().setUp()
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "Choose storage", "goal": "th-G01"}])
        self.env = dict(os.environ, DATG_SESSION="tm-fixture")

    def cli(self, *args, stdin=None):
        return subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"),
                               "--project", self.p, *args], input=stdin,
                              env=self.env, capture_output=True, text=True, timeout=10)

    def test_failed_sender_preserves_saved_update_and_stdout(self):
        self.env["DATG_NOTIFY_CMD"] = "/usr/bin/false"
        before = self.state()["v"]
        result = self.cli("update", "th-D01", "--title", "Choose durable storage", "--esc", "architect")
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout, "th-D01\n")
        self.assertIn("th-D01: unconfirmed", result.stderr)
        state = self.state()
        self.assertEqual(state["v"], before + 1)
        self.assertEqual(state["items"][0]["title"], "Choose durable storage")
        self.assertEqual(state["items"][0]["esc"], "architect")

    def test_timeout_is_partial_success_with_uncertain_delivery(self):
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.dict(os.environ, self.env), mock.patch(
            "datg_log.subprocess.run", side_effect=subprocess.TimeoutExpired("sender", 30)
        ) as sender, contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = datg.main(["--project", self.p, "update", "th-D01", "--esc", "architect"])
        self.assertEqual(code, 3)
        self.assertEqual(sender.call_count, 1)
        self.assertEqual(sender.call_args.kwargs["timeout"], 30)
        self.assertIn("delivery may have occurred", err.getvalue())
        self.assertIn("ledger saved", err.getvalue())
        self.assertEqual(self.state()["items"][0]["esc"], "architect")
        self.assertEqual(self.log()[-1]["kind"], "notify_failure")

    def test_log_failure_does_not_mask_saved_ledger_or_notification_failure(self):
        err = io.StringIO()
        with mock.patch.dict(os.environ, self.env), mock.patch(
            "datg_log.subprocess.run", side_effect=FileNotFoundError("sender missing")
        ), mock.patch("datg_log.append_log", side_effect=OSError("log unavailable")), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            code = datg.main(["--project", self.p, "update", "th-D01", "--esc", "architect"])
        self.assertEqual(code, 3)
        self.assertIn("ledger saved", err.getvalue())
        self.assertEqual(self.state()["items"][0]["esc"], "architect")

    def test_mixed_batch_reports_each_sender_and_keeps_all_writes(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "Choose runtime", "goal": "th-G01"}])
        sender = os.path.join(self.p, "sender.sh")
        calls = os.path.join(self.p, "calls")
        with open(sender, "w") as f:
            f.write('#!/bin/sh\nprintf "%s\\n" "$5" >> "' + calls + '"\n'
                    'case "$5" in th-D01*) exit 0;; *) exit 1;; esac\n')
        os.chmod(sender, 0o755)
        self.env["DATG_NOTIFY_CMD"] = sender
        batch = [{"op": "add", "kind": "T", "title": "Keep the unrelated task", "goal": "th-G01"},
                 {"op": "update", "id": "th-D02", "esc": "architect"},
                 {"op": "update", "id": "th-D01", "esc": "architect"}]
        before = self.state()["v"]
        result = self.cli("batch", stdin=json.dumps(batch))
        self.assertEqual(result.returncode, 3)
        self.assertIn("th-D01: sender confirmed queueing", result.stderr)
        self.assertIn("th-D02: unconfirmed", result.stderr)
        self.assertIn("do not replay the saved update/batch", result.stderr)
        state = self.state()
        self.assertEqual(state["v"], before + 1)
        self.assertEqual(len(state["tasks"]), 1)
        self.assertTrue(all(i["esc"] == "architect" for i in state["items"]))
        with open(calls) as f:
            self.assertEqual(len(f.readlines()), 2)  # no automatic retry of either sender

    def test_plain_update_and_batch_do_not_invoke_missing_sender(self):
        self.env["DATG_NOTIFY_CMD"] = os.path.join(self.p, "missing-sender")
        for args, stdin in [(("update", "th-D01", "--title", "New title"), None),
                            (("batch",), json.dumps([{"op": "update", "id": "th-D01", "esc": ""}]))]:
            result = self.cli(*args, stdin=stdin)
            self.assertEqual((result.returncode, result.stderr), (0, ""))

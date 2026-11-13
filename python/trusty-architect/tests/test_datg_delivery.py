"""#10960 G-5 (DOC-87 section 9.5): delivery state on a closed task, flag-only.

A task close may claim a delivery state (prepared < sent < delivered < acknowledged) and a task may say the
state its request needs. The recorded state is the claim capped by the evidence kind in the outcome; a claim
above the evidence, or a recorded state below the need, is a FLAG line. The close always lands.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import datg  # noqa: E402
import datg_guards  # noqa: E402

DATG = os.path.join(SCRIPTS, "datg.py")
MSG = "Notice sent to Matt, message id 1794225540"
PANE = "told Matt; pane capture: captures/matt-0412.txt"


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.p = self._tmp.name
        datg.op_init(self.p, "th")
        datg.write_ops(self.p, [{"op": "add", "kind": "G", "title": "keep Matt informed"},
                                {"op": "add", "kind": "T", "title": "tell Matt the window moved", "goal": "th-G01"}])
        self.sp = datg.paths(self.p)[1]

    def cli(self, *args, stdin=None):
        return subprocess.run([sys.executable, DATG, "--project", self.p, *args], input=stdin,
                              capture_output=True, text=True)

    def edit_task(self, **fields):
        with open(self.sp) as f:
            st = json.load(f)
        st["tasks"][0].update(fields)
        with open(self.sp, "w") as f:
            json.dump(st, f)

    def archived(self, tid="th-T01"):
        return json.loads(datg.archive_lookup(self.p, tid))["item"]

    def guards_log(self):
        try:
            with open(os.path.join(datg.paths(self.p)[0], "guards.jsonl")) as f:
                return [json.loads(x) for x in f]
        except FileNotFoundError:
            return []


class DeliveryCase(Base):
    def test_evidence_kinds_rank_prepared_sent_delivered_acknowledged_10960(self):
        for outcome, want in (("draft in notes/matt.md", "prepared"), ("Matt acknowledged", "prepared"),
                              (MSG, "sent"), ("api id: req-7f3a91", "sent"), ("send ok, exit 0", "sent"),
                              (PANE, "delivered"), ("inbox read: inbox-55120", "delivered"),
                              (MSG + "; reply #4471", "acknowledged"), ("ack: https://example.com/m/9", "acknowledged")):
            self.assertEqual(datg_guards.delivery_state(outcome), want, outcome)

    def test_sent_with_only_a_message_id_records_sent_and_flags_an_acknowledged_claim_10960(self):
        # X14: the request needs an acknowledgement; a message id proves only `sent`.
        self.cli("update", "th-T01", "--needs", "acknowledged")
        r = self.cli("close", "th-T01", "--state", "acknowledged", "--outcome", MSG + "; Matt acknowledged")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.archived()["delivery"], "sent")
        self.assertIn("claims acknowledged but the outcome evidence supports sent", r.stderr)
        self.assertIn("closed done at sent; the request needs acknowledged", r.stderr)
        self.assertEqual([(x["guard"], x["ref"]) for x in self.guards_log()], [("delivery", "th-T01")] * 2)

    def test_tell_closes_at_delivered_when_no_ack_was_requested_10960(self):
        # X15
        self.cli("update", "th-T01", "--needs", "delivered")
        r = self.cli("close", "th-T01", "--state", "delivered", "--outcome", PANE)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertEqual(self.archived()["delivery"], "delivered")
        self.assertEqual(self.guards_log(), [])

    def test_a_claim_with_no_evidence_is_downgraded_to_prepared_10960(self):
        # X16
        r = self.cli("close", "th-T01", "--state", "delivered", "--outcome", "told Matt in notes/matt.md")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.archived()["delivery"], "prepared")
        self.assertIn("claims delivered but the outcome evidence supports prepared", r.stderr)

    def test_prepared_closed_done_against_a_delivery_request_is_flagged_10960(self):
        r = self.cli("add", "T", "--goal", "th-G01", "--title", "send Matt the notice", "--needs", "delivered")
        self.assertEqual(r.returncode, 0, r.stderr)
        r = self.cli("close", "th-T02", "--outcome", "notice drafted in notes/matt-notice.md")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.archived("th-T02")["delivery"], "prepared")
        self.assertIn("th-T02 closed done at prepared; the request needs delivered", r.stderr)

    def test_a_task_with_no_delivery_state_or_need_is_not_flagged_10960(self):
        r = self.cli("close", "th-T01", "--outcome", MSG)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertNotIn("delivery", self.archived())
        self.assertEqual(self.guards_log(), [])

    def test_the_flag_never_refuses_a_close_even_under_enforce_10960(self):
        with open(self.sp) as f:
            st = json.load(f)
        st["config"]["guard_mode"] = "enforce"
        st["tasks"][0]["needs"] = "acknowledged"
        with open(self.sp, "w") as f:
            json.dump(st, f)
        r = self.cli("close", "th-T01", "--state", "acknowledged", "--outcome", MSG)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("FLAG delivery", r.stderr)
        self.assertEqual(self.archived()["status"], "done")
        r = self.cli("guards")
        self.assertNotIn("delivery", r.stdout)   # `guards` exit codes are unchanged: closed tasks are not rescanned

    def test_malformed_or_unreadable_delivery_state_never_reads_as_delivered_10960(self):
        # Fail-open check: a hand-edited or non-string claim, an unknown need, or a raising check -> prepared.
        for bad in ("DELIVERED!!", 3, {"state": "delivered"}, ["delivered"], None):
            with self.subTest(bad=bad):
                self.setUp()
                self.edit_task(delivery=bad, needs="deliverd")
                r = self.cli("close", "th-T01", "--outcome", PANE)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(self.archived()["delivery"], "prepared")
                self.assertIn("is not a delivery state", r.stderr)
                self.assertIn("needs 'deliverd' is not a delivery state", r.stderr)
        self.setUp()
        datg.write_ops(self.p, [{"op": "update", "id": "th-T01", "needs": "delivered"}])
        with mock.patch.object(datg_guards, "delivery_state", side_effect=ValueError("boom")):
            datg.write_ops(self.p, [{"op": "close", "id": "th-T01", "outcome": PANE, "state": "delivered"}])
        self.assertEqual(self.archived()["delivery"], "prepared")
        self.assertIn("not checked (ValueError: boom)", self.guards_log()[-1]["text"])

    def test_owner_redirect_lowers_or_clears_the_need_and_close_still_succeeds_10960(self):
        # Reversal: the owner redirects ("just send it") or cancels; the need follows, the close lands unflagged.
        self.cli("update", "th-T01", "--needs", "acknowledged")
        self.assertEqual(self.cli("update", "th-T01", "--needs", "sent").returncode, 0)
        r = self.cli("close", "th-T01", "--state", "sent", "--outcome", MSG)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.cli("add", "T", "--goal", "th-G01", "--title", "ask Matt to confirm", "--needs", "acknowledged")
        self.assertEqual(self.cli("update", "th-T02", "--needs", "none").returncode, 0)
        r = self.cli("close", "th-T02", "--outcome", "cancelled by the owner, see #10960")
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertNotIn("needs", self.archived("th-T02"))
        r = self.cli("update", "th-G01", "--state", "sent")
        self.assertEqual(r.returncode, 2)
        self.assertIn("state and needs apply to tasks", r.stderr)


if __name__ == "__main__":
    unittest.main()

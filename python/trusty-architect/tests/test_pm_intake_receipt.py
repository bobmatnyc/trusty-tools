"""pm_intake notify/receipt: a local append is reported as unconfirmed, never as queued or delivered (#10723).

Exit contract: 0 acked by the Architect, 2 error (nothing appended), 3 appended and read back, delivery
unconfirmed. datg treats a pm-notify exit 3 carrying the `queued-local` marker as a successful local queue.
"""
import json
import os
import subprocess
import sys

import pm_intake
from test_datg import Base, SCRIPTS, datg

NOTIFY = os.path.join(SCRIPTS, "pm-notify.sh")
INTAKE = os.path.join(SCRIPTS, "pm_intake.py")


def _env(inbox):
    return {**os.environ, "SUPERVISOR_INBOX_DIR": str(inbox), "ARCHITECT_INBOX_DIR": str(inbox)}


def _notify(inbox, *text):
    return subprocess.run([NOTIFY, "--kind", "decision", "--session", "tm-a", *(text or ("pick A",))],
                          capture_output=True, text=True, env=_env(inbox), timeout=30)


def _intake(inbox, *args):
    return subprocess.run([sys.executable, INTAKE, *args], capture_output=True, text=True,
                          env=_env(inbox), timeout=30)


def test_successful_append_reports_queued_local_not_queued(tmp_path):
    r = _notify(tmp_path)
    assert r.returncode == 3, (r.returncode, r.stdout, r.stderr)
    out = r.stdout.strip()
    assert out != "queued"
    assert out.startswith("queued-local "), out
    assert "unconfirmed" in out
    assert "delivered" not in out and " sent" not in out, out
    (line,) = (tmp_path / "pm-messages.jsonl").read_text().splitlines()
    assert out.split()[1].rstrip(":") == pm_intake.msg_id(line)


def test_append_failure_exits_2_with_no_queue_claim(tmp_path):
    (tmp_path / "pm-messages.jsonl").mkdir()  # the target is a directory: the append must fail
    r = _notify(tmp_path)
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
    assert "queued" not in r.stdout
    assert "Traceback" not in r.stderr
    assert "not queued" in r.stderr


def test_receipt_is_unconfirmed_until_the_architect_acks(tmp_path):
    mid = _notify(tmp_path).stdout.split()[1].rstrip(":")
    r = _intake(tmp_path, "receipt", mid)
    assert r.returncode == 3, (r.stdout, r.stderr)
    assert r.stdout.startswith(f"queued-local {mid}") and "unconfirmed" in r.stdout
    assert _intake(tmp_path, "ack", mid, "--outcome", "ruled A").returncode == 0
    r = _intake(tmp_path, "receipt", mid)
    assert (r.returncode, r.stdout.strip()) == (0, f"acked {mid}: ruled A")
    assert _intake(tmp_path, "receipt", "0123456789ab").returncode == 2


class DatgCallerCase(Base):
    """datg's escalation sender reads pm-notify's exit 3 + queued-local as a local queue, not a failure."""

    def test_queued_local_exit_3_is_not_a_notify_failure(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "which db", "goal": "th-G01"}])
        stub = os.path.join(self.p, "stub-notify.sh")
        with open(stub, "w") as f:
            f.write("#!/bin/sh\necho 'queued-local 0123456789ab: appended, delivery unconfirmed'\nexit 3\n")
        os.chmod(stub, 0o755)
        env = dict(os.environ, DATG_NOTIFY_CMD=stub, DATG_SESSION="tm-pilot")
        r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", self.p,
                            "update", "th-D01", "--esc", "architect"], capture_output=True, text=True, env=env)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        self.assertEqual([x for x in self.log() if x.get("kind") == "notify_failure"], [])

    def test_real_pm_notify_escalation_is_queued_without_failure(self):
        datg.write_ops(self.p, [{"op": "add", "kind": "D", "title": "which db", "goal": "th-G01"}])
        inbox = os.path.join(self.p, "inbox")
        env = dict(os.environ, SUPERVISOR_INBOX_DIR=inbox, DATG_SESSION="tm-pilot")
        env.pop("DATG_NOTIFY_CMD", None)
        env.pop("ARCHITECT_INBOX_DIR", None)
        r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", self.p,
                            "update", "th-D01", "--esc", "architect"], capture_output=True, text=True, env=env)
        self.assertEqual((r.returncode, r.stderr), (0, ""))
        lines = [json.loads(x) for x in open(os.path.join(inbox, "pm-messages.jsonl"))]
        self.assertEqual([m["kind"] for m in lines], ["decision"])

"""#10764: a message queued in inbox/pm-messages.jsonl wakes the Architect through the seeded poller.

Why: the seeded fleet-poll.py never read the queue, so a PM's question or an outside MCP caller's message waited
for the Architect's next pass.
What: drives cycle() against a fake Architect pane (no tmux) with a queue in the test's inbox: a waking message
alerts and types the wake, a held one only reaches triage.jsonl, an upgraded poll state skips the backlog, the
outside-MCP wake prefix, `ack --no-reply-needed`, and the shipped copies matching this tree.
"""
import hashlib
import importlib.util
import json
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SUB = os.path.dirname(HERE)
SCRIPTS = os.path.join(SUB, "scripts")
# The shipped seed, when this runs in the monorepo; a standalone tree has only SCRIPTS.
SHIPPED = os.path.join(os.path.dirname(os.path.dirname(SUB)),
                       "crates", "trusty-mpm", "src", "assets", "architect", "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll_pm_messages", "fleet-poll.py")
T0 = 1_000_000.0
RULE = "─" * 60


class Pane:
    """The Architect's idle pane: an empty input box that records what is typed and submitted."""

    def __init__(self):
        self.box, self.submitted = "", []

    def capture(self, _pane=None):
        return "\n".join(["⏺ Done.", RULE, f"❯\xa0{self.box}", RULE]) + "\n"

    def send(self, _pane, *keys):
        if keys[0] == "-l":
            self.box += keys[1]
        elif keys[0] == "BSpace":
            self.box = self.box[:-len(keys)]
        elif keys[0] == "Enter":
            self.submitted.append(self.box)
            self.box = ""


@pytest.fixture
def pane(monkeypatch, tmp_path):
    p = Pane()
    monkeypatch.setattr(fp, "list_panes", lambda: [{"session": fp.ARCHITECT, "pane": "%1",
                                                    "cmd": "claude", "active": "11"}])
    monkeypatch.setattr(fp, "capture", p.capture)
    monkeypatch.setattr(fp, "send_to_architect", p.send)
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    monkeypatch.setattr(fp, "host_alerts", lambda now: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "read_events", lambda state: [])
    monkeypatch.setattr(fp, "watchdog_alerts", lambda *a: ([], "architect=ok"))
    monkeypatch.setattr(fp, "il_cron_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "ctx_watch_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "owner_answer_alerts", lambda now, dry_run, ever_read=False: ([], None))
    monkeypatch.setattr(fp, "self_clear_running", lambda arch_dir: False)
    monkeypatch.setattr(fp, "ALERTS", str(tmp_path / "alerts.jsonl"))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    monkeypatch.setattr(fp, "STATE", str(tmp_path / "poll-state.json"))
    monkeypatch.setattr(fp, "HOLD", str(tmp_path / "hold.json"))
    monkeypatch.setattr(fp, "QUIET_SESSIONS_FILE", str(tmp_path / "quiet.txt"))
    return p


def _queue(tmp_path, *msgs):
    """Append each message to the test inbox's queue (conftest points SUPERVISOR_INBOX_DIR at tmp_path)."""
    with open(tmp_path / "pm-messages.jsonl", "a") as f:
        for m in msgs:
            f.write(json.dumps(m) + "\n")


def _pm_alerts(tmp_path):
    path = tmp_path / "alerts.jsonl"
    if not path.exists():
        return []
    return [a for a in map(json.loads, path.read_text().splitlines()) if a["kind"] == "pm_message"]


QUESTION = {"ts": "2026-11-27T00:00:00Z", "session": "tm-a", "kind": "decision", "text": "A or B?"}


def test_a_queued_question_wakes_the_architect_on_the_next_cycle(pane, tmp_path):
    _queue(tmp_path, QUESTION)
    fp.cycle({}, now=T0)
    alerts = _pm_alerts(tmp_path)
    assert [a["session"] for a in alerts] == ["tm-a"], alerts
    assert "decision: A or B?" in alerts[0]["detail"]
    assert len(pane.submitted) == 1 and "1 new alerts ->" in pane.submitted[0]


def test_a_held_message_reaches_triage_and_wakes_nobody(pane, tmp_path):
    _queue(tmp_path, dict(QUESTION, kind="progress", text="halfway"))
    fp.cycle({}, now=T0)
    assert _pm_alerts(tmp_path) == [] and pane.submitted == []
    (rec,) = map(json.loads, (tmp_path / "triage.jsonl").read_text().splitlines())
    assert (rec["kind"], rec["decision"], rec["reason"]) == ("pm_message", "log", "progress")


def test_each_message_wakes_once(pane, tmp_path):
    _queue(tmp_path, QUESTION)
    state = {}
    fp.cycle(state, now=T0)
    fp.cycle(state, now=T0 + 60)
    assert len(_pm_alerts(tmp_path)) == 1 and len(pane.submitted) == 1


def test_an_upgraded_poll_state_skips_the_backlog(pane, tmp_path):
    _queue(tmp_path, QUESTION, dict(QUESTION, text="old blocker", kind="blocker"))
    state = {"sessions": {}, "alerts": {}}          # a poller state written before #10764
    fp.cycle(state, now=T0)
    assert _pm_alerts(tmp_path) == [], "the backlog must not wake"
    _queue(tmp_path, dict(QUESTION, text="new one?"))
    fp.cycle(state, now=T0 + 60)
    assert [a["detail"] for a in _pm_alerts(tmp_path)] == ["decision: new one?"]


def test_outside_mcp_wake_text_names_the_sender(tmp_path):
    pi = fp.pm_intake
    msg = {"session": "mcp:ext", "source": "mcp", "sender": "ext", "kind": "decision", "text": "hi"}
    _queue(tmp_path, msg)
    mk = lambda kind, mid, detail, session, pane, now: {"fingerprint": mid, "detail": detail}  # noqa: E731
    (got,) = pi.poll_step({}, mk, T0, str(tmp_path / "triage.jsonl"))
    assert got["detail"] == f"[OUTSIDE MCP sender=ext, not the owner; id {got['fingerprint']}] decision: hi"
    assert pi.origin({"kind": "done"}, "abc") == ""
    assert pi.origin({"source": "mcp"}, "abc") == "[OUTSIDE MCP sender=?, not the owner; id abc] "


def test_ack_no_reply_needed_acks_an_unreplied_mcp_message(tmp_path):
    _queue(tmp_path, {"session": "mcp:ext", "source": "mcp", "sender": "ext", "kind": "fyi", "text": "note"})
    env = dict(os.environ, SUPERVISOR_INBOX_DIR=str(tmp_path))
    env.pop("ARCHITECT_INBOX_DIR", None)
    run = lambda *a: subprocess.run([sys.executable, os.path.join(SCRIPTS, "pm_intake.py"), *a],  # noqa: E731
                                    capture_output=True, text=True, env=env)
    (mid,) = [line.split()[0] for line in run("unread").stdout.splitlines()]
    refused = run("ack", mid, "--outcome", "seen")
    assert refused.returncode != 0 and "--no-reply-needed" in refused.stderr
    done = run("ack", mid, "--outcome", "seen", "--no-reply-needed")
    assert done.returncode == 0, done.stderr
    assert done.stdout.strip() == f"acked 1 first={mid} last={mid}"
    (row,) = map(json.loads, (tmp_path / "pm-acks.jsonl").read_text().splitlines())
    assert row["outcome"] == "no-reply-needed: seen"


@pytest.mark.parametrize("name", ["fleet-poll.py", "pm_intake.py"])
def test_shipped_copy_and_provenance_match_this_tree(name):
    mine = open(os.path.join(SCRIPTS, name), "rb").read()
    if os.path.isdir(SHIPPED):
        assert open(os.path.join(SHIPPED, name), "rb").read() == mine
    prov = json.load(open(os.path.join(SUB, "provenance.json")))
    assert prov["files"][f"scripts/{name}"] == hashlib.sha256(mine).hexdigest()

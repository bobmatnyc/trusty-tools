"""Tests for pm_cursor: unread/ack cursor, hook injection, wake-failure alert, 30-min escalation (owner D39).

Test: tests/test_pm_cursor.py
"""
import json
import os
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pending_hook_dedupe as ph  # noqa: E402
import pm_cursor as pc  # noqa: E402
import pm_intake  # noqa: E402

T0 = 1_800_000_000.0


@pytest.fixture
def inbox(tmp_path, monkeypatch):
    monkeypatch.setenv("SUPERVISOR_INBOX_DIR", str(tmp_path))
    return tmp_path


def put(inbox, *msgs):
    with open(inbox / "pm-messages.jsonl", "a") as f:
        for m in msgs:
            f.write(json.dumps(m) + "\n")
    return [i for i, _ in pc.JsonlSource().messages()]


def M(kind="decision", text="x", ts=None, **kw):
    return dict({"ts": ts or pm_intake.iso(T0), "session": "tm-a", "kind": kind, "text": text}, **kw)


def test_unread_ack_and_wake_only(inbox):
    d, p = put(inbox, M("decision"), M("progress", "plain note"))
    assert [m["id"] for m in pc.unread()] == [d, p]  # hold counts as unread for listing
    assert [m["id"] for m in pc.unread(wake_only=True)] == [d]
    assert pc.ack([d], "relayed", now=T0) == [d]
    assert [m["id"] for m in pc.unread()] == [p]
    assert pc.ack([d], "again") == []  # idempotent, no second line
    rec = [json.loads(line) for line in open(inbox / "pm-acks.jsonl")]
    assert rec == [{"ts": pm_intake.iso(T0), "id": d, "outcome": "relayed"}]
    with pytest.raises(ValueError):
        pc.ack(["nope"], "x")


def test_ack_through_acks_prefix_only(inbox):
    a, b, c = put(inbox, M(text="1"), M(text="2"), M(text="3"))
    assert pc.ack_through(b, "batch") == [a, b]
    assert [m["id"] for m in pc.unread()] == [c]
    with pytest.raises(ValueError):
        pc.ack_through("nope", "x")


def test_inject_block_lists_every_call_and_caps(inbox):
    assert pc.inject_block(now=T0) is None
    put(inbox, *[M(text=f"msg {i} " + "y" * 200, ts=pm_intake.iso(T0 + i), pr=7, head="abcdef1234567") for i in range(12)])
    for _ in range(2):  # no dedupe: identical on every call
        out = pc.inject_block(now=T0 + 20)
        lines = out.splitlines()
        assert lines[0].startswith("PM MESSAGES UNREAD (ack with pm_intake.py ack <id> --outcome")
        assert len(lines) == 1 + 10 + 1 and lines[-1] == "+2 more"
        assert "tm-a decision PR#7 abcdef1234" in lines[1]
        assert len(lines[1].split(" ", 6)[6]) <= 120


def test_hold_only_unread_injects_nothing(inbox):
    put(inbox, M("progress", "just a note"))
    assert pc.inject_block(now=T0) is None


def test_wake_failure_alert_until_sent_or_empty(inbox):
    [i] = put(inbox, M())
    pc.record_wake_result("skipped: dialog open", now=T0)
    assert "WAKE FAILING" not in pc.inject_block(now=T0)
    pc.record_wake_result("failed: text not confirmed", now=T0 + 10)
    pc.record_wake_result("failed: text not confirmed", now=T0 + 130)
    out = pc.inject_block(now=T0 + 140)
    assert out.splitlines()[0] == f"WAKE FAILING since {pm_intake.iso(T0 + 10)} (2 attempts)"
    pc.record_wake_result("sent to %0", now=T0 + 200)
    assert "WAKE FAILING" not in pc.inject_block(now=T0 + 210)
    pc.record_wake_result("failed: x", now=T0 + 300)
    pc.ack([i], "done")
    assert pc.inject_block(now=T0 + 310) is None


def sync_recorder():
    calls = []

    def fn(item_id, summary):
        calls.append((item_id, summary["count"], summary["oldest"]["id"] if summary["oldest"] else None))
        return item_id or "A01"
    return calls, fn


def test_escalation_one_summary_item_updated_in_place(inbox):
    pc.inject_block(now=T0)  # first run: sets `since`
    a, b = put(inbox, M(ts=pm_intake.iso(T0 + 1)), M(text="b", ts=pm_intake.iso(T0 + 300)))
    calls, fn = sync_recorder()
    pc.inject_block(now=T0 + 29 * 60, sync_fn=fn)
    assert calls == []
    out = pc.inject_block(now=T0 + 31 * 60, sync_fn=fn)
    pc.inject_block(now=T0 + 32 * 60, sync_fn=fn)  # unchanged: no second call
    assert calls == [(None, 1, a)] and "PUSH DUE" not in out
    pc.inject_block(now=T0 + 36 * 60, sync_fn=fn)  # b crosses 30 min: same item, count 2
    assert calls[1] == ("A01", 2, a)
    pc.ack([a, b], "x")
    pc.inject_block(now=T0 + 40 * 60, sync_fn=fn)  # all acked: the summary closes
    assert calls[2] == ("A01", 0, None)
    assert json.load(open(inbox / "pm-escalation.json"))["summary_id"] is None


def test_backlog_and_old_messages_never_escalate(inbox):
    old = put(inbox, M(ts=pm_intake.iso(T0 - 5 * 3600)), M(text="b", ts=pm_intake.iso(T0 - 90000)))
    calls, fn = sync_recorder()
    pc.inject_block(now=T0, sync_fn=fn)  # first run
    pc.inject_block(now=T0 + 3600 * 2, sync_fn=fn)
    assert calls == [] and pc.push_due() == []
    # a fresh message older than 24 h by the time it is still unacked drops out again
    put(inbox, M(text="c", ts=pm_intake.iso(T0 + 10)))
    pc.inject_block(now=T0 + 3600 * 25, sync_fn=fn)
    assert calls == [] and len(old) == 2


def test_push_due_at_60_min_for_eligible_only(inbox):
    pc.inject_block(now=T0)
    [i] = put(inbox, M(ts=pm_intake.iso(T0 + 1)))
    calls, fn = sync_recorder()
    out = pc.inject_block(now=T0 + 61 * 60, sync_fn=fn)
    assert "PUSH DUE" in out and i in out.splitlines()[-1]
    assert pc.push_due() == [i] and pc.push_due(mark=True) == [i] and pc.push_due() == []
    assert "PUSH DUE" not in pc.inject_block(now=T0 + 62 * 60, sync_fn=fn)


def test_old_format_state_is_migrated_without_flood(inbox):
    put(inbox, *[M(text=str(n), ts=pm_intake.iso(T0 - 3600)) for n in range(5)])
    (inbox / "pm-escalation.json").write_text(json.dumps({"escalated": {}, "push_due": {}, "retry_at": 1.0}))
    calls, fn = sync_recorder()
    pc.inject_block(now=T0, sync_fn=fn)
    pc.inject_block(now=T0 + 7200, sync_fn=fn)
    st = json.load(open(inbox / "pm-escalation.json"))
    assert calls == [] and "escalated" not in st and st["since"] == T0


def test_failed_dashboard_sync_retries_after_5_min(inbox):
    pc.inject_block(now=T0)
    put(inbox, M(ts=pm_intake.iso(T0 + 1)))
    calls = []
    bad = lambda i, s: calls.append(1)  # noqa: E731  returns None = failure
    pc.inject_block(now=T0 + 1900, sync_fn=bad)
    pc.inject_block(now=T0 + 1960, sync_fn=bad)  # inside the retry window
    assert len(calls) == 1
    pc.inject_block(now=T0 + 1900 + 301, sync_fn=bad)
    assert len(calls) == 2


def test_dashboard_sync_refuses_when_fewer_than_16_ids_free(monkeypatch, capsys):
    import dashboard_ids
    ran = []
    monkeypatch.setattr(dashboard_ids, "free_count", lambda prefix, root=None: 15)
    monkeypatch.setattr(pc, "_run_db", lambda *a: ran.append(a))
    m = M(id="abc")
    assert pc._dashboard_sync(None, {"count": 3, "oldest": m}) is None
    assert ran == [] and "free A ids" in capsys.readouterr().err
    monkeypatch.setattr(dashboard_ids, "free_count", lambda prefix, root=None: 16)
    monkeypatch.setattr(pc, "_run_db", lambda *a: subprocess.CompletedProcess(a, 0, '{"id": "A07"}', ""))
    assert pc._dashboard_sync(None, {"count": 3, "oldest": m}) == "A07"


def test_hook_output_skips_subagent_and_echoes_event(inbox, monkeypatch):
    put(inbox, M())
    monkeypatch.setattr(ph, "STATE", str(inbox / "ph.json"))
    assert ph.build_output({"agent_id": "sub1", "session_id": "s"}, now=T0) is None
    for ev in ("PostToolUse", "UserPromptSubmit"):
        out = ph.build_output({"session_id": "s", "hook_event_name": ev}, now=T0)
        assert out["hookSpecificOutput"]["hookEventName"] == ev
        assert "PM MESSAGES UNREAD" in out["hookSpecificOutput"]["additionalContext"]


def test_cli_unread_ack_ack_through(inbox):
    a, b = put(inbox, M(text="one"), M("progress", "two"))
    env = dict(os.environ, SUPERVISOR_INBOX_DIR=str(inbox))
    run = lambda *a_: subprocess.run([sys.executable, os.path.join(pm_cursor_dir(), "pm_intake.py"), *a_],  # noqa: E731
                                     capture_output=True, text=True, env=env)
    assert a in run("unread", "--wake-only").stdout and b not in run("unread", "--wake-only").stdout
    assert b in run("unread").stdout
    r = run("ack-through", b, "--outcome", "swept")
    assert r.returncode == 0 and r.stdout.strip() == f"acked 2 first={a} last={b}"
    assert run("unread").stdout == ""
    assert run("ack", "zzz", "--outcome", "x").returncode != 0


def pm_cursor_dir():
    return os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")



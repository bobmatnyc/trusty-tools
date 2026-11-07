"""Tests for pending_hook_dedupe: the PostToolUse hook speaks only when the pending set changes.

Test: tests/test_pending_answers_hook.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pending_hook_dedupe as ph  # noqa: E402

A = {"id": "D01", "kind": "answered", "at": "t1", "text": "yes"}
B = {"id": "D02", "kind": "wont_do", "at": "t2", "text": "no"}
UNPARK = {"id": "A0a", "kind": "owner_unpark_due", "at": "t0", "text": "0 updates since parked"}


def test_same_set_twice_is_silent(tmp_path):
    st = str(tmp_path / "s.json")
    assert ph.decide([A, B], st, "sess", now=1000.0) is not None
    assert ph.decide([A, B], st, "sess", now=1010.0) is None


def test_changed_set_emits(tmp_path):
    st = str(tmp_path / "s.json")
    ph.decide([A], st, "sess", now=1000.0)
    out = ph.decide([A, B], st, "sess", now=1010.0)
    assert out and "D02" in out


def test_reminder_after_15_minutes_once(tmp_path):
    st = str(tmp_path / "s.json")
    ph.decide([A], st, "sess", now=1000.0)
    assert ph.decide([A], st, "sess", now=1000.0 + 899) is None
    assert ph.decide([A], st, "sess", now=1000.0 + 900) is not None
    assert ph.decide([A], st, "sess", now=1000.0 + 901) is None


def test_unpark_due_never_listed(tmp_path):
    st = str(tmp_path / "s.json")
    assert ph.decide([UNPARK], st, "sess", now=1000.0) is None
    out = ph.decide([UNPARK, A], st, "sess", now=1001.0)
    assert "A0a" not in out and "D01" in out


def test_empty_resets_and_sessions_are_independent(tmp_path):
    st = str(tmp_path / "s.json")
    ph.decide([A], st, "s1", now=1000.0)
    assert ph.decide([], st, "s1", now=1001.0) is None
    assert ph.decide([A], st, "s1", now=1002.0) is not None  # returned after clearing
    assert ph.decide([A], st, "s2", now=1003.0) is not None  # other session has its own state


def _turns():
    import json
    path = os.environ["ARCHITECT_OWNER_TURNS"]
    return [json.loads(x) for x in open(path, encoding="utf-8")] if os.path.exists(path) else []


def test_owner_prompt_is_recorded_verbatim_for_relay_citations_10780(tmp_path, monkeypatch):
    """#10780: relay_provenance.py checks an owner-labelled relay against this record."""
    monkeypatch.setattr(ph, "STATE", str(tmp_path / "ph.json"))
    monkeypatch.setattr(ph, "ROOT", str(tmp_path))   # the dashboard store and user state stay in tmp
    ph.build_output({"session_id": "s", "hook_event_name": "UserPromptSubmit",
                     "prompt": "reset was done just now"}, now=1_793_490_000.0)
    for not_owner in ("[poll 23:40Z] wake", "<task-notification>", "/clear"):
        ph.build_output({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt": not_owner}, now=1.0)
    ph.build_output({"session_id": "s", "hook_event_name": "PostToolUse", "prompt": "x"}, now=1.0)
    assert [(t["at"], t["text"]) for t in _turns()] == [("2026-10-31T23:40:00Z", "reset was done just now")]


def test_owner_turn_record_keeps_its_newest_half_past_the_cap_10780(monkeypatch):
    monkeypatch.setattr(ph, "OWNER_TURNS_MAX", 400)
    for i in range(12):
        ph.record_owner_turn({"prompt": f"turn {i:02d} " + "x" * 40}, now=1_793_490_000.0 + i)
    texts = [t["text"][:7] for t in _turns()]
    assert texts[-1] == "turn 11" and "turn 00" not in texts and os.path.getsize(os.environ["ARCHITECT_OWNER_TURNS"]) <= 400

"""Tests for pending_hook_dedupe: the PostToolUse hook speaks only when the pending set changes.

Test: scripts/test_pending_answers_hook.py
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

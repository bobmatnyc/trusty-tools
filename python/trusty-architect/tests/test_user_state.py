"""User state (dashboard settings pane): default active, one poller alert per change, hook line.

Test: scripts/test_user_state.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pending_hook_dedupe as ph  # noqa: E402
import user_state  # noqa: E402



def put_settings(root, state, at="2026-10-09T15:40:00Z", by="owner"):
    d = root / "meta"
    d.mkdir(exist_ok=True)
    (d / "settings.json").write_text(json.dumps(
        {"version": 1, "user_state": state, "user_state_at": at, "user_state_by": by}))


def test_default_is_active_when_unset_or_invalid(tmp_path):
    assert user_state.read(str(tmp_path))["state"] == "active"
    put_settings(tmp_path, "napping")
    assert user_state.read(str(tmp_path))["state"] == "active"


def test_read_returns_the_stored_state(tmp_path):
    put_settings(tmp_path, "remote")
    assert user_state.read(str(tmp_path)) == {"state": "remote", "at": "2026-10-09T15:40:00Z", "by": "owner"}


def test_hook_line_leads_and_is_deduped(tmp_path):
    s = str(tmp_path / "s.json")
    a = {"id": "D01", "kind": "answered", "at": "t1", "text": "yes"}
    out = ph.decide([a], s, "sess", now=1000.0, user_state="remote")
    assert out.splitlines()[0] == "USER STATE: remote" and "D01" in out
    assert ph.decide([a], s, "sess", now=1010.0, user_state="remote") is None
    assert ph.decide([], s, "sess", now=1020.0, user_state="remote") is None
    assert ph.decide([], s, "sess", now=1030.0, user_state="away") == "USER STATE: away"
    assert ph.decide([], s, "sess", now=1030.0 + 900, user_state="away") == "USER STATE: away"
    assert ph.decide([], s, "sess", now=1030.0 + 901, user_state="away") is None

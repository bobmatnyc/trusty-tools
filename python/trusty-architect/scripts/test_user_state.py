"""User state (dashboard settings pane): default active, one poller alert per change, hook line.

Test: scripts/test_user_state.py
"""
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pending_hook_dedupe as ph  # noqa: E402
import user_state  # noqa: E402

_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)


def put_settings(root, state, at="2026-10-09T15:40:00Z", by="Owner"):
    d = root / "meta"
    d.mkdir(exist_ok=True)
    (d / "settings.json").write_text(json.dumps(
        {"version": 1, "data": {"user_state": state, "user_state_at": at, "user_state_by": by}}))


def test_default_is_active_when_unset_or_invalid(tmp_path):
    assert user_state.read(str(tmp_path))["state"] == "active"
    put_settings(tmp_path, "napping")
    assert user_state.read(str(tmp_path))["state"] == "active"


def test_read_returns_the_stored_state(tmp_path):
    put_settings(tmp_path, "remote")
    assert user_state.read(str(tmp_path)) == {"state": "remote", "at": "2026-10-09T15:40:00Z", "by": "Owner"}


def test_alert_once_per_change(tmp_path):
    st = {}
    assert fp.user_state_alerts(1000, str(tmp_path)) == []          # unset: nothing to announce
    put_settings(tmp_path, "remote")
    new, _ = fp.dedup(st, fp.user_state_alerts(1000, str(tmp_path)), 1000)
    assert len(new) == 1 and new[0]["kind"] == "user_state"
    assert new[0]["fingerprint"] == "2026-10-09T15:40:00Z"
    assert new[0]["display"] == "[dashboard]: user state -> remote"
    assert fp.dedup(st, fp.user_state_alerts(1100, str(tmp_path)), 1100)[0] == []
    put_settings(tmp_path, "away", at="2026-10-09T16:00:00Z")
    new, _ = fp.dedup(st, fp.user_state_alerts(1200, str(tmp_path)), 1200)
    assert [a["display"] for a in new] == ["[dashboard]: user state -> away"]


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

"""Effective user state: any owner signal under 15 minutes old makes the state active over the manual setting.

Test: scripts/test_user_state_effective.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pending_hook_dedupe as ph  # noqa: E402
import user_state  # noqa: E402

NOW = user_state._epoch("2026-10-29T23:00:00Z")


def put(root, coll, doc_id, data):
    d = root / coll
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def sleeping(root, by="Architect (auto-sleep)"):
    put(root, "meta", "settings", {"user_state": "sleeping", "user_state_at": "2026-10-29T19:00:00.000Z", "user_state_by": by})


def test_signal_within_15_min_gives_active_over_manual_sleeping(tmp_path):
    sleeping(tmp_path)
    put(tmp_path, "items", "Da8", {"answered_at": "2026-10-29T22:57:00.000Z", "status": "done"})
    eff = user_state.effective(str(tmp_path), NOW)
    assert eff["state"] == "active" and eff["manual"] == "sleeping"
    assert eff["reason"] == "active: answered Da8 3 min ago"


def test_signal_older_than_15_min_gives_manual(tmp_path):
    sleeping(tmp_path)
    put(tmp_path, "items", "Da8", {"answered_at": "2026-10-29T22:44:00.000Z"})
    eff = user_state.effective(str(tmp_path), NOW)
    assert eff["state"] == "sleeping" and eff["reason"].startswith("sleeping: manual setting")


def test_no_signal_gives_manual(tmp_path):
    sleeping(tmp_path)
    assert user_state.effective(str(tmp_path), NOW)["state"] == "sleeping"
    assert user_state.effective(str(tmp_path / "empty"), NOW)["state"] == "active"  # nothing stored: default


def test_architect_stamps_are_not_owner_signals(tmp_path):
    sleeping(tmp_path)  # user_state_at 19:00 by the Architect, and an Architect-answered item
    put(tmp_path, "items", "D01", {"answered_at": "2026-10-29T22:58:00.000Z", "answered_by": "Architect"})
    assert user_state.effective(str(tmp_path), NOW)["state"] == "sleeping"


def test_owner_state_change_and_prompt_signal_and_sync(tmp_path):
    put(tmp_path, "meta", "settings", {"user_state": "remote", "user_state_at": "2026-10-29T19:00:00.000Z", "user_state_by": "owner"})
    assert user_state.effective(str(tmp_path), user_state._epoch("2026-10-29T19:05:00Z"))["state"] == "active"  # manual remote still counts
    sleeping(tmp_path)
    assert user_state.record_signal(str(tmp_path), "turn in tm-architect", NOW)
    assert not user_state.record_signal(str(tmp_path), "turn in tm-architect", NOW + 10)  # throttled
    assert user_state.effective(str(tmp_path), NOW + 120)["reason"] == "active: turn in tm-architect 2 min ago"
    put(tmp_path, "items", "Da9", {"answered_at": "2026-10-29T23:05:00.000Z"})
    assert user_state.sync_signal(str(tmp_path), NOW + 400)
    assert json.loads((tmp_path / "meta" / "settings.json").read_text())["owner_signal_kind"] == "answered Da9"


def test_hook_prints_effective_state_with_reason(tmp_path, monkeypatch):
    sleeping(tmp_path)
    put(tmp_path, "items", "Da8", {"answered_at": "2026-10-29T22:57:00.000Z"})
    s = str(tmp_path / "s.json")
    eff = user_state.effective(str(tmp_path), NOW)
    out = ph.decide([], s, "sess", now=NOW, user_state=eff["state"], state_reason=eff["reason"])
    assert out == "USER STATE: active (active: answered Da8 3 min ago)"
    assert ph.owner_prompt({"hook_event_name": "UserPromptSubmit", "prompt": "do it"})
    assert not ph.owner_prompt({"hook_event_name": "UserPromptSubmit", "prompt": "[poll 23:00Z] 1 new alerts"})
    assert not ph.owner_prompt({"hook_event_name": "PostToolUse", "prompt": "do it"})


def stored(root):
    return json.loads((root / "meta" / "settings.json").read_text())


def test_signal_flips_stored_sleeping_to_active_logged_as_auto(tmp_path):
    sleeping(tmp_path)
    assert user_state.record_signal(str(tmp_path), "turn in tm-architect", NOW)
    d = stored(tmp_path)
    assert d["user_state"] == "active" and d["user_state_by"] == "auto: owner activity (turn in tm-architect)"
    assert user_state.read(str(tmp_path))["state"] == "active"
    # the auto stamp is not itself an owner signal
    assert user_state.last_signal(str(tmp_path))[1] == "turn in tm-architect"


def test_signal_leaves_remote_and_active_alone(tmp_path):
    for st in ("remote", "active"):
        put(tmp_path, "meta", "settings", {"user_state": st, "user_state_by": "owner"})
        assert user_state.record_signal(str(tmp_path), "turn in tm-architect", NOW)
        d = stored(tmp_path)
        assert d["user_state"] == st and d["user_state_by"] == "owner"


def test_architect_write_never_flips_and_stale_signal_does_not(tmp_path):
    sleeping(tmp_path)
    put(tmp_path, "items", "D01", {"answered_at": "2026-10-29T22:58:00.000Z", "answered_by": "Architect"})
    assert not user_state.sync_signal(str(tmp_path), NOW)
    assert stored(tmp_path)["user_state"] == "sleeping"
    put(tmp_path, "items", "D02", {"answered_at": "2026-10-29T20:00:00.000Z"})  # owner, but 3 h old
    assert user_state.sync_signal(str(tmp_path), NOW)
    assert stored(tmp_path)["user_state"] == "sleeping"
    put(tmp_path, "items", "D03", {"answered_at": "2026-10-29T22:59:00.000Z"})
    assert user_state.sync_signal(str(tmp_path), NOW)
    assert stored(tmp_path)["user_state"] == "active"


# owner 2026-10-30 ~03:40Z: "Don't count that as me being awake, I'm setting it to walk away."
def test_manual_sleeping_set_is_not_a_signal_and_grace_ignores_turn(tmp_path):
    sleeping(tmp_path, by="owner")  # set 19:00
    t = user_state._epoch("2026-10-29T19:00:00Z")
    assert user_state.effective(str(tmp_path), t + 60)["state"] == "sleeping"  # the set itself is no signal
    assert not user_state.record_signal(str(tmp_path), "turn in tm-architect", t + 60)  # turn 1 min later ignored
    data = json.loads((tmp_path / "meta" / "settings.json").read_text())
    assert data["user_state"] == "sleeping" and "owner_signal_at" not in data
    assert user_state.effective(str(tmp_path), t + 60)["state"] == "sleeping"


def test_signal_after_grace_wakes_manual_sleeping(tmp_path):
    sleeping(tmp_path, by="owner")
    t = user_state._epoch("2026-10-29T19:00:00Z")
    assert user_state.record_signal(str(tmp_path), "turn in tm-architect", t + 600)
    assert user_state.effective(str(tmp_path), t + 660)["state"] == "active"
    assert json.loads((tmp_path / "meta" / "settings.json").read_text())["user_state"] == "active"


def test_grace_ignores_answers_and_sync(tmp_path):
    put(tmp_path, "meta", "settings", {"user_state": "away", "user_state_at": "2026-10-29T19:00:00.000Z", "user_state_by": "owner"})
    put(tmp_path, "items", "Da8", {"answered_at": "2026-10-29T19:03:00.000Z"})
    t = user_state._epoch("2026-10-29T19:00:00Z")
    assert user_state.effective(str(tmp_path), t + 240)["state"] == "away"
    assert not user_state.sync_signal(str(tmp_path), t + 240)


def test_mobile_is_sticky_over_a_fresh_owner_signal(tmp_path):
    put(tmp_path, "meta", "settings", {"user_state": "mobile", "user_state_at": "2026-10-29T19:00:00.000Z", "user_state_by": "owner",
                                       "owner_signal_at": "2026-10-29T22:58:00.000Z", "owner_signal_kind": "turn in tm-architect"})
    eff = user_state.effective(str(tmp_path), NOW)
    assert eff["state"] == "mobile" and eff["manual"] == "mobile"
    assert user_state.record_signal(str(tmp_path), "turn in tm-architect", NOW)
    assert user_state.read(str(tmp_path))["state"] == "mobile"  # the signal write never flips the stored state

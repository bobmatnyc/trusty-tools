"""auto_sleep.step with a fixed clock; owner_answers resume_request; resume alert.

Test: scripts/test_auto_sleep.py
"""
import json
import os
from datetime import datetime, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
import sys
sys.path.insert(0, HERE)
import auto_sleep  # noqa: E402


def utc(s):
    return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()


def put_settings(root, **data):
    d = root / "meta"
    d.mkdir(exist_ok=True)
    (d / "settings.json").write_text(json.dumps({"version": 3, **data}))


def read(root):
    raw = json.loads((root / "meta" / "settings.json").read_text())
    return {"version": raw.pop("version"), "data": raw}


# Rome is UTC+2 on 2026-10-10 (CEST until 10-25).
BASE = dict(tz="Europe/Rome", sleep_start="20:00", user_state="active")


def test_fires_once_at_start_time(tmp_path):
    put_settings(tmp_path, **BASE)
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T17:59:00")) is None   # 19:59 Rome
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T18:00:00")) == "fired"  # 20:00 Rome
    doc = read(tmp_path)
    assert doc["version"] == 4
    assert doc["data"]["user_state"] == "sleeping" and doc["data"]["sleep_auto_on"] == "2026-10-10"
    assert doc["data"]["user_state_by"] == "Architect (auto-sleep)"
    assert doc["data"]["user_state_at"] == "2026-10-10T18:00:00.000Z"
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T18:02:00")) is None


def test_overnight_does_not_fire_before_start_next_morning(tmp_path):
    put_settings(tmp_path, **BASE, sleep_auto_on="2026-10-10")
    assert auto_sleep.step(str(tmp_path), utc("2026-10-11T01:00:00")) is None   # 03:00 Rome next day
    assert auto_sleep.step(str(tmp_path), utc("2026-10-11T17:00:00")) is None   # 19:00 Rome
    assert auto_sleep.step(str(tmp_path), utc("2026-10-11T18:30:00")) == "fired"  # 20:30 Rome, new date


def test_already_sleeping_is_not_rewritten_but_marked(tmp_path):
    put_settings(tmp_path, **{**BASE, "user_state": "sleeping"}, user_state_at="x", user_state_by="owner")
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T19:00:00")) == "marked"
    d = read(tmp_path)["data"]
    assert d["user_state_at"] == "x" and d["user_state_by"] == "owner" and d["sleep_auto_on"] == "2026-10-10"


def test_owner_woke_after_fire_is_not_slept_again_that_day(tmp_path):
    put_settings(tmp_path, **BASE)
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T18:00:00")) == "fired"
    d = read(tmp_path)["data"]
    d["user_state"] = "active"
    put_settings(tmp_path, **d)
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T20:00:00")) is None
    assert read(tmp_path)["data"]["user_state"] == "active"


@pytest.mark.parametrize("extra", [{"sleep_start": "8pm"}, {"tz": "Nowhere/Land"}, {"sleep_start": None}])
def test_bad_settings_do_nothing(tmp_path, extra):
    put_settings(tmp_path, **{**BASE, **extra})
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T23:00:00")) is None
    assert read(tmp_path)["version"] == 3


def test_missing_settings_and_conflict(tmp_path, monkeypatch):
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T23:00:00")) is None
    put_settings(tmp_path, **BASE)
    real = auto_sleep.ds.Store.update

    def racing(self, coll, doc_id, data, if_version=None):
        return real(self, coll, doc_id, data, if_version=if_version - 1)

    monkeypatch.setattr(auto_sleep.ds.Store, "update", racing)
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T23:00:00")) is None


def test_mobile_is_treated_like_remote_at_sleep_start(tmp_path):
    put_settings(tmp_path, **dict(BASE, user_state="mobile"))
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T18:00:00")) == "fired"
    assert read(tmp_path)["data"]["user_state"] == "sleeping"


def test_mobile_before_sleep_start_is_untouched(tmp_path):
    put_settings(tmp_path, **dict(BASE, user_state="mobile"))
    assert auto_sleep.step(str(tmp_path), utc("2026-10-10T10:00:00")) is None
    assert read(tmp_path)["data"]["user_state"] == "mobile"

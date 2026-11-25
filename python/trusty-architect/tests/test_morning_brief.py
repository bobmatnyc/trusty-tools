"""Morning brief scheduler: due day, window, once per local date across restarts, settings, DST.

#10571 S2: the pin's test_fleet_poll_morning_brief.py drove morning_brief.step through fleet-poll.py's
morning_brief_alerts; here alerts() wraps step() itself. The alert's kind, fingerprint and detail text, its dedup
and triage, and the store-error log belong to the poller and ship with it (S3).
Test: tests/test_morning_brief.py
Dates: Rome is UTC+2 until 2026-10-25 and UTC+1 after; New York springs forward on 2026-03-08.
"""
import json
from datetime import datetime, timezone

import morning_brief as mb

ROME = {"tz": "Europe/Rome", "user_state": "active"}


def utc(s):
    return datetime.fromisoformat(s).replace(tzinfo=timezone.utc).timestamp()


def put_settings(root, **data):
    d = root / "meta"
    d.mkdir(exist_ok=True)
    (d / "settings.json").write_text(json.dumps({"version": 3, **data}))


def alerts(state, when, root):
    w = mb.step(state, utc(when), str(root))
    return [] if w is None else [{"window": w}]


def test_fires_once_at_or_after_time_on_a_weekday(tmp_path):
    put_settings(tmp_path, **ROME)
    state = {}
    assert alerts(state, "2026-10-21T05:29:59", tmp_path) == []          # Wed 07:29:59 Rome
    out = alerts(state, "2026-10-21T05:31:00", tmp_path)                 # first tick past 07:30
    assert out[0]["window"] == {"date": "2026-10-21", "start": "2026-10-20T05:30:00Z", "end": "2026-10-21T05:31:00Z",
                                "time": "07:30", "tz": "Europe/Rome", "warnings": []}
    assert alerts(state, "2026-10-21T05:32:00", tmp_path) == []          # same day: no second fire
    assert alerts(state, "2026-10-22T05:30:00", tmp_path)[0]["window"]["date"] == "2026-10-22"


def test_weekend_is_skipped_with_weekdays_but_daily_fires(tmp_path):
    put_settings(tmp_path, **ROME)
    assert alerts({}, "2026-10-24T05:45:00", tmp_path) == []             # Sat
    assert alerts({}, "2026-10-25T06:45:00", tmp_path) == []             # Sun (CET from today)
    put_settings(tmp_path, **ROME, morning_brief={"days": "daily"})
    assert len(alerts({}, "2026-10-24T05:45:00", tmp_path)) == 1


def test_monday_window_starts_friday_across_the_dst_change(tmp_path):
    put_settings(tmp_path, **ROME)
    out = alerts({}, "2026-10-26T06:40:00", tmp_path)                    # Mon 07:40 CET
    assert out[0]["window"]["start"] == "2026-10-23T05:30:00Z"           # Fri 07:30 CEST
    assert out[0]["window"]["end"] == "2026-10-26T06:40:00Z"
    tue = alerts({}, "2026-10-27T06:30:00", tmp_path)
    assert tue[0]["window"]["start"] == "2026-10-26T06:30:00Z"           # Tuesday starts Monday's brief time


def test_no_double_fire_across_restart(tmp_path):
    put_settings(tmp_path, **ROME)
    state = {}
    assert len(alerts(state, "2026-10-21T05:31:00", tmp_path)) == 1
    reloaded = json.loads(json.dumps(state))                             # poll-state.json round trip
    assert reloaded["morning_brief"]["last"] == "2026-10-21"
    assert alerts(reloaded, "2026-10-21T06:00:00", tmp_path) == []
    assert len(alerts(reloaded, "2026-10-22T05:31:00", tmp_path)) == 1


def test_poller_down_fires_up_to_four_hours_late_else_skips_the_day(tmp_path):
    put_settings(tmp_path, **ROME)
    late = {}
    assert len(alerts(late, "2026-10-21T09:30:00", tmp_path)) == 1       # exactly 4 h late
    skipped = {}
    assert alerts(skipped, "2026-10-21T09:30:01", tmp_path) == []
    assert alerts(skipped, "2026-10-21T15:00:00", tmp_path) == []
    assert "morning_brief" not in skipped or skipped["morning_brief"].get("last") is None
    assert len(alerts(skipped, "2026-10-22T05:30:00", tmp_path)) == 1    # next day fires normally


def test_disabled_never_fires(tmp_path):
    put_settings(tmp_path, **ROME, morning_brief={"enabled": False})
    assert alerts({}, "2026-10-21T05:31:00", tmp_path) == []


def test_custom_tz_time_and_days_are_honoured(tmp_path):
    put_settings(tmp_path, **ROME, morning_brief={"time": "06:00", "days": ["mon", "wed"], "tz": "America/New_York"})
    assert alerts({}, "2026-10-21T09:59:00", tmp_path) == []             # Wed 05:59 EDT
    out = alerts({}, "2026-10-21T10:00:00", tmp_path)                    # Wed 06:00 EDT = 10:00Z
    assert out[0]["window"]["tz"] == "America/New_York" and out[0]["window"]["time"] == "06:00"
    assert out[0]["window"]["start"] == "2026-10-19T10:00:00Z"           # previous configured day: Monday
    assert alerts({}, "2026-10-22T10:30:00", tmp_path) == []             # Thu not configured


def test_dst_spring_forward_keeps_wall_clock_time(tmp_path):
    put_settings(tmp_path, tz="America/New_York", morning_brief={"days": "daily"})
    out = alerts({}, "2026-03-09T11:31:00", tmp_path)                    # Mon 07:31 EDT
    assert out[0]["window"]["start"] == "2026-03-08T11:30:00Z"           # Sun 07:30 EDT, 24 h wall clock
    sun = alerts({}, "2026-03-08T11:31:00", tmp_path)
    assert sun[0]["window"]["start"] == "2026-03-07T12:30:00Z"           # Sat 07:30 EST: a 23 h window


def test_absent_settings_use_standard_defaults_with_host_zone(tmp_path, monkeypatch):
    monkeypatch.setenv("TZ", "Asia/Tokyo")
    cfg = mb.resolve(None)
    assert (cfg["enabled"], cfg["time"], str(cfg["tz"]), cfg["warnings"]) == (True, "07:30", "Asia/Tokyo", [])
    assert cfg["days"] == {0, 1, 2, 3, 4}
    out = alerts({}, "2026-10-20T22:31:00", tmp_path)                    # Wed 07:31 JST, no settings doc
    assert out[0]["window"]["tz"] == "Asia/Tokyo"


def test_settings_tz_beats_host_zone_and_bad_values_fall_back_with_warnings(monkeypatch):
    monkeypatch.setenv("TZ", "Asia/Tokyo")
    cfg = mb.resolve({"tz": "Europe/Rome", "morning_brief": {"enabled": "yes", "time": "7:5", "days": "someday", "tz": "Nope/Zone"}})
    assert (cfg["enabled"], cfg["time"], str(cfg["tz"])) == (True, "07:30", "Europe/Rome")
    assert cfg["days"] == {0, 1, 2, 3, 4}
    assert len(cfg["warnings"]) == 4

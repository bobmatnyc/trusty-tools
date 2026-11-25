"""Reminders attached to Intent Ledger tasks (#10571): il_remind's pure functions and the chat ack parse.

#10571 S2: the datg verbs, the ilio `remind:` field, the poller's `reminder` alert, the dashboard row and the migration
cases of the pin's test_il_remind.py and test_il_remind_ack.py ship with the datg and poller re-merge (S3, S4).
Test: python3 -m pytest -q tests/test_il_remind.py
"""
from datetime import datetime, timedelta, timezone

import pytest

import il_remind

UTC = timezone.utc
T0 = datetime(2026, 11, 24, 12, 0, tzinfo=UTC)


# ---- il_remind (pure) -------------------------------------------------------

@pytest.mark.parametrize("raw,want", [("2026-11-24", "2026-11-24T12:00Z"), ("2026-11-24T09:30Z", "2026-11-24T09:30Z"),
                                      ("2026-11-24T09:30:45Z", "2026-11-24T09:30Z")])
def test_parse_at_forms(raw, want):
    assert il_remind.parse_at(raw) == want


@pytest.mark.parametrize("bad", ["", "tomorrow", "2026-13-01", "2026-11-24 09:30"])
def test_parse_at_rejects(bad):
    with pytest.raises(il_remind.RemindError):
        il_remind.parse_at(bad)


@pytest.mark.parametrize("bad", ["14", "0d", "d14", "14m", "1h30"])
def test_parse_every_rejects(bad):
    with pytest.raises(il_remind.RemindError):
        il_remind.parse_every(bad)


def test_one_shot_fires_once_then_waits_for_done():
    t = {"id": "ex-T01", "status": "open"}
    r, created = il_remind.add(t, "2026-11-24", "note")
    assert created and r["state"] == "pending" and r["n"] == 1
    st = {"tasks": [t]}
    assert il_remind.due(st, T0 - timedelta(minutes=1)) == []
    assert [x[1]["n"] for x in il_remind.due(st, T0)] == [1]
    il_remind.fire(r, T0)
    assert r["state"] == "fired" and r["fired_at"] == "2026-11-24T12:00Z"
    assert il_remind.due(st, T0 + timedelta(days=30)) == []   # fired: never due again
    assert il_remind.next_reminder(t) is r                    # still shown on the task until done
    il_remind.mark_done(t, 1)
    assert il_remind.next_reminder(t) is None


def test_repeat_reschedules_past_now_in_whole_steps():
    t = {"id": "ex-T01", "status": "open"}
    r, _ = il_remind.add(t, "2026-11-24", "again", every="14d")
    il_remind.fire(r, T0 + timedelta(days=30))   # the poller was down for a month
    assert r["state"] == "fired" and r["at"] == "2026-11-24T12:00Z" and r["fires"] == 1   # a fired one waits for its ack
    assert il_remind.due({"tasks": [t]}, T0 + timedelta(days=30)) == []
    il_remind.mark_done(t, 1, now=T0 + timedelta(days=30))   # the ack: 12-08 and 12-22 are behind now (12-24)
    assert r["state"] == "pending" and r["at"] == "2027-01-05T12:00Z"


def test_add_is_idempotent_and_capped():
    t = {"id": "ex-T01", "status": "open"}
    r1, c1 = il_remind.add(t, "2026-11-24", "same")
    r2, c2 = il_remind.add(t, "2026-11-24", "same")
    assert (c1, c2, r1 is r2) == (True, False, True)
    for i in range(7):
        il_remind.add(t, "2026-11-25", f"n{i}")
    with pytest.raises(il_remind.RemindError):
        il_remind.add(t, "2026-11-26", "one too many")
    with pytest.raises(il_remind.RemindError):
        il_remind.add({"id": "ex-T09", "status": "done"}, "2026-11-24", "x")
    with pytest.raises(il_remind.RemindError):
        il_remind.add({"id": "ex-T09", "status": "open"}, "2026-11-24", "x" * 161)


def test_next_reminder_prefers_fired_then_earliest():
    t = {"id": "ex-T01", "status": "open"}
    a, _ = il_remind.add(t, "2026-12-01", "later")
    b, _ = il_remind.add(t, "2026-11-30", "sooner")
    assert il_remind.next_reminder(t) is b
    il_remind.fire(a, T0 + timedelta(days=30))
    assert il_remind.next_reminder(t) is a


# ---- chat ack --------------------------------------------------------------

@pytest.mark.parametrize("text,want", [
    ("ack ex-T84#2", [("ex-T84", 2)]),
    ("Ack ex-T84#2, ex-T6e#1  ex-T01#10", [("ex-T84", 2), ("ex-T6e", 1), ("ex-T01", 10)]),
    ("ack all", "all"),
    ("thanks for the update", None),
    ("acknowledge ex-T84#2", None),
])
def test_parse_ack(text, want):
    assert il_remind.parse_ack(text) == want


def test_parse_ack_rejects_words_that_are_not_ids():
    with pytest.raises(il_remind.RemindError):
        il_remind.parse_ack("ack the sweep")

"""Run windows on tasks: run_progress helper, schema acceptance/rejection, tally "(k running)".

Test: scripts/test_dashboard_run.py
"""
import importlib.util
import json
import os
import sys
from datetime import datetime, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_derive as derive  # noqa: E402
import dashboard_schema as sch  # noqa: E402

spec = importlib.util.spec_from_file_location("dashboard_db", os.path.join(SCRIPTS, "dashboard-db.py"))
db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db)

T = "2026-10-11T04:00:00Z"
ITEM = {"kind": "task", "status": "open", "title": "t", "session": "s", "goal": "G01", "rank": 1, "created": T, "updated": T}
WIN = {"run_start": "2026-10-12T03:45Z", "run_end": "2026-10-19T23:59Z"}


def at(s):
    return datetime.strptime(s, "%Y-%m-%dT%H:%MZ").replace(tzinfo=timezone.utc)


def test_progress_none_without_window():
    assert derive.run_progress(dict(ITEM), at("2026-10-12T05:00Z")) is None
    assert derive.run_progress(dict(ITEM, run_start=WIN["run_start"]), at("2026-10-12T05:00Z")) is None


def test_progress_day_of_days_and_running():
    p = derive.run_progress(dict(ITEM, **WIN), at("2026-10-14T10:00Z"))
    assert (p["day"], p["days"], p["ended"], p["next_run"]) == (3, 8, False, None)
    assert p["running"] is True


def test_progress_before_start_and_after_end():
    p = derive.run_progress(dict(ITEM, **WIN), at("2026-10-11T10:00Z"))
    assert (p["day"], p["ended"], p["running"]) == (0, False, False)
    late = at("2026-10-20T00:30Z")
    p = derive.run_progress(dict(ITEM, **WIN), late)
    assert (p["day"], p["days"], p["ended"], p["running"]) == (8, 8, True, False)
    assert derive.run_progress(dict(ITEM, status="done", **WIN), late)["ended"] is False


def test_progress_next_run_from_schedule():
    it = dict(ITEM, run_schedule=["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-16"], **WIN)
    assert derive.run_progress(it, at("2026-10-13T12:00Z"))["next_run"] == "2026-10-13"  # today's run counts
    assert derive.run_progress(it, at("2026-10-15T12:00Z"))["next_run"] == "2026-10-16"
    assert derive.run_progress(it, at("2026-10-17T12:00Z"))["next_run"] is None


def test_next_run_skips_dates_already_logged():
    it = dict(ITEM, run_schedule=["2026-10-12", "2026-10-13"], run_log=[{"at": "2026-10-12T03:46:00Z", "note": "run 1"}], **WIN)
    assert derive.run_progress(it, at("2026-10-12T05:00Z"))["next_run"] == "2026-10-13"
    it["run_log"].append({"at": "2026-10-13T04:00Z"})
    assert derive.run_progress(it, at("2026-10-12T05:00Z"))["next_run"] is None


def test_progress_accepts_seconds_and_rejects_garbage():
    p = derive.run_progress(dict(ITEM, run_start="2026-10-12T03:45:00Z", run_end="2026-10-13T00:00:00Z"), at("2026-10-12T05:00Z"))
    assert (p["day"], p["days"]) == (1, 2)
    assert derive.run_progress(dict(ITEM, run_start="soon", run_end="later"), at("2026-10-12T05:00Z")) is None


CASES = [
    ({"run_start": "2026-10-12T03:45Z", "run_end": "2026-10-19T23:59Z"}, False),
    ({"run_start": "2026-10-12T03:45:10Z", "run_end": "2026-10-19"}, False),
    ({"run_schedule": ["2026-10-12", "2026-10-13"], "run_log": [{"at": "2026-10-12T03:45Z", "note": "run 1", "duration_s": 120}]}, False),
    ({"run_start": "tomorrow"}, True),
    ({"run_end": "2026-13-45T99:99Z"}, True),
    ({"run_schedule": ["10/12/2026"]}, True),
    ({"run_log": [{"at": "yesterday"}]}, True),
    ({"run_log": [{"at": "2026-10-12T03:45Z", "duration_s": -1}]}, True),
    ({"run_log": [{"at": "2026-10-12T03:45Z", "bogus": 1}]}, True),
]


@pytest.mark.parametrize("extra,bad", CASES)
def test_schema_run_fields(extra, bad):
    errs = sch.validate("items", "T01", {**ITEM, **extra}) if hasattr(sch, "validate") else None
    assert bool(errs) is bad, errs


def put(root, doc_id, **data):
    d = root / "items"
    d.mkdir(exist_ok=True)
    (d / f"{doc_id}.json").write_text(json.dumps(dict(data, version=1)))


def test_tally_counts_running(tmp_path, capsys, monkeypatch):
    now = datetime.now(timezone.utc)
    f = lambda d: d.strftime("%Y-%m-%dT%H:%MZ")  # noqa: E731
    from datetime import timedelta
    put(tmp_path, "T01", kind="task", status="open", run_start=f(now - timedelta(days=1)), run_end=f(now + timedelta(days=1)))
    put(tmp_path, "T02", kind="task", status="open", run_start=f(now + timedelta(days=1)), run_end=f(now + timedelta(days=2)))
    put(tmp_path, "T03", kind="task", status="done", run_start=f(now - timedelta(days=1)), run_end=f(now + timedelta(days=1)))
    put(tmp_path, "T04", kind="task", status="open")
    assert db.main(["--root", str(tmp_path), "tally"]) == 0
    assert capsys.readouterr().out == "3 open dashboard items: 0 questions, 0 actions, 3 tasks (1 running)\n"
    assert db.main(["--root", str(tmp_path), "tally", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["running"] == 1

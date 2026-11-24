"""Architect context bar 500k vs PM 350k (owner D8f 2026-11-12), per consuming script.

#10571 S1 ships ctx_bar.py and token_watch.py, so their checks are here. The ctx-watch,
alert_triage and fleet-poll checks port with those scripts in S3, and the pin's
committed-schedule check read an instance file (local/il-schedule.json) the bundle does not ship.
"""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S1: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")


def _mod(name, f):
    s = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, f))
    m = importlib.util.module_from_spec(s)
    s.loader.exec_module(m)
    return m


cb = _mod("cb", "ctx_bar.py")
tw = _mod("tw", "token_watch.py")
B = [200_000, 315_000, 350_000, 400_000]


def _sched(tmp_path, key=500_000):
    p = tmp_path / "s.json"
    p.write_text(json.dumps({"thresholds": {"architect_bar_ctx": key} if key else {}}))
    return str(p)


def test_ctx_bar_levels(tmp_path):
    p = _sched(tmp_path)
    assert cb.levels("tm-architect", p) == {"bar": 500_000, "approach": 450_000, "guard": 550_000}
    assert cb.levels("tm-x-01", p) == {"bar": 350_000, "approach": 315_000, "guard": 400_000}
    assert cb.bar_pct("tm-architect", schedule_path=p) == 50 and cb.bar_pct("tm-x-01", schedule_path=p) == 35
    assert cb.warn_pct("tm-architect", schedule_path=p) == 45 and cb.warn_pct("tm-x-01", schedule_path=p) == 31
    assert cb.levels("tm-architect", _sched(tmp_path, None))["bar"] == 350_000  # key absent: default
    assert cb.levels("tm-architect", str(tmp_path / "missing.json"))["bar"] == 350_000


def test_architect_session_name_comes_from_the_environment(tmp_path, monkeypatch):
    """#10571 S1: the Architect's session name is ARCHITECT_SESSION (default tm-architect), never a constant."""
    p = _sched(tmp_path)
    monkeypatch.setenv("ARCHITECT_SESSION", "tm-arch2")
    renamed = _mod("cb_env", "ctx_bar.py")
    assert renamed.levels("tm-arch2", p)["bar"] == 500_000
    assert renamed.levels("tm-architect", p)["bar"] == 350_000
    assert cb.ARCHITECT_SESSION == "tm-architect"


def test_token_watch_buckets(tmp_path, monkeypatch):
    monkeypatch.setenv("IL_SCHEDULE", _sched(tmp_path))
    assert tw.ctx_bar.main_buckets("tm-architect", B) == [200_000, 450_000, 500_000, 550_000]
    assert tw.ctx_bar.main_buckets("tm-x-01", B) == B

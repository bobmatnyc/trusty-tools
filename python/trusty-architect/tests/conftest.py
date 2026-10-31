"""Tests never write the live dashboard store: fleet-poll's exited step reads DASHBOARD_ROOT."""
import os
import sys

import pytest

# #10161: modules live in scripts/ beside tests/ (shipped) or beside this file (prototype)
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))


@pytest.fixture(autouse=True)
def _scratch_dashboard_root(tmp_path_factory, monkeypatch):
    monkeypatch.setenv("DASHBOARD_ROOT", str(tmp_path_factory.mktemp("dashroot")))


@pytest.fixture(autouse=True)
def _no_triage_model_call(monkeypatch):
    """Cycle tests never reach the Haiku gate; the triage tests inject their own runner."""
    monkeypatch.setenv("TRIAGE_HAIKU", "off")


@pytest.fixture(autouse=True)
def _triage_log_in_tmp(tmp_path, monkeypatch):
    """No test appends to the live inbox/triage.jsonl; the guard test fails if one does."""
    monkeypatch.setenv("TRIAGE_LOG", str(tmp_path / "triage.jsonl"))


@pytest.fixture(autouse=True)
def _il_scheduler_isolated(tmp_path, monkeypatch):
    """The poller's IL steps (il-cron, ctx-watch) never read the live schedule or write live state."""
    monkeypatch.setenv("IL_SCHEDULE", str(tmp_path / "no-il-schedule.json"))
    monkeypatch.setenv("IL_CRON_STATE", str(tmp_path / "il-cron-state.json"))
    monkeypatch.setenv("CTX_PROXY_LOG", str(tmp_path / "ctx-proxy.jsonl"))
    monkeypatch.setenv("CTX_CYCLE_JSONL", str(tmp_path / "ctx-cycle.jsonl"))


@pytest.fixture(autouse=True)
def _no_caller_architect_session(monkeypatch):
    """The caller's ARCHITECT_SESSION never renames the Architect in a test (#10161); a test sets its own."""
    monkeypatch.delenv("ARCHITECT_SESSION", raising=False)


@pytest.fixture(autouse=True)
def _wake_alert_in_tmp(tmp_path, monkeypatch):
    """No test writes or clears the live inbox/pm-wake-alert.json (D39)."""
    monkeypatch.setenv("PM_WAKE_ALERT", str(tmp_path / "pm-wake-alert.json"))


@pytest.fixture(autouse=True)
def _no_live_project_registry(tmp_path_factory, monkeypatch):
    """#10571: the Projects view never reads the operator's live tm registry; a test that wants one writes its own."""
    monkeypatch.setenv("TRUSTY_MPM_ROOT", str(tmp_path_factory.mktemp("mpmroot")))

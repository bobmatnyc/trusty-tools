"""Tests never write the live dashboard store: fleet-poll's exited step reads DASHBOARD_ROOT."""
import os
import tempfile

import pytest

# #8436: a fresh checkout has no inbox/, and no test writes the checkout's own one.
# Set before any test module imports fleet-poll.py, which reads these at import.
_INBOX = tempfile.mkdtemp(prefix="architect-test-inbox-")
os.environ.setdefault("ARCHITECT_INBOX_DIR", _INBOX)
os.environ.setdefault("SUPERVISOR_INBOX_DIR", _INBOX)


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

"""tests/ gets the same isolation as scripts/conftest.py: a fleet-poll cycle never
writes the live dashboard store or inbox/triage.jsonl and never calls the Haiku
triage gate (#8436: tests/ runs in CI beside scripts/)."""
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
    monkeypatch.setenv("TRIAGE_HAIKU", "off")


@pytest.fixture(autouse=True)
def _triage_log_in_tmp(tmp_path, monkeypatch):
    monkeypatch.setenv("TRIAGE_LOG", str(tmp_path / "triage.jsonl"))

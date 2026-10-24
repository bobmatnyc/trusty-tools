"""dashboard_store guards the shipped tests cover in trusty-tools (#10161): the live-store refusal and timestamp normalising.

The prototype covers both in tests that need scripts this sub-project does not ship (the fleet poller's exited step and
the store migrator); these two keep the store's Test: pointers resolving.
Test: tests/test_dashboard_store_guards.py
"""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import dashboard_store as ds  # noqa: E402


def test_store_refuses_live_root_under_pytest():
    with pytest.raises(ds.StoreError):
        ds.Store(ds.DEFAULT_ROOT).set("sessions", "S99", {"name": "x"})
    assert not os.path.exists(os.path.join(ds.DEFAULT_ROOT, "sessions", "S99.json"))


def test_minute_timestamps_gain_seconds_in_timestamp_fields_only():
    doc = {"created": "2026-10-12T03:46Z", "done_at": "2026-10-12T03:47Z", "title": "2026-10-12T03:46Z",
           "updates": [{"at": "2026-10-12T03:48Z", "text": "2026-10-12T03:48Z"}]}
    assert ds.normalize_timestamps(doc) == {
        "created": "2026-10-12T03:46:00Z", "done_at": "2026-10-12T03:47:00Z", "title": "2026-10-12T03:46Z",
        "updates": [{"at": "2026-10-12T03:48:00Z", "text": "2026-10-12T03:48Z"}]}

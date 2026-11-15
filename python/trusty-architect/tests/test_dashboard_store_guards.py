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


@pytest.mark.parametrize("fmt", ds.FORMATS)
def test_store_stamps_created_and_updated_and_ignores_client_values(tmp_path, fmt):
    """#10678: a client-supplied created/updated never reaches the store; event fields (done_at) stay the client's."""
    ticks = iter(["2026-11-02T06:00:00Z", "2026-11-02T06:05:00Z", "2026-11-02T06:10:00Z"])
    store = ds.Store(str(tmp_path / "q"), fmt=fmt, schema_mode="off", clock=lambda: next(ticks))
    bogus = "1999-01-01T00:00:00Z"
    store.set("docs", "a", {"title": "t", "created": bogus, "updated": "2099-12-31T23:59:59Z", "done_at": bogus})
    first = store.get("docs", "a")["data"]
    assert (first["created"], first["updated"], first["done_at"]) == ("2026-11-02T06:00:00Z", "2026-11-02T06:00:00Z", bogus)

    store.update("docs", "a", {"created": "2099-12-31T23:59:59Z", "updated": bogus})
    second = store.get("docs", "a")["data"]
    assert (second["created"], second["updated"]) == ("2026-11-02T06:00:00Z", "2026-11-02T06:05:00Z")

    store.update("docs", "a", {"title": "t2"})   # no stamp sent: the store still records the write time
    assert store.get("docs", "a")["data"]["updated"] == "2026-11-02T06:10:00Z"

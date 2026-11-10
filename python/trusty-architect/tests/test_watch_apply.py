"""watch-apply.py: the owner's Watch / Unwatch requests reach `tm fleet project add|remove` once per key (#10896).

Why: the CLI is the only writer of local/watch.json (epic #10894 D6); the applier must record the CLI's real outcome.
A stub `tm` (TM_BIN) logs each call and exits with the code the test asks for.
Test: scripts/watch-apply.py
"""
import importlib.util
import json
import os
import stat

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("watch_apply", os.path.join(SCRIPTS, "watch-apply.py"))
wa = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wa)
import dashboard_store as ds  # noqa: E402

STUB = """#!/bin/sh
printf '%s\\n' "$*" >> "{log}"
printf '%s' "$STUB_STDOUT"
printf '%s' "$STUB_STDERR" >&2
exit "${{STUB_EXIT:-0}}"
"""


@pytest.fixture
def world(tmp_path, monkeypatch):
    """A store root, an Architect dir and a stub tm on TM_BIN; returns (store, dir, calls())."""
    root, arch = tmp_path / "queue", tmp_path / "arch"
    (arch / "local").mkdir(parents=True)
    log = tmp_path / "calls.log"
    stub = tmp_path / "tm"
    stub.write_text(STUB.format(log=log))
    stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("TM_BIN", str(stub))
    for name in ("STUB_EXIT", "STUB_STDOUT", "STUB_STDERR"):
        monkeypatch.delenv(name, raising=False)

    def calls():
        return log.read_text().splitlines() if log.exists() else []
    return ds.Store(str(root)), arch, calls


def put(store, doc_id, key, action, created="2026-11-10T10:00:00Z"):
    store.set(wa.COLL, doc_id, {"key": key, "action": action, "status": "new", "created": created, "by": "owner"})


def run(store, arch):
    return wa.main(["--root", store.root, "--dir", str(arch)])


def data(store, doc_id):
    return store.get(wa.COLL, doc_id)["data"]


def test_nonzero_exit_marks_failed_with_the_cli_stderr(world, monkeypatch):
    store, arch, calls = world
    put(store, "w1", "proj-a", "watch")
    monkeypatch.setenv("STUB_EXIT", "1")
    monkeypatch.setenv("STUB_STDERR", "Error: proj-a is not a registered project")
    assert run(store, arch) == 1
    d = data(store, "w1")
    assert d["status"] == "failed", d
    assert "proj-a is not a registered project" in d["message"] and d["done_at"]
    assert calls() == [f"fleet project add --dir {arch} -- proj-a"]


def test_zero_exit_marks_applied_with_the_cli_stdout(world, monkeypatch):
    store, arch, calls = world
    put(store, "w1", "proj-a", "watch")
    monkeypatch.setenv("STUB_STDOUT", "added proj-a to the watch list\n")
    assert run(store, arch) == 0
    assert data(store, "w1")["status"] == "applied"
    assert data(store, "w1")["message"] == "added proj-a to the watch list"
    assert run(store, arch) == 0 and len(calls()) == 1      # an applied doc is not run again


def test_two_new_docs_for_one_key_make_one_cli_call_and_the_newest_decides(world):
    store, arch, calls = world
    put(store, "w1", "proj-a", "unwatch", created="2026-11-10T10:00:00Z")
    put(store, "w2", "proj-a", "watch", created="2026-11-10T10:05:00Z")
    assert run(store, arch) == 0
    assert calls() == [f"fleet project add --dir {arch} -- proj-a"]
    assert data(store, "w1")["status"] == data(store, "w2")["status"] == "applied"


@pytest.mark.parametrize("key,action", [("-rf", "watch"), ("--dir=/tmp", "unwatch"), ("a/b", "watch"),
                                        (".hidden", "watch"), ("proj-a", "delete")])
def test_bad_key_or_action_is_refused_without_a_cli_call(world, key, action):
    store, arch, calls = world
    put(store, "w1", key, action)
    assert run(store, arch) == 1
    d = data(store, "w1")
    assert d["status"] == "failed" and ("refused key" in d["message"] or "unknown action" in d["message"])
    assert calls() == []


def test_unwatch_of_a_key_absent_from_watch_json_is_applied_without_a_cli_call(world):
    store, arch, calls = world
    (arch / "local" / "watch.json").write_text(json.dumps({"version": 1, "projects": [{"key": "other"}]}))
    put(store, "w1", "proj-a", "unwatch")
    assert run(store, arch) == 0
    d = data(store, "w1")
    assert (d["status"], d["message"]) == ("applied", "already not watched")
    assert calls() == []


def test_missing_binary_marks_failed(world, monkeypatch, tmp_path):
    store, arch, _ = world
    monkeypatch.setenv("TM_BIN", str(tmp_path / "no-such-tm"))
    put(store, "w1", "proj-a", "watch")
    assert run(store, arch) == 1
    assert data(store, "w1")["status"] == "failed" and "not found" in data(store, "w1")["message"]


def test_long_stderr_is_capped(world, monkeypatch):
    store, arch, _ = world
    put(store, "w1", "proj-a", "watch")
    monkeypatch.setenv("STUB_EXIT", "3")
    monkeypatch.setenv("STUB_STDERR", "x" * 5000)
    run(store, arch)
    assert len(data(store, "w1")["message"]) == wa.MESSAGE_MAX


def test_applier_never_runs_list_and_written_docs_validate(world):
    store, arch, calls = world
    put(store, "w1", "proj-a", "watch")
    put(store, "w2", "proj-b", "unwatch")
    run(store, arch)
    assert all(" list" not in c for c in calls())
    import dashboard_schema as sch
    for doc_id in ("w1", "w2"):
        assert sch.validate(wa.COLL, doc_id, data(store, doc_id)) == []

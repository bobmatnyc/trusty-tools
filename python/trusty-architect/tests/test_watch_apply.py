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
[ -n "$STUB_SLEEP" ] && exec sleep "$STUB_SLEEP"
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
    for name in ("STUB_EXIT", "STUB_STDOUT", "STUB_STDERR", "STUB_SLEEP", "DASHBOARD_FORMAT", "DASHBOARD_SCHEMA"):
        monkeypatch.delenv(name, raising=False)

    def calls():
        return log.read_text().splitlines() if log.exists() else []
    return ds.Store(str(root)), arch, calls


def put(store, doc_id, key, action, created="2026-11-10T10:00:00Z"):
    store.set(wa.COLL, doc_id, {"key": key, "action": action, "status": "new", "created": created, "by": "owner"})


def run(store, arch, *extra):
    return wa.main(["--root", store.root, "--dir", str(arch), *extra])


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
                                        (".hidden", "watch"), ("proj-a", "delete"),
                                        ("proj-a\n", "watch")])
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


def test_a_doc_changed_by_another_writer_keeps_that_content_and_is_reported(world, monkeypatch, capsys):
    store, arch, _ = world
    put(store, "w1", "proj-a", "watch")
    theirs = {"key": "proj-a", "action": "watch", "status": "dismissed", "created": "2026-11-10T10:00:00Z",
              "by": "owner"}
    real = wa.run_cli

    def racing_cli(*args):   # the owner dismisses the request while the CLI runs
        store.set(wa.COLL, "w1", theirs)
        return real(*args)
    monkeypatch.setattr(wa, "run_cli", racing_cli)
    assert run(store, arch) == 0
    assert data(store, "w1") == theirs
    err = capsys.readouterr().err
    assert "w1" in err and "another writer" in err, err


def test_an_outcome_the_store_refuses_leaves_the_doc_new_and_exits_2(world, monkeypatch):
    store, arch, calls = world
    # A doc the enforcing schema rejects (an unknown property), written past the check.
    ds.Store(store.root, fmt="jsonl", schema_mode="off").set(wa.COLL, "w1", {
        "key": "proj-a", "action": "watch", "status": "new", "created": "2026-11-10T10:00:00Z", "by": "owner",
        "note": "not in the schema"})
    monkeypatch.setenv("DASHBOARD_FORMAT", "jsonl")
    monkeypatch.setenv("DASHBOARD_SCHEMA", "enforce")
    assert run(store, arch) == 2
    assert ds.Store(store.root, fmt="jsonl").get(wa.COLL, "w1")["data"]["status"] == "new"
    assert calls() == [f"fleet project add --dir {arch} -- proj-a"]


@pytest.mark.parametrize("watch_json", [None, "{not json", '{"version": 2, "projects": []}'])
def test_unwatch_without_a_readable_watch_json_asks_the_cli(world, watch_json):
    store, arch, calls = world
    if watch_json is not None:
        (arch / "local" / "watch.json").write_text(watch_json)
    put(store, "w1", "proj-a", "unwatch")
    assert run(store, arch) == 0
    assert calls() == [f"fleet project remove --dir {arch} -- proj-a"]
    assert data(store, "w1")["status"] == "applied"


def test_a_cli_call_past_the_timeout_marks_failed(world, monkeypatch):
    store, arch, _ = world
    put(store, "w1", "proj-a", "watch")
    monkeypatch.setenv("STUB_SLEEP", "5")
    assert run(store, arch, "--timeout", "1") == 1
    d = data(store, "w1")
    assert d["status"] == "failed" and "timed out after 1s" in d["message"], d

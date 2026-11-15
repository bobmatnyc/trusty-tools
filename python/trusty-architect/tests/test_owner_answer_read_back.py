"""#10680: the Architect reads a saved owner answer back from the store before acting on it, and never fails open.

Run: python3 -m pytest tests/test_owner_answer_read_back.py
Test: tests/test_owner_answer_read_back.py
"""
import importlib.util
import json
import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_store as ds  # noqa: E402
import owner_answers as oa  # noqa: E402

spec = importlib.util.spec_from_file_location("pending_answers", os.path.join(SCRIPTS, "pending-answers.py"))
pa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pa)

LONG = "Approve the rollout, but hold the second region until the canary has run for a full day without errors."


def answer(root, doc_id, text, at, if_version=None):
    return ds.Store(str(root)).set("items", doc_id, {"kind": "question", "status": "answered", "title": "Rollout?",
                                                     "answer": text, "answered_at": at}, if_version=if_version)


def listed(root, doc_id):
    return next(e for e in oa.pending(str(root), colls=("items",)) if e["id"] == doc_id)


def read_back_cli(root, entry, *extra):
    return pa.main(["--root", str(root), "--read-back", entry["id"], "--kind", entry["kind"], "--at", entry["at"], *extra])


def test_read_back_returns_the_stored_answer_and_version_not_the_value_in_hand(tmp_path, capsys):
    answer(tmp_path, "D29", LONG, "2026-11-01T20:00:00Z")
    entry = listed(tmp_path, "D29")
    hook_line_text = entry["text"][:60]   # the hook shows the Architect 60 characters
    got = oa.read_back(str(tmp_path), "D29", entry["kind"], entry["at"])
    assert got["text"] == LONG and got["text"] != hook_line_text
    assert got["version"] == 1
    assert read_back_cli(tmp_path, entry) == 0
    assert capsys.readouterr().out == f"D29\tanswered\t2026-11-01T20:00:00Z\tv1\t{LONG}\n"


def test_answer_saved_again_after_the_listing_blocks_the_action(tmp_path, capsys):
    answer(tmp_path, "D29", "yes", "2026-11-01T20:00:00Z")
    stale = listed(tmp_path, "D29")
    answer(tmp_path, "D29", "no, wait for the audit", "2026-11-01T20:05:00Z", if_version=1)   # saved during the pass
    with pytest.raises(oa.ReadBackError) as err:
        oa.read_back(str(tmp_path), "D29", stale["kind"], stale["at"])
    assert err.value.reason == "mismatch"
    assert read_back_cli(tmp_path, stale) == 3
    out = capsys.readouterr()
    assert out.out == "" and "do not act" in out.err
    fresh = listed(tmp_path, "D29")
    assert oa.read_back(str(tmp_path), "D29", fresh["kind"], fresh["at"])["text"] == "no, wait for the audit"


@pytest.mark.parametrize("case", ["relayed", "text_differs", "missing", "unreadable"])
def test_failed_or_mismatched_read_back_blocks_the_action(tmp_path, capsys, case):
    answer(tmp_path, "D29", "yes", "2026-11-01T20:00:00Z")
    entry = listed(tmp_path, "D29")
    extra = ()
    if case == "relayed":
        ds.Store(str(tmp_path)).update("items", "D29", {"relayed": "2026-11-01T20:01:00Z tm-apex"}, if_version=1)
    elif case == "text_differs":
        extra = ("--text", "no")
    elif case == "missing":
        ds.Store(str(tmp_path)).delete("items", "D29", if_version=1)
    else:
        (tmp_path / "items" / "D29.json").write_text("{not json")
    code = read_back_cli(tmp_path, entry, *extra)
    out = capsys.readouterr()
    assert code == (2 if case == "unreadable" else 3), out.err
    assert out.out == "" and "read-back refused, do not act" in out.err
    with pytest.raises(oa.ReadBackError) as err:
        oa.read_back(str(tmp_path), "D29", entry["kind"], entry["at"], *extra[1:])
    assert err.value.reason == ("unreadable" if case == "unreadable" else "mismatch")


def test_read_back_needs_kind_and_at(tmp_path, capsys):
    answer(tmp_path, "D29", "yes", "2026-11-01T20:00:00Z")
    assert pa.main(["--root", str(tmp_path), "--read-back", "D29"]) == 2
    assert capsys.readouterr().out == ""


def test_answer_saved_after_the_read_back_stays_pending_and_is_acted_on(tmp_path):
    """Acceptance (a): the relayed write is pinned to the read-back version, so a later save is not marked relayed."""
    answer(tmp_path, "D29", "yes", "2026-11-01T20:00:00Z")
    entry = listed(tmp_path, "D29")
    got = oa.read_back(str(tmp_path), "D29", entry["kind"], entry["at"])
    answer(tmp_path, "D29", "no, wait for the audit", "2026-11-01T20:05:00Z", if_version=got["version"])
    with pytest.raises(ds.Conflict):
        ds.Store(str(tmp_path)).update("items", "D29", {"relayed": "2026-11-01T20:06:00Z tm-apex"}, if_version=got["version"])
    fresh = listed(tmp_path, "D29")
    assert fresh["text"] == "no, wait for the audit"
    assert oa.read_back(str(tmp_path), "D29", fresh["kind"], fresh["at"])["text"] == "no, wait for the audit"

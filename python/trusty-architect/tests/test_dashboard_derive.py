"""dashboard_derive: pure derived values, golden output over the fixture store, and the no-direct-reads gate.

Why: one rule per derived value (the owner 2026-10-11 04:12Z, deterministic data); callers must not read store files themselves.
Test: scripts/test_dashboard_derive.py
"""
import json
import os
import re
import sys
from datetime import datetime, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
sys.path.insert(0, SCRIPTS)
import dashboard_derive as dv  # noqa: E402
import dashboard_store as ds  # noqa: E402

FIX = os.path.join(HERE, "fixtures", "dashboard")
NOW = datetime(2026, 10, 11, 12, 0, tzinfo=timezone.utc)


def doc(i, **d):
    return {"id": i, "version": 1, "data": d}


@pytest.fixture
def snap():
    st = ds.Store(os.path.join(FIX, "golden"), fmt="jsonl", schema_mode="off")
    return {c: st.list_docs(c) for c in ("items", "sessions", "goals", "effort_tunes", "priorities")}


def test_tally_open_excludes_parked_and_counts_it_apart():
    items = [doc("D01", kind="question", status="open"),
             doc("D02", kind="question", status="open", parked={"until": "2026-10-12T00:00:00Z", "choice": "tomorrow"}),
             doc("D03", kind="question", status="open", parked={"until": None, "choice": "clarify"}),
             doc("D04", kind="question", status="open", parked={"until": None, "choice": "owner"}),   # page: not parked
             doc("A01", kind="action", status="open"), doc("T01", kind="task", status="open"),
             doc("D05", kind="question", status="answered")]
    assert dv.tally(items) == {"open": 4, "questions": 2, "actions": 1, "tasks": 1, "answered_unrelayed": 1, "parked": 2}


def test_next_id_wraps_and_free_count_and_retire():
    ids = [doc(f"D{n:02x}", status="done", done_at=f"2026-01-{n % 28 + 1:02d}") for n in range(0, 256)]
    snap = {"items": ids[:254] + [doc("A00", status="open")]}
    assert dv.next_id(snap, "D") == "D" + "fe"          # 254 D ids used (00-fd); highest fd, next fe
    assert dv.free_count(snap, "D") == 2
    full = {"items": ids}
    assert dv.next_id(full, "D") is None
    assert len(dv.retire_candidates(full, "D")) == 64
    assert dv.next_id({"items": [doc("D05")]}, "D") == "D06" and dv.next_id({}, "G") == "G00"
    assert dv.next_id({"items": [doc("Dff")]}, "D") == "D00"                      # wraps past ff
    assert dv.next_id({"items": [doc("A3")]}, "A") == "A00"                      # legacy ids are not in the pattern


def test_stale_tasks_use_kind_not_id_prefix():
    items = [doc("T01", kind="task", status="open", updated="2026-10-01T00:00:00Z"),
             doc("T02", kind="action", status="open", updated="2026-10-01T00:00:00Z"),    # T id, not a task
             doc("D09", kind="task", status="open", updated="2026-10-10T23:00:00Z"),
             doc("D10", kind="task", status="open", updated="2026-10-01T00:00:00Z", parked={"until": None, "choice": "stale"})]
    assert [r[1] for r in dv.stale_tasks(items, NOW)] == ["T01"]


def test_goal_sessions_links_both_ways_and_skips_parked_and_unknown():
    goals = [doc("G01", lifecycle="active", sessions=["tm-a", "tm-gone"]), doc("G02", lifecycle="parked", sessions=["tm-a"]),
             doc("G03", lifecycle="active", sessions=["tm-a", "tm-b"])]
    sessions = [doc("S01", name="tm-a"), doc("S02", name="tm-b")]
    assert dv.goal_sessions(goals, sessions) == ({"G01": ["tm-a"], "G03": ["tm-a", "tm-b"]}, {"tm-a": ["G01", "G03"], "tm-b": ["G03"]})


def test_ordered_by_rank_then_id():
    docs = [doc("B", rank=2), doc("A", rank=2), doc("Z", rank=1), doc("N")]
    assert [d["id"] for d in dv.ordered(docs)] == ["Z", "A", "B", "N"]


def test_golden_derived_values(snap):
    got = {"tally": dv.tally(snap["items"]), "pending": dv.pending(snap, NOW),
           "stale": [[r[0].isoformat() if r[0] else None, *r[1:]] for r in dv.stale_tasks(snap["items"], NOW)],
           "next": {p: dv.next_id(snap, p) for p in "ADGST"},
           "goal_sessions": dv.goal_sessions(snap["goals"], snap["sessions"])}
    path = os.path.join(FIX, "golden", "derive.json")
    text = json.dumps(got, sort_keys=True, indent=1, ensure_ascii=False) + "\n"
    if os.environ.get("UPDATE_GOLDEN"):
        open(path, "w").write(text)
    assert text == open(path).read()


def test_no_script_reads_store_files_directly():
    """Only the store, the jsonl codec, the migrator and the server may list or glob a store root."""
    allowed = {"dashboard_store.py", "dashboard_jsonl.py", "dashboard-migrate.py", "dashboard-serve.py"}
    pat = re.compile(r"(os\.listdir|glob\.glob|os\.scandir)\([^\n]*(DEFAULT_ROOT|args\.root|store\.root|cdir|coll|owner-queue)")
    bad = []
    for n in sorted(os.listdir(SCRIPTS)):
        if n.endswith((".py", ".sh")) and not n.startswith("test_") and n not in allowed:
            bad += [f"{n}: {l.strip()}" for l in open(os.path.join(SCRIPTS, n), encoding="utf-8") if pat.search(l)]
    assert bad == []

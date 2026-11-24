"""il_timeline: which ledger task a project was on at a given time (ar-T5h).

#10571 S2: the pin has no case for il_timeline.py (its consumer dashboard_tasks.py ships in S4); these cover the
module's own contract: open-to-done activity, last-touched wins, a parked task never wins, a named task breaks a
tie, a re-opened id is a new incarnation, and repo_datg_dirs finds ledgers one and two levels down.
Test: python3 -m pytest -q tests/test_il_timeline.py
"""
import json
import os

import il_timeline as tl

T = 1_800_000_000  # a whole minute; events below are minutes after it


def iso(minute):
    from datetime import datetime, timezone
    return datetime.fromtimestamp(T + 60 * minute, timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


def ledger(d, events, tasks=(), session=None):
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "log.jsonl"), "w") as f:
        for minute, tid, ev in events:
            f.write(json.dumps({"t": iso(minute), "id": tid, "ev": ev}) + "\n")
        f.write("not json\n")
    with open(os.path.join(d, "state.json"), "w") as f:
        json.dump({"tasks": list(tasks), "session": session or {}}, f)
    return d


def test_last_touched_open_task_is_current_until_done(tmp_path):
    d = ledger(str(tmp_path / "datg"), [(0, "ar-T01", "open"), (5, "ar-T02", "open"), (10, "ar-T01", "update"),
                                        (20, "ar-T01", "done"), (30, "ar-T02", "done")],
               tasks=[{"id": "ar-T01", "title": "first"}])
    t = tl.load({"p": d})
    assert t.at("p", T - 1) is None                       # before any event
    assert t.at("p", T + 60 * 6)[0] == "ar-T02"           # T02 opened last
    assert t.at("p", T + 60 * 11)[0] == "ar-T01"          # T01 touched again
    assert t.at("p", T + 60 * 21)[0] == "ar-T02"          # T01 done: T02 is left
    assert t.at("p", T + 60 * 31) is None                 # nothing active
    key = t.newest["ar-T01"]
    assert t.inc[key] == {"id": "ar-T01", "open": T, "close": T + 60 * 20, "title": "first"}
    assert t.at("other", T) is None


def test_parked_never_wins_and_a_named_task_breaks_a_minute_tie(tmp_path):
    d = ledger(str(tmp_path / "datg"), [(0, "ar-T01", "open"), (0, "ar-T02", "open"), (3, "ar-T03", "open")],
               tasks=[{"id": "ar-T03", "status": "parked"}], session={"task": "on ar-T01 now"})
    t = tl.load({"p": d})
    assert t.at("p", T)[0] == "ar-T01"                    # same minute: the session names T01
    assert t.at("p", T + 60 * 4)[0] == "ar-T01"           # T03 is newer but parked


def test_reopened_id_is_a_new_incarnation(tmp_path):
    d = ledger(str(tmp_path / "datg"), [(0, "ar-T01", "open"), (1, "ar-T01", "done"), (5, "ar-T01", "open")])
    t = tl.load({"p": d})
    first, second = t.at("p", T), t.at("p", T + 60 * 6)
    assert first == ("ar-T01", T) and second == ("ar-T01", T + 300) and t.newest["ar-T01"] == second


def test_repo_datg_dirs_finds_one_and_two_level_checkouts(tmp_path):
    a = tmp_path / "repo-a" / ".trusty-mpm" / "datg"
    b = tmp_path / "owner" / "repo-b" / ".trusty-mpm" / "datg"
    a.mkdir(parents=True)
    b.mkdir(parents=True)
    out = tl.repo_datg_dirs(str(tmp_path), "arch-dir", "/il")
    assert sorted(out.values()) == sorted([str(a), str(b), "/il"])
    assert out["arch-dir"] == "/il"

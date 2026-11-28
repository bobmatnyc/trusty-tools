"""Replay driver for dashboard_derive.py (#12029 P2b): runs a list of cases against the library.

Why: the module has no command line; every caller passes it document lists and an explicit `now`. This file is
copied beside the frozen module (and ids36.py) at record time as dd_driver.py. It reads CASES.json, a list of
{"op": ...} objects, and prints `case <i> <op>` then the answer as one json.dumps line (ensure_ascii off, keys in
the function's own order): a datetime prints as isoformat(), a tuple as a list, a set as a sorted list. `now` is an
ISO string read with datetime.fromisoformat; `retire` without `now` reads the clock, which the recorder freezes.
examples/replay_port.rs mirrors it as `replay_port dashboard_derive`.
Usage: driver.py CASES.json
"""
import json
import os
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_derive as dd  # noqa: E402

PREDICATES = {"building_brief": dd.building_brief, "is_parked": dd.is_parked, "is_relayed": dd.is_relayed}


def conv(x):
    if isinstance(x, datetime):
        return x.isoformat()
    if isinstance(x, (list, tuple)):
        return [conv(v) for v in x]
    if isinstance(x, set):
        return sorted(x)
    if isinstance(x, dict):
        return {k: conv(v) for k, v in x.items()}
    return x


def answer(c):
    op = c["op"]
    now = datetime.fromisoformat(c["now"]) if "now" in c else None
    hours = {"hours": c["hours"]} if "hours" in c else {}
    if op in PREDICATES:
        return [PREDICATES[op](d) for d in c["data"]]
    if op == "is_selftest":
        return [dd.is_selftest(doc) for doc in c["docs"]]
    if op == "decision_counts":
        return dd.decision_counts(c["items"])
    if op == "tally":
        return dd.tally(c["items"])
    if op == "run_progress":
        return [dd.run_progress(d, now) for d in c["data"]]
    if op == "running_count":
        return dd.running_count(c["items"], now)
    if op == "used_ids":
        return dd.used_ids(c["snap"], c["prefix"])
    if op == "free_count":
        return dd.free_count(c["snap"], c["prefix"])
    if op == "next_id":
        return dd.next_id(c["snap"], c["prefix"], c.get("last_issued"))
    if op == "retire":
        kw = {k: c[k] for k in ("keep_free", "age_days") if k in c}
        return dd.retire_candidates(c["snap"], c["prefix"], now=now, **kw)
    if op == "kept_ids":
        return dd.kept_ids(c["snap"], c["prefix"])
    if op in ("last_activity", "last_progress", "acted_on"):
        return [getattr(dd, op)(d) for d in c["data"]]
    if op == "stamp_progress":
        return dd.stamp_progress(c.get("old"), c["new"], now)
    if op == "stale_tasks":
        return dd.stale_tasks(c["items"], now, **hours)
    if op == "stale_parked":
        return dd.stale_parked(c["items"], now, **({"days": c["days"]} if "days" in c else {}))
    if op == "stale_ranked":
        return dd.stale_ranked(c["items"], c["goals"], now, **hours)
    if op == "goal_sessions":
        return dd.goal_sessions(c["goals"], c["sessions"])
    if op == "ordered":
        return [doc["id"] for doc in dd.ordered(c["docs"])]
    if op == "by_acted_on":
        return [doc["id"] for doc in dd.by_acted_on(c["docs"])]
    if op == "pending":
        return dd.pending(c["snap"], now)
    if op == "request_queue":
        return dd.request_queue(c["snap"], now)
    raise KeyError(op)


def main(argv):
    if len(argv) != 1:
        sys.stderr.write("usage: driver.py CASES.json\n")
        return 2
    with open(argv[0], encoding="utf-8") as f:
        cases = json.load(f)
    for i, case in enumerate(cases):
        print(f"case {i} {case['op']}")
        print("  " + json.dumps(conv(answer(case)), ensure_ascii=False))
    return 0


sys.exit(main(sys.argv[1:]))

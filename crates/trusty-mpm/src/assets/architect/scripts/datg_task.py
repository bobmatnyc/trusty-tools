"""datg_task.py: the Task-ID dispatch check and the WIP cap of an Intent Ledger (ar-Tf5, D09).

Why: #11101 upstreams the prototype's Task gate into the split datg modules. A dispatch carries
`Task: <id>@MMDD` for an open task, and a ledger keeps at most `wip_cap` active tasks, so work is
finished, parked or folded before more starts (the owner 2026-11-10).
What: ACTIVE statuses, wip_cap/wip_count/wip_check (warn into WARNINGS, or raise in refuse mode),
task_mmdd (the @stamp of a task's `created`), task_mode and task_problem (why a prompt's Task token is
unusable). Misses go to inbox/task-compliance.jsonl through task_tag.log_miss. datg.py re-exports
every name.
Test: tests/test_datg.py TaskTagCase
"""
import re

import task_tag
from datg_store import DatgError

WIP_CAP_DEFAULT = 8   # the owner 2026-11-10: 8 active tasks per Architect/PM ledger; config.wip_cap overrides per ledger
ACTIVE = ("open", "running")
WARNINGS = []         # messages `add T` collected for the CLI to print on stderr


def wip_cap(st):
    cap = (st.get("config") or {}).get("wip_cap")
    return cap if isinstance(cap, int) and not isinstance(cap, bool) and cap > 0 else WIP_CAP_DEFAULT


def wip_count(st):
    """Active tasks: open or running; parked and done do not count."""
    return sum(1 for t in st["tasks"] if t.get("status") in ACTIVE)


def wip_check(st, new_id):
    """Adding one more task over the cap: warn (default) or, in refuse mode, raise. The new task is not yet in st."""
    n, cap = wip_count(st) + 1, wip_cap(st)
    if n <= cap:
        return
    msg = f"{new_id} would make {n} active tasks (cap {cap}); finish, park or fold one first"
    task_tag.log_miss("wip", msg, st["code"])
    if task_mode(st) == "refuse":
        raise DatgError(msg)
    WARNINGS.append("warning: " + msg)


def task_mmdd(task):
    """MMDD of a task's creation (the @stamp of its tag), or None for a task made before `created` existed."""
    c = str(task.get("created") or "")
    return c[5:7] + c[8:10] if re.match(r"^\d{4}-\d{2}-\d{2}", c) else None


def task_mode(st):
    """warn (default) or refuse for the Task check: config.task_mode, $TASK_TAG_MODE, local/task-mode."""
    return task_tag.mode((st.get("config") or {}).get("task_mode"))


def task_problem(st, prompt):
    """Why the prompt's Task token is unusable, or None. Own-ledger ids must be live and match their @MMDD stamp;
    a foreign id and a dashboard id are accepted on format alone. Pure."""
    specs = task_tag.find_tasks(prompt)
    if not specs:
        ex = next((t for t in st["tasks"] if t["status"] in ACTIVE), None)
        eg = f" (e.g. {ex['id']}@{task_mmdd(ex)})" if ex and task_mmdd(ex) else (f" (e.g. {ex['id']})" if ex else "")
        return f"names no task id. Add 'Task: <id>' for an open task{eg}, "
    for sp in specs:
        if sp["kind"] != "il" or not sp["id"].startswith(st["code"] + "-"):
            continue
        t = next((x for x in st["tasks"] if x["id"] == sp["id"]), None)
        if t is None or t.get("status") == "done":
            return f"names unknown or closed task {sp['id']}"
        mmdd = task_mmdd(t)
        if sp["stamp"] and mmdd and sp["stamp"] != mmdd:
            return f"names {sp['id']}@{sp['stamp']} but that id was created @{mmdd} (a reused id?)"
    return None

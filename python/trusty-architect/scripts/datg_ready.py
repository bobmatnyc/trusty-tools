"""datg_ready.py: whether an Intent Ledger is ready for a context clear (`datg.py ready`, #10579).

Why: a clear may only run when the ledger holds the work a fresh session resumes from. A HOLD, or a fresh write
with no task, is not that, and self-clear's `il_ok` (#10599) must be able to tell.
What: `op_ready(project)` returns (exit code, reasons, info). Ready (0) needs every one of: SESSION task and next set
and not placeholders; at least one active goal or open/running task; `owed` and `crons` recorded (an empty list,
"none", counts as recorded); and `fresh` true (the same window and stamp `datg.py fresh` uses). A HOLD never counts.
1 = not ready, with one plain-English reason per failed rule. 2 = the ledger is missing or unreadable.
Test: tests/test_datg_ready.py
"""
from datg_read import op_fresh
from datg_store import VAGUE, DatgError, load_state, paths


NEEDS_INPUT = "needs owner input:"   # df-D5f: the marker tm fleet init seeds when state.md names no task or next


def _placeholder(value):
    v = str(value or "").strip()
    return len(v) < 3 or v.lower() in VAGUE or v.lower().startswith(NEEDS_INPUT)


def op_ready(project, since=None, max_age=None):
    """(code, reasons, info) for `project`'s ledger; see the module doc."""
    import os
    if not os.path.exists(paths(project)[1]):
        return 2, ["the Intent Ledger is missing; run datg.py init"], {}
    try:
        st = load_state(project)
    except DatgError as e:
        return 2, [f"the Intent Ledger cannot be read ({e})"], {}
    ses = st.get("session") or {}
    reasons = []
    for key, what in (("task", "what is being worked on"), ("next", "the next step")):
        if _placeholder(ses.get(key)):
            reasons.append(f"no {key} recorded ({what}). Set the current task and next step: run "
                           f"python3 scripts/datg.py --project {project} session set --task \"...\" --next \"...\"")
    open_goals = [g for g in st.get("goals") or [] if g.get("status") == "active"]
    open_tasks = [t for t in st.get("tasks") or [] if t.get("status") in ("open", "running")]
    if not open_goals and not open_tasks:
        reasons.append("no open goal or task item: there is no saved work to resume")   # stable text, asserted by #10599
    if "owed" not in ses and not ses.get("owed_none"):
        reasons.append("what is owed is not recorded; set it with datg.py session set --owe CHECK=EXPECT@REF or --owe none")
    if "crons" not in ses:
        reasons.append("the scheduled jobs are not recorded; set them with datg.py session set --cron ... or --cron none")
    code, msg = op_fresh(project, since, max_age)
    if code == 2:
        return 2, [f"the Intent Ledger cannot be read ({msg})"], {}
    if code != 0:
        reasons.append(f"the ledger was not written after the latest change ({msg})")
    info = {"written_at": ses.get("written_at"), "hold": ses.get("hold") or ""}
    return (0 if not reasons else 1), reasons, info

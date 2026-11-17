"""Architect context band for the Live page: dashboard_archctx.architect_ctx (IL ar-Te8 follow-up, the owner 2026-10-24 ~15:15Z).

Why: PM sessions carry ctx_k from inbox/dashboard-feed.json, but tm-architect is skipped by the poller, so the Live page's Architect
circle had no context band. The Architect's context comes from the same measurement the self-clear cycle uses (scripts/self-ctx.py).
What: architect_ctx(projects_dir, root) -> {ctx_k, approach_k, bar_k}: ctx_k = last assistant usage (input + cache read + cache creation) of the
newest interactive transcript under projects_dir/<encoded root>, in thousands (null when unmeasurable); approach_k and bar_k come from
<root>/local/il-schedule.json "thresholds" approach_ctx and bar_ctx (null when absent).
`tasks` [{id, title, state}] (same shape as the PM rows) = the Architect's own open/running IL tasks from <root>/local/il/.trusty-mpm/datg/state.json,
state "open" for both, titles cut to 80 chars, at most 60 rows; [] when the ledger is absent. Read-only; never raises.
Test: scripts/test_dashboard_harness.py (TestArchitectCtx)
"""
import importlib.util
import json
import os

_HERE = os.path.dirname(os.path.abspath(__file__))
_mod = None


def _self_ctx():
    """self-ctx.py has a hyphen in its name, so it is loaded by path."""
    global _mod
    if _mod is None:
        spec = importlib.util.spec_from_file_location("self_ctx", os.path.join(_HERE, "self-ctx.py"))
        _mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(_mod)
    return _mod


def _k(n):
    return round(n / 1000, 1) if isinstance(n, (int, float)) and n > 0 else None


TASK_MAX, TITLE_MAX = 60, 80


def architect_tasks(root):
    try:
        with open(os.path.join(root, "local", "il", ".trusty-mpm", "datg", "state.json"), encoding="utf-8") as f:
            rows = json.load(f).get("tasks") or []
    except (OSError, ValueError, AttributeError):
        return []
    out = []
    for t in rows:
        if isinstance(t, dict) and t.get("status") in ("open", "running"):
            title = " ".join(str(t.get("title") or "").split())
            out.append({"id": str(t.get("id") or ""), "title": title[:TITLE_MAX], "state": "open"})
    return out[:TASK_MAX]


def architect_ctx(projects_dir, root):
    out = {"ctx_k": None, "approach_k": None, "bar_k": None, "tasks": []}
    if not root:
        return out
    out["tasks"] = architect_tasks(root)
    try:
        with open(os.path.join(root, "local", "il-schedule.json"), encoding="utf-8") as f:
            th = (json.load(f).get("thresholds") or {})
        out["approach_k"], out["bar_k"] = _k(th.get("approach_ctx")), _k(th.get("bar_ctx"))
    except (OSError, ValueError, AttributeError):
        pass
    try:
        sc = _self_ctx()
        # #10571: Claude's rule (every non-alphanumeric byte, "_" too), as self-ctx.py and the harness encode it
        out["ctx_k"] = _k(sc.self_ctx_tokens(os.path.join(projects_dir, sc.project_slug(root))))
    except Exception:   # a measurement failure must not break the snapshot; null = unknown
        pass
    return out

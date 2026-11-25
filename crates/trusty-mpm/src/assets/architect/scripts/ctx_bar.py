"""ctx_bar.py: the one place that knows the Architect's context bar differs from the PMs'.

Why: the owner (D8f, 2026-11-12 15:40Z) raised the bar for the Architect session only
to 450k tokens; PMs keep the 350k bar. One key, `thresholds.architect_bar_ctx` in
local/il-schedule.json, drives every watcher; absent key = every session uses the PM default. The Architect session is
ARCHITECT_SESSION, default tm-architect (session names stay env-only, #8436).
What: pure lookups. "Approaching" is 90% of the bar; the compaction guard is 110% of it.
Test: scripts/test_ctx_bar.py and the per-script bar tests.
"""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
ARCHITECT_SESSION = os.environ.get("ARCHITECT_SESSION") or "tm-architect"  # #10571 S1: env, not a constant
PM_BAR = 350_000
PM_APPROACH = 315_000
PM_GUARD = 400_000
WINDOW = 1_000_000
KEY = "architect_bar_ctx"


def architect_bar(schedule_path=None):
    """The Architect bar in tokens from il-schedule.json, or None when unset/unreadable."""
    path = schedule_path or os.environ.get("IL_SCHEDULE") or os.path.join(ROOT, "local", "il-schedule.json")
    try:
        with open(path) as f:
            v = (json.load(f).get("thresholds") or {}).get(KEY)
    except (OSError, ValueError, AttributeError):
        return None
    return v if isinstance(v, int) and not isinstance(v, bool) and v > 0 else None


def levels_for_bar(b):
    """{'bar','approach','guard'} for a bar of `b` tokens: approach 90%, guard 110%."""
    return {"bar": b, "approach": b * 9 // 10, "guard": b * 11 // 10}


def levels(session, schedule_path=None):
    """{'bar','approach','guard'} in tokens for `session`."""
    b = architect_bar(schedule_path) if session == ARCHITECT_SESSION else None
    if b is None:
        return {"bar": PM_BAR, "approach": PM_APPROACH, "guard": PM_GUARD}
    return levels_for_bar(b)


def bar_pct(session, window=WINDOW, schedule_path=None):
    """The bar as a whole percent of the context window."""
    return int(100 * levels(session, schedule_path)["bar"] / window)


def warn_pct(session, window=WINDOW, schedule_path=None):
    """The 'approaching' line as a whole percent of the window."""
    return int(100 * levels(session, schedule_path)["approach"] / window)


def main_buckets(session, buckets, schedule_path=None):
    """token_watch ctx_main buckets with the PM approach/bar/guard values swapped for `session`'s."""
    lv = levels(session, schedule_path)
    swap = {PM_APPROACH: lv["approach"], PM_BAR: lv["bar"], PM_GUARD: lv["guard"]}
    return sorted({swap.get(b, b) for b in buckets})

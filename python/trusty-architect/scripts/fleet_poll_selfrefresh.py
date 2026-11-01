"""fleet_poll_selfrefresh.py — the poller's self-refresh step: a stub, #10599 PR-1's slot (#10571 Phase 2).

Why: #10599 owns the Architect's self-refresh (start self-clear.sh when its own context passes the bar). The
poller's cycle already calls this step once a cycle, so that PR changes this file only.
What: self_refresh_step(state, panes, now, dry_run) returns "": nothing starts and the poll.log line is
unchanged. fleet-poll.py calls it after the wake, unless the wake typed into the Architect this cycle.
bind() receives fleet-poll.py's late-bound view, for the step #10599 writes.
Test: tests/test_fleet_poll_split.py
"""
P = None  # fleet-poll.py's late-bound view; set by bind()


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def self_refresh_step(state, panes, now, dry_run):
    """The log phrase of this cycle's self-refresh; "" (nothing ran) until #10599 lands."""
    return ""

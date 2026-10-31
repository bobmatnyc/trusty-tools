"""fleet_poll_alerts.py — fleet-poll.py's per-cycle alert steps beyond the panes (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap; its alert steps move here unchanged, and later steps land here.
What: resolve_context_bar (the context bar, each warning logged once per process), self_ctx_alerts (the
Architect's own context against that bar), il_cron_alerts (il-cron.py's due cadence jobs) and
ctx_watch_alerts (ctx-watch.py's clear candidates). Names from the poller are read through `P`, the late-bound view
fleet-poll.py passes to bind(), so rebinding a fleet-poll name still steers these, as before the split.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports), tests/test_fleet_poll_watchdog_wiring.py
"""
P = None  # fleet-poll.py's late-bound view; set by bind()


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def resolve_context_bar(dry_run=False):
    """(bar_tokens, window_tokens) from fleet-classify's context_bar(), logging
    each warning once per process (#9997): an invalid value falls back to the
    default and says so; it never switches the alert off."""
    bar, window, warnings = P.fleet_classify.context_bar()
    for w in warnings:
        if w not in P._LOGGED_BAR_WARNINGS:
            P._LOGGED_BAR_WARNINGS.add(w)
            P.log(f"WARNING {w}", dry_run)
    return bar, window


def self_ctx_alerts(now, dry_run=False):
    """One "self_ctx" alert when the Architect's own context (self_ctx.py,
    tailing its transcript) is at/over the context bar, in tokens; no window
    is involved (#9997). A missing or unreadable transcript logs and yields
    no alert; self_ctx.py never raises for that."""
    bar, _ = P.resolve_context_bar(dry_run)
    try:
        tokens = P.self_ctx.self_ctx_tokens(P.SELF_CTX_DIR)
    except (OSError, ValueError, TypeError) as e:
        P.log(f"self_ctx ERROR {type(e).__name__}: {e}", dry_run)
        return []
    if tokens is None or tokens < bar:
        return []
    return [P.alert("self_ctx", str(bar), f"self ctx {tokens} tokens (bar {bar} tokens)",
                    P.ARCHITECT, now=now)]


def il_cron_alerts(now, dry_run=False, step=None):
    """Intent Ledger cadence jobs (il-cron.py, SPEC-IL-01 section 4), hourly inside (prototype :567-575).
    A failure is logged and yields no alerts: it must never break the poll loop.
    Test: test_il_cron_alerts_map_each_finding, test_a_failing_il_step_logs_and_yields_nothing"""
    try:
        return [P.alert("il_cron", fp_, detail, now=now)
                for fp_, detail in (step or P.il_cron.step)(now=now, dry_run=dry_run)]
    except Exception as e:
        P.log(f"il-cron ERROR {type(e).__name__}: {e}", dry_run)
        return []


def ctx_watch_alerts(now, dry_run=False, step=None):
    """Clear-signal candidates S1-S5 (ctx-watch.py) as `ctxwatch` alerts (prototype :578-589).
    Sends no keys; a dry run writes no state. Never raises.
    Test: test_ctx_watch_alerts_map_each_candidate, test_a_failing_il_step_logs_and_yields_nothing"""
    out = []
    try:
        for c in (step or P.ctx_watch.step)(now=now, write=not dry_run):
            out.append(P.alert("ctxwatch", c["fingerprint"], f"{c['signal']} {c['action']}: {c['detail']}",
                               session=c["session"], now=now))
    except Exception as e:
        P.log(f"ctx-watch ERROR {type(e).__name__}: {e}", dry_run)
    return out

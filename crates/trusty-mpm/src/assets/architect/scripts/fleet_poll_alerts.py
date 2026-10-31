"""fleet_poll_alerts.py — fleet-poll.py's per-cycle alert steps beyond the panes (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap; its alert steps move here unchanged, and later steps land here.
What: resolve_context_bar (the context bar, each warning logged once per process) and self_ctx_alerts (the
Architect's own context against that bar). Names from the poller are read through `P`, the late-bound view
fleet-poll.py passes to bind(), so rebinding a fleet-poll name still steers these, as before the split.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports)
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

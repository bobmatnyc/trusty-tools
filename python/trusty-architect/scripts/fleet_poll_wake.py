"""fleet_poll_wake.py — fleet-poll.py's wake: the one-line pointer typed into the Architect (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap; its wake half moves here unchanged.
What: _norm, _box_full_text, _own_pointer, wake and dispatch_wake. Names from the poller (capture,
send_to_architect, parse_pane, ARCHITECT, ...) are read through `P`, the late-bound view fleet-poll.py
passes to bind(), so rebinding `fleet-poll.capture` or `fleet-poll.wake` still steers them, as before.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports)
"""
import re
import time
from datetime import datetime, timezone

P = None  # fleet-poll.py's late-bound view; set by bind()


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def _norm(text):
    """Strip all whitespace (including the box's leading non-breaking space)
    so a message that wraps across screen rows compares equal to itself."""
    return re.sub(r"\s+", "", text.replace("\xa0", ""))


def _box_full_text(capture):
    """Normalized text of the whole input box: the ❯ row plus any wrapped
    continuation rows that carry no rule of their own. A narrow pane (the
    Architect's own is ~47 columns) wraps a typed message across several
    screen rows, and only the first carries the ❯ glyph input_state.classify()
    keys on — this reads the box find_box() bounds instead, so wrapping never
    makes a message that IS present look unconfirmed. "" if no box is found."""
    lines = P._screen_lines(capture)
    box = P.find_box(lines)
    if not box:
        return ""
    top, bottom = box
    return P._norm("".join(lines[top + 1:bottom]).replace("❯", ""))


def _own_pointer(text):
    """True if normalized box text looks like a wake() pointer this poller
    typed itself ('[poll HH:MMZ] N new alerts -> path'), including one split
    across wrapped rows. Never true for a human draft that merely starts with
    the same characters without the rest of the shape — a false positive here
    would delete a real draft, which wake() must never do."""
    return text.startswith("[poll") and "newalerts->" in text


def wake(n, panes, dry_run):
    """Type the pointer into the Architect's input box. Returns a log phrase.

    Why: 2026-09-27/28 poll.log — the Architect's pane is narrow enough that a
    typed pointer wraps across screen rows, so the old prefix-in-one-line
    confirmation failed even though the text WAS in the box, leaving that text
    sitting there for later cycles to trip over as "input typed" and, once,
    to get submitted merged with unrelated input.
    What: confirms against the whole box (wrap-tolerant); on a genuine
    confirmation failure, removes exactly the characters it just typed
    (BSpace x len(msg) — safe because the box was verified empty/suggestion
    immediately before typing, so nothing else can be there) and reports
    whether the cleanup left the box empty; and recognizes its own stale
    pointer left over from an earlier cycle with a distinct log reason instead
    of the generic "input typed", without ever deleting text it cannot
    positively identify as its own.
    Test: test_wake_confirms_wrapped_message_and_sends_enter,
    test_wake_cleans_up_its_own_text_when_confirmation_fails,
    test_wake_detects_its_own_stale_pointer_and_does_not_send
    """
    sup = sorted((p for p in panes if p["session"] == P.ARCHITECT),
                 key=lambda p: p["active"] != "11")
    if not sup:
        return f"skipped: no {P.ARCHITECT} pane"
    target = sup[0]["pane"]
    raw = P.capture(target)
    info = P.parse_pane(raw)
    if info["dialog"]:
        return "skipped: dialog open"
    if not info["has_box"]:
        # Usually the Architect busy on its own turn: Claude Code's TUI drops
        # the rule-bounded box find_box() requires while it works, even though
        # a stale "❯" line elsewhere can still make input_state.classify()
        # guess "empty"/"suggestion" — that guess is not this skip's real
        # cause, so it must never appear in the log in its place.
        return "skipped: no input box found"
    if info["box"] not in ("empty", "suggestion"):
        if P._own_pointer(P._box_full_text(raw)):
            return "skipped: own stale pointer in box"
        return f"skipped: input {info['box']}"
    msg = f"[poll {datetime.now(timezone.utc):%H:%M}Z] {n} new alerts -> {P.ALERTS}"
    if dry_run:
        return f"dry-run: would send to {target}: {msg}"
    P.send_to_architect(target, "-l", msg)
    time.sleep(0.5)
    if P._norm(msg) not in P._box_full_text(P.capture(target)):
        P.send_to_architect(target, *(["BSpace"] * len(msg)))
        time.sleep(0.3)
        empty = P.input_state.classify(P.capture(target))[0] in ("empty", "suggestion")
        return ("failed: text not confirmed in the input box; cleaned up, box empty"
                if empty else
                "failed: text not confirmed in the input box; cleanup left box non-empty")
    P.send_to_architect(target, "Enter")
    time.sleep(1)
    left = P.input_state.classify(P.capture(target))[1]
    return f"sent to {target}" + (" (still in box after Enter)" if msg[:40] in left else "")


def dispatch_wake(state, panes, new_count, dry_run):
    """Carry `new_count` into state["wake_pending"] and attempt one wake()
    when anything is pending; clears only on a confirmed send, so a skipped
    or failed attempt leaves the FULL total for the next cycle to retry.
    Returns the log phrase."""
    state["wake_pending"] = state.get("wake_pending", 0) + new_count
    if not state["wake_pending"]:
        return "none"
    try:
        result = P.wake(state["wake_pending"], panes, dry_run)
    except P.TmuxError as e:
        result = f"failed: {e}"
    if result.startswith("sent"):
        state["wake_pending"] = 0
    return result

"""fleet_poll_wake.py — fleet-poll.py's wake: the one-line pointer typed into the Architect (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap; its wake half moves here unchanged.
What: self_clear_running, _norm, _box_full_text, _own_pointer, wake and dispatch_wake. Names from the poller (capture,
send_to_architect, parse_pane, ARCHITECT, ...) are read through `P`, the late-bound view fleet-poll.py
passes to bind(), so rebinding `fleet-poll.capture` or `fleet-poll.wake` still steers them, as before.
Test: tests/test_fleet_poll.py (through fleet-poll.py's re-exports), tests/test_fleet_poll_owner_answers.py
"""
import fcntl
import os
import re
import subprocess
import time
from datetime import datetime, timezone

P = None  # fleet-poll.py's late-bound view; set by bind()


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def _pid_is_self_clear(pid):
    """True if `pid` is alive and its command line names self-clear.sh (prototype :1446)."""
    try:
        out = subprocess.run(["ps", "-p", str(int(pid)), "-o", "command="],
                             capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.SubprocessError, ValueError, TypeError):
        return False
    return "self-clear.sh" in out


def self_clear_running(arch_dir):
    """True while a self-clear.sh run may be typing into the Architect's box (#10571, #10599).

    Why: a wake Enter between self-clear's last idle check and its /clear puts /clear into a live turn.
    What: `<arch_dir>/inbox/self-clear.pid` held under flock, or naming a live self-clear.sh, is running;
    a pid file that cannot be opened, locked or read counts as running. With no pid file, any live
    process whose command line names scripts/self-clear.sh counts (the bundled script writes none yet).
    Test: test_wake_skips_while_self_clear_holds_its_lock, test_wake_proceeds_when_the_lock_is_free,
    test_an_unreadable_self_clear_lock_counts_as_held
    """
    path = os.path.join(arch_dir, "inbox", "self-clear.pid")
    if os.path.lexists(path):
        try:
            fd = os.open(path, os.O_RDONLY)
        except OSError:
            return True
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                return True                       # self-clear holds it
            fcntl.flock(fd, fcntl.LOCK_UN)
            raw = os.read(fd, 64).decode("ascii").strip()
            return _pid_is_self_clear(int(raw))   # a stale pid file is not a run
        except (OSError, ValueError, UnicodeDecodeError):
            return True
        finally:
            os.close(fd)
    try:
        return subprocess.run(["pgrep", "-f", "scripts/self-clear.sh"], capture_output=True,
                              timeout=5).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


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


def wake(n, panes, dry_run, defer_turn=False):
    """Type the pointer into the Architect's input box. Returns a log phrase.

    Why: 2026-09-27/28 poll.log — the Architect's pane is narrow enough that a
    typed pointer wraps across screen rows, so the old prefix-in-one-line
    confirmation failed even though the text WAS in the box, leaving that text
    sitting there for later cycles to trip over as "input typed" and, once,
    to get submitted merged with unrelated input.
    What: confirms against the whole box (wrap-tolerant). tmux has no atomic read-and-send,
    so a human can type between the empty-box read and the pointer landing (#10670). On a
    failed confirmation it re-reads the box: if the pointer is the last thing in it (or the
    box holds only part of it), it removes exactly what it typed (BSpace x len(msg)) and
    reports whether the box is empty; if the human's text follows the pointer, it removes
    nothing ("box holds other text"). It presses Enter only when the box holds exactly the pointer.
    Windows that remain: a key typed after the re-read and before BSpace or Enter is not
    seen (BSpace then removes that many of its characters; Enter submits the draft with the
    pointer), and a pointer with a draft after it stays in the box until the human clears
    it. It also recognizes its
    own stale pointer from an earlier cycle ("own stale pointer in box") and never deletes
    text it cannot positively identify as its own. A suggestion wakes only when every
    non-blank box cell is dim or inverse (the cursor); any other cell is "input typed". `defer_turn` holds the wake while a
    turn runs (owner_answer-only batches); other kinds keep the pre-#10670 timing.
    On a pane with no plain bottom rule under the ❯ it sends no keys in any box state
    and returns "skipped: no bottom rule ..."; the pending wake goes out once the rule
    returns. That is a stall, never a typed-over draft. One stranded-pointer window remains:
    a rule that vanishes in the 0.5s after typing makes the confirming read see the footer
    as box text, so the pointer stays in the box, unsubmitted, until a human clears it.
    Test: test_wake_confirms_a_message_that_wraps_across_screen_rows,
    test_wake_cleans_up_its_own_text_when_confirmation_fails,
    test_wake_detects_its_own_stale_pointer_and_does_not_send,
    test_a_busy_turn_keeps_the_answer_and_wakes_when_it_ends,
    test_a_draft_typed_as_the_wake_lands_is_kept_and_never_submitted,
    test_a_batch_with_another_kind_does_not_wait_for_a_running_turn,
    test_an_empty_box_with_no_bottom_rule_skips_and_sends_no_keys,
    test_a_draft_with_no_bottom_rule_is_still_never_touched,
    test_no_bottom_rule_sends_no_keys_in_any_box_state,
    test_the_pending_wake_is_delivered_once_the_bottom_rule_returns,
    test_a_wrapped_suggestion_under_a_plain_rule_still_wakes,
    test_a_dim_row_over_an_undimmed_draft_is_never_typed_beside,
    test_a_suggestion_wakes_only_when_the_whole_box_is_dim,
    test_a_rule_that_vanishes_after_typing_strands_the_pointer_without_enter
    """
    sup = sorted((p for p in panes if p["session"] == P.ARCHITECT),
                 key=lambda p: p["active"] != "11")
    if not sup:
        return f"skipped: no {P.ARCHITECT} pane"
    if P.self_clear_running(P.ROOT):   # #10599: never type while self-clear may send /clear
        return "skipped: self-clear running"
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
    # #10670: find_box() runs to the screen's end when no plain rule closes the box, so
    # nothing tells a suggestion or a draft from the footer: send no keys in any state.
    if P.find_box(rows := P._screen_lines(raw))[1] == len(rows):
        return "skipped: no bottom rule under the input box"
    # #10670: with `defer_turn`, a running turn holds the wake; background agents alone do not.
    if defer_turn and P.fleet_classify.ACTIVE.search(info["status"]):
        return "skipped: architect turn running"
    # #10670: classify() reads one SGR 2 on the ❯ row; a suggestion is a box whose every cell is dim.
    state = ("suggestion" if P.fleet_classify.box_is_suggestion(raw)
             else "typed" if info["box"] == "suggestion" else info["box"])
    if state not in ("empty", "suggestion"):
        if P._own_pointer(P._box_full_text(raw)):
            return "skipped: own stale pointer in box"
        return f"skipped: input {state}"
    # #10670: classify() reads only the ❯ row; a draft can sit on a later row.
    if state == "empty" and P._box_full_text(raw):
        return "skipped: input typed"
    msg = f"[poll {datetime.now(timezone.utc):%H:%M}Z] {n} new alerts -> {P.ALERTS}"
    if dry_run:
        return f"dry-run: would send to {target}: {msg}"
    P.send_to_architect(target, "-l", msg)
    time.sleep(0.5)
    box, want = P._box_full_text(P.capture(target)), P._norm(msg)
    if box != want:
        # #10670: a human typed as this landed. Enter would submit their draft with it,
        # and BSpace removes only this text when this text is the last thing in the box.
        if not (box.endswith(want) or want.startswith(box)):
            return "failed: box holds other text; left untouched"
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


def dispatch_wake(state, panes, new_count, dry_run, answer_count=0):
    """Carry `new_count` into state["wake_pending"] and attempt one wake()
    when anything is pending; clears only on a confirmed send, so a skipped
    or failed attempt leaves the FULL total for the next cycle to retry.
    A batch of only `owner_answer` alerts (`answer_count` of them, #10670) waits out a
    running turn; any other kind keeps its pre-#10670 timing. Returns the log phrase."""
    state["wake_pending"] = state.get("wake_pending", 0) + new_count
    state["wake_pending_answers"] = state.get("wake_pending_answers", 0) + answer_count
    if not state["wake_pending"]:
        return "none"
    only_answers = state["wake_pending_answers"] >= state["wake_pending"]
    try:
        if only_answers:
            result = P.wake(state["wake_pending"], panes, dry_run, defer_turn=True)
        else:
            result = P.wake(state["wake_pending"], panes, dry_run)
    except P.TmuxError as e:
        result = f"failed: {e}"
    if result.startswith("sent"):
        state["wake_pending"] = state["wake_pending_answers"] = 0
        if not dry_run:
            state["wake_sent_at"] = time.time()  # the watchdog's silent clock (prototype :1692)
    return result

#!/usr/bin/env python3
"""architect-watchdog.py — classify the Architect session and decide on a relaunch.

Why: the owner's ruling f4 (2026-10-04 17:2xZ) keeps the poller as the one process
that outlives the Architect and gives it a watchdog: a dead Architect session
is restarted from the latest snapshot, and a silent one is only reported.
Only one Claude session runs live; there is no idle shadow, and two
Architects would double-post relays.
What: pure decision logic for fleet-poll.py, with tmux, the clock, the
transcript mtime and the relauncher all passed in (no I/O here).
`classify` returns "dead" (tmux session missing, or every pane at a shell),
"silent" (alive, a wake sent more than `silent_minutes` ago and no transcript
write since) or "ok". `step` turns that into alert tuples
(kind, fingerprint, detail) and one log phrase. Dead: `architect_dead`, then
one relaunch at most per RELAUNCH_DEBOUNCE s, and after MAX_FAILURES failed
relaunches `architect_relaunch_failed` and no more attempts until the
Architect is seen alive. Silent: `architect_silent` only — never a relaunch,
never keys (an open AskUserQuestion dialog is the normal cause).
Book (state["watchdog"]): last_relaunch, failures, pending (a relaunch that
reported success and has not yet been seen alive; seeing the Architect dead
again counts it as a failure).
Frozen (the owner, 2026-10-04 23:0xZ, after the 20:19-21:30Z hang): the pane shows a
running turn (`busy`) AND either the newest transcript is older than
`frozen_min` (FROZEN_MIN, default 15) or the spinner text is byte-identical to
the previous poll's (book["spinner"]; the live spinner carries a timer, so a
stuck one repeats). It needs no wake. `architect_frozen` only: never a
relaunch, never keys.
Test: scripts/test_fleet_poll_watchdog.py
"""
import os

SILENT_MINUTES_DEFAULT = 45
FROZEN_MIN_DEFAULT = 15    # minutes of transcript silence under a running turn
RELAUNCH_DEBOUNCE = 1800   # seconds between relaunch attempts
MAX_FAILURES = 2           # failed relaunches before the watchdog gives up
RELAUNCH_TIMEOUT = 300     # seconds session-relaunch.sh may take


def silent_minutes():
    """SILENT_MINUTES from the environment, else the default."""
    try:
        return float(os.environ.get("SILENT_MINUTES", SILENT_MINUTES_DEFAULT))
    except ValueError:
        return float(SILENT_MINUTES_DEFAULT)


def frozen_minutes():
    """FROZEN_MIN from the environment, else the default."""
    try:
        return float(os.environ.get("FROZEN_MIN", FROZEN_MIN_DEFAULT))
    except ValueError:
        return float(FROZEN_MIN_DEFAULT)


def classify(panes, session, shells, wake_at, transcript_mtime, now, silent_min, *,
             busy=False, spinner="", prev_spinner=None, frozen_min=FROZEN_MIN_DEFAULT):
    """("dead"|"frozen"|"silent"|"ok", detail) for the Architect session.

    `panes` is list_panes() output; `shells` the shell command names;
    `wake_at` the epoch time of the last confirmed wake (None if none);
    `transcript_mtime` the newest interactive transcript's mtime (None if
    there is none). Silent needs a wake: with none outstanding, a quiet
    Architect is just idle. Frozen needs `busy` (a running turn): then a
    transcript older than `frozen_min` minutes, or a non-empty `spinner` equal
    to `prev_spinner`, means the UI is stuck.
    Test: test_classify_dead_when_session_missing,
    test_classify_dead_when_pane_at_shell, test_classify_silent_after_deadline,
    test_classify_ok_cases, test_classify_frozen_by_stale_transcript,
    test_classify_frozen_by_identical_spinner, test_classify_not_frozen_when_idle,
    test_classify_not_frozen_when_spinner_changes
    """
    mine = [p for p in panes if p["session"] == session]
    if not mine:
        return "dead", f"tmux session {session} is missing"
    if all(p["cmd"] in shells or p["cmd"].startswith("-") for p in mine):
        return "dead", f"pane runs {mine[0]['cmd']}, not claude"
    if busy and transcript_mtime is not None and now - transcript_mtime > frozen_min * 60:
        return "frozen", (f"running turn, no transcript write for "
                          f"{int((now - transcript_mtime) // 60)} min (limit {frozen_min:g})")
    if busy and spinner and spinner == prev_spinner:
        return "frozen", f"running turn, spinner text unchanged across polls: {spinner[:80]}"
    if wake_at and now - wake_at > silent_min * 60 and (
            transcript_mtime is None or transcript_mtime < wake_at):
        return "silent", (f"no transcript write for {int((now - wake_at) // 60)} min "
                          f"since the wake (limit {silent_min:g})")
    return "ok", ""


def step(book, panes, now, *, session, shells, wake_at, transcript_mtime, silent_min,
         relauncher, dry_run=False, busy=False, spinner="",
         frozen_min=FROZEN_MIN_DEFAULT):
    """(alerts, log phrase, relaunched) for one poll.

    `book` is the mutable watchdog state. `relauncher()` returns
    ("ok"|"failed"|"aborted", detail); "aborted" (a live process appeared)
    is not a failure. `alerts` are (kind, fingerprint, detail) tuples.
    Why: see the module docstring; the debounce stops a crash loop from
    starting a Claude every poll, and MAX_FAILURES stops it for good.
    Test: test_step_dead_relaunches_once_then_debounces,
    test_step_gives_up_after_two_failures, test_step_silent_never_relaunches,
    test_step_ok_resets_failures, test_step_success_then_dead_counts_failure,
    test_step_dry_run_does_not_relaunch, test_step_frozen_never_relaunches,
    test_step_frozen_by_spinner_needs_two_polls
    """
    kind, detail = classify(panes, session, shells, wake_at, transcript_mtime, now,
                            silent_min, busy=busy, spinner=spinner,
                            prev_spinner=book.get("spinner"), frozen_min=frozen_min)
    book["spinner"] = spinner if busy and kind != "dead" else None
    if kind == "ok":
        book["failures"], book["pending"] = 0, False
        return [], "architect=ok", False
    if kind == "frozen":
        book["failures"], book["pending"] = 0, False  # alive, so a past relaunch worked
        return ([("architect_frozen", "frozen", detail)],
                f"architect=frozen {detail}; alert only, no relaunch, no keys", False)
    if kind == "silent":
        return ([("architect_silent", f"wake-{int(wake_at)}", detail)],
                f"architect=silent {detail}; alert only, no relaunch, no keys", False)
    alerts = [("architect_dead", "dead", detail)]
    if book.get("pending"):  # last relaunch reported success, yet it is dead again
        book["failures"], book["pending"] = book.get("failures", 0) + 1, False
    failures = book.get("failures", 0)
    if failures >= MAX_FAILURES:
        alerts.append(("architect_relaunch_failed", "gave-up",
                       f"{failures} relaunches failed; watchdog stopped"))
        return alerts, f"architect=dead {detail}; relaunch suspended after {failures} failures", False
    wait = RELAUNCH_DEBOUNCE - (now - book.get("last_relaunch", 0))
    if wait > 0:
        return alerts, f"architect=dead {detail}; relaunch debounced {int(wait)}s", False
    if dry_run:
        return alerts, f"architect=dead {detail}; dry-run: would relaunch", False
    book["last_relaunch"] = now
    status, why = relauncher()
    if status == "ok":
        book["pending"] = True
        return alerts, f"architect=dead {detail}; relaunched ({why})", True
    if status == "aborted":
        return alerts, f"architect=dead {detail}; relaunch aborted ({why})", False
    book["failures"] = failures + 1
    if book["failures"] >= MAX_FAILURES:
        alerts.append(("architect_relaunch_failed", "gave-up",
                       f"{book['failures']} relaunches failed: {why}"))
    return alerts, f"architect=dead {detail}; relaunch FAILED ({why})", False

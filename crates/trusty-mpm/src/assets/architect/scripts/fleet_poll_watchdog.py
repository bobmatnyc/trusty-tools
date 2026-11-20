"""fleet_poll_watchdog.py — the poller's Architect watchdog step (#10571 Phase 2, prototype fleet-poll.py :1739-1816).

Why: the poller outlives the Architect, so it is the one process that can see the Architect dead, silent or
frozen (architect-watchdog.py's rules) and raise that to the owner.
What: architect_busy reads the Architect's pane (no keys); architect_transcript_mtime its newest transcript;
run_architect_relaunch the one relaunch path; watchdog_alerts runs architect-watchdog.step once a cycle and
returns (alerts, log phrase). The relaunch is `tm architect up`, the bound launch path (#11367); while
`tm` is not installed it reports "failed" before touching tmux, so after two polls the watchdog stops and
raises architect_relaunch_failed. Names from the poller are read through `P` (bind()), as in the other modules.
Test: tests/test_fleet_poll_watchdog_wiring.py
"""
import json
import os
import subprocess

P = None  # fleet-poll.py's late-bound view; set by bind()
DONE_SUMMARY = "· done "  # finished-turn summary: "✻ Brewed for 9s · done 7:14 AM"


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def _alive(pane):
    return pane["cmd"] not in P.SHELLS and not pane["cmd"].startswith("-")


LAUNCHES = {"init", "respawn_and_launch", "launch"}  # `tm architect up` actions that start a claude


def _up(runner, *extra):
    """(exit code, parsed --json report or None) of one `tm architect up` run."""
    r = runner([P.TM_BIN, "architect", "up", "--json", "--dir", P.ROOT, *extra],
               capture_output=True, text=True, timeout=P.watchdog.RELAUNCH_TIMEOUT)
    try:
        report = json.loads(r.stdout or "")
    except ValueError:
        report = None
    return r.returncode, report if isinstance(report, dict) else None


def _state(report):
    observed = (report or {}).get("observed")
    return observed.get("state") if isinstance(observed, dict) else None


def run_architect_relaunch(lister=None, runner=None):
    """Relaunch the Architect session. Returns ("ok"|"failed"|"aborted", detail).

    Why: the dead-Architect branch of the watchdog needs one real relaunch, and a `claude` with no
    launch record loses its pm-guard anchor exemptions (#11367).
    What: fails, touching nothing, while `tm` is not installed; re-lists panes and aborts if a live
    (non-shell) Architect pane now exists; runs `tm architect up --json`, the bound launch path. Fails
    closed unless up exits 0, names this session and took a launch action, and a following
    `up --dry-run` observes the Architect live and bound by its launch record.
    Test: test_relaunch_aborts_when_live, test_relaunch_runs_the_bound_launch_path,
    test_relaunch_fails_closed, test_relaunch_without_tm_never_touches_tmux
    """
    lister, runner = lister or P.list_panes, runner or subprocess.run
    if not P.TM_BIN or not os.path.isfile(P.TM_BIN):
        return "failed", "tm is not installed"
    try:
        if [p for p in lister() if P.is_architect_pane(p) and _alive(p)]:
            return "aborted", "a live claude process exists"
        code, report = _up(runner)
        if code != 0 or report is None:
            return "failed", f"tm architect up exit {code}"
        if report.get("session") != P.ARCHITECT:
            return "failed", f"tm architect up acted on {report.get('session')!r}, not {P.ARCHITECT}"
        if report.get("action") not in LAUNCHES:
            return "failed", f"tm architect up did {report.get('action')} ({_state(report)})"
        code, report = _up(runner, "--dry-run")
        if code != 0 or _state(report) != "live_bound":
            return "failed", f"no launch record binds the relaunched Architect ({_state(report)})"
        return "ok", "tm architect up: live and bound"
    except subprocess.TimeoutExpired:
        return "failed", f"timeout after {P.watchdog.RELAUNCH_TIMEOUT}s"
    except (OSError, P.TmuxError) as e:
        return "failed", f"{type(e).__name__}: {e}"


def architect_transcript_mtime():
    """mtime of the Architect's newest interactive transcript, or None."""
    try:
        path = P.self_ctx.newest_transcript(P.SELF_CTX_DIR)
        return os.path.getmtime(path) if path else None
    except OSError:
        return None


def architect_busy(panes, capturer=None):
    """(busy, spinner text) of the Architect's live pane; (False, "") if none or unreadable.

    Why: the frozen check needs "a turn is running" and the spinner text, which only the pane shows.
    What: parse_pane on a read-only capture of the first non-shell Architect pane; no keys. A finished
    turn's "· done" summary is idle, whatever monitor rows still run.
    Test: test_architect_busy_reads_spinner, test_architect_busy_completed_turn_is_idle
    """
    capturer = capturer or P.capture
    for p in panes:
        if P.is_architect_pane(p) and _alive(p):
            try:
                info = P.parse_pane(capturer(p["pane"]))
            except P.TmuxError as e:
                P.log(f"architect capture ERROR {e}")
                return False, ""
            if DONE_SUMMARY in info["status"]:
                return False, ""  # completed turn at the prompt; a live Monitor row is not a turn
            return info["busy"], info["status"]
    return False, ""


def watchdog_alerts(state, panes, now, dry_run=False):
    """(alerts, log phrase) for the Architect watchdog this cycle."""
    book = state.setdefault("watchdog", {})
    busy, spinner = P.architect_busy(panes)
    wd = P.watchdog
    found, phrase, relaunched = wd.step(
        book, panes, now, session=P.ARCHITECT, shells=P.SHELLS,
        wake_at=state.get("wake_sent_at"), transcript_mtime=P.architect_transcript_mtime(),
        silent_min=wd.silent_minutes(), relauncher=P.run_architect_relaunch,
        dry_run=dry_run, busy=busy, spinner=spinner, frozen_min=wd.frozen_minutes())
    if relaunched:
        state["wake_sent_at"] = None  # the new session starts a fresh silent clock
    return [P.alert(k, f, d, P.ARCHITECT, now=now) for k, f, d in found], phrase

#!/usr/bin/env python3
"""fleet-poll.py — deterministic fleet poller; wakes the supervisor only on need.

Why: the owner's ruling (2026-09-25) makes supervisor polling deterministic. A
Claude session woke every 15 minutes and read every pane with inference even
when nothing needed action. This script decides without any LLM whether
anything needs the supervisor, and wakes the tm-architect pane only then.
What: every --interval seconds it lists the tmux panes, captures each tm-*
PM pane, classifies it (input box via scripts/input-state.py `classify`,
dialog, question, idle, ctx, plan-limit, error, exited — see quiet_sessions,
below, for two exceptions), reads new permission_prompt / agent_needs_input
lines of inbox/events.jsonl, checks host load (5-min avg > LOAD_FACTOR x
cores), disk and swap, self_ctx_alerts (self-ctx.py: the supervisor's own
context, which has no `ctx N%` line to read), and appends each NEW alert as
a JSON line to inbox/alerts.jsonl. Dedup state is inbox/poll-state.json: an
alert with the same (session, pane, kind, fingerprint) is not re-emitted
until it clears for one cycle, or its kind's re-emit interval passes.
`dispatch_wake` types the one-line pointer into the tm-architect pane and
sends keys to no other pane. quiet_sessions.py drops "idle" for a parked
session and "exited" for a disposable test-fixture one. The pure parsing and
alert evaluation live in scripts/fleet-classify.py; this file does the I/O.
Test: tests/test_fleet_poll.py, tests/test_quiet_sessions.py
rc_restore: a pane whose status line shows `/rc failed` (Remote Control dropped
by an account change) gets scripts/rc-restore.sh, at most once per session per
RC_DEBOUNCE s; the outcome is an rc_restored / rc_failed alert (rc_step).
The Architect's own pane gets the same (architect_rc_step) on `/rc failed` or a
changed `✻<account>` status-line field, only while its box is `empty` and idle on
a fresh re-check; after a successful restore caused by an account change every
PM runs rc_step in that cycle, and no wake is typed into the Architect pane in a
cycle that typed rc-restore keys into it.
Test: scripts/test_fleet_poll_rc.py
architect watchdog: each cycle classifies tm-architect dead / frozen / silent / ok
(architect-watchdog.py). Dead: an `architect_dead` alert, a macOS notification
and one relaunch (session-relaunch.sh, first prompt /tm-session-resume), at
most one per 30 min, two failures then `architect_relaunch_failed`. Silent:
alert and notification only; no relaunch and no keys, ever. Frozen (busy pane plus a
transcript older than FROZEN_MIN, default 15 min, or an unchanged spinner across two
polls): `architect_frozen` alert and notification, same no-relaunch, no-keys rule.
Test: scripts/test_fleet_poll_watchdog.py
usage: each cycle samples Claude account usage (scripts/usage.py poll_step: the
statusline cache, or a /usage probe in one idle pane, at most hourly and at once
on an account change) into local/usage/ledger.jsonl and emits `usage` alerts
(5h exhaustion within 45 min, weekly 80/90%, cutover recommended). A sampler
failure is logged and never stops the cycle.
Test: scripts/test_usage.py
dashboard: each cycle runs scripts/dashboard_feed.py (subprocess, 60 s timeout) with the
pane classification of that cycle, writes inbox/dashboard-feed.json, and applies it to
the dashboard store itself (dashboard_feed.apply_feed: the `usage` field of changed
sessions/<S> docs and meta/state.fleet_usage, each pinned with if_version; a version
conflict skips that row until the next poll). No alert and no Architect turn on success.
A failed apply raises one `dashboard` alert per failure text, carrying the error. Not run
in --dry-run.
Test: scripts/test_fleet_poll_dashboard.py
Usage: scripts/fleet-poll.py [--once] [--interval N] [--dry-run]
       (normally run via scripts/start-fleet-poll.sh)
"""
import argparse
import copy
import glob
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
# ARCHITECT_INBOX_DIR (#8436), else SUPERVISOR_INBOX_DIR as notify-supervisor.sh
# names it: tests write elsewhere.
INBOX = (os.environ.get("ARCHITECT_INBOX_DIR") or os.environ.get("SUPERVISOR_INBOX_DIR")
         or os.path.join(ROOT, "inbox"))
EVENTS = os.path.join(INBOX, "events.jsonl")
ALERTS = os.path.join(INBOX, "alerts.jsonl")
TRIAGE = os.path.join(INBOX, "triage.jsonl")
TRIAGE_RULES = os.environ.get("TRIAGE_RULES") or os.path.join(ROOT, "local", "triage-rules.json")
STATE = os.path.join(INBOX, "poll-state.json")
LOG = os.path.join(INBOX, "poll.log")
SOCKET = os.environ.get("TMUX_SOCKET", "")

# The Architect's session, the only pane wake() types into; the poller's own
# session derives from it (#8436). SUPERVISOR is the name the code below uses.
ARCHITECT = os.environ.get("ARCHITECT_SESSION") or "tm-architect"
SUPERVISOR = ARCHITECT
POLL_SESSION = os.environ.get("ARCHITECT_POLL_SESSION") or f"{ARCHITECT}-poll"
COLLECTOR_SESSION = "tm-supervisor-collector"  # scripts/start-question-collector.sh
SKIP_SESSIONS = {ARCHITECT, COLLECTOR_SESSION, POLL_SESSION}
SHELLS = {"bash", "zsh", "sh", "fish", "dash", "ksh", "tcsh", "csh", "login"}
EVENT_TYPES = {"permission_prompt", "agent_needs_input"}  # idle_prompt is not an alert

DASHBOARD_TIMEOUT = 60    # seconds the feeder subprocess may run
DASHBOARD_SCRIPT = os.path.join(HERE, "dashboard_feed.py")
DASHBOARD_FEED = os.path.join(INBOX, "dashboard-feed.json")
DISK_HIGH = 90
LOAD_FACTOR_DEFAULT = 1.5  # x cores, vs. the 5-min load avg; env LOAD_FACTOR overrides
RC_DEBOUNCE = 600         # seconds between rc-restore runs for one session
RC_TIMEOUT = 120          # seconds one rc-restore.sh run may take
RC_SKIPS_ALERT = 3        # consecutive skips (busy / typed input) before an rc_failed alert
RC_SCRIPT = os.path.join(HERE, "rc-restore.sh")
RELAUNCH_SCRIPT = os.path.join(HERE, "session-relaunch.sh")

# Builder band (architect.md "Builder slots follow CPU and disk"; architect-host-admin skill).
# Each is overridable by the env var of the same name.
# Load lines per the owner, 2026-10-01 18:4xZ ("x1.5: 16/20, stop 30"): 16 / 20 / 30 on 16 cores.
BAND_STOP_LOAD_FACTOR = 1.875  # x cores: stop above this 5-min load (30 on 16)
BAND_HOLD_LOAD_FACTOR = 1.125  # x cores: stateless hold line, used only when prev is None
BAND_OPEN_LOAD_FACTOR = 1.0    # x cores: from hold/stop, open only below this (16 on 16)
BAND_REHOLD_LOAD_FACTOR = 1.25  # x cores: from open, hold only above this (20 on 16)
BAND_STOP_FREE_GIB = 100.0     # data volume: stop below this
BAND_HOLD_FREE_GIB = 200.0     # data volume: hold below this
BAND_STOP_MEM_FREE_PCT = 20.0  # stop below this memory free %
DATA_VOLUME = "/System/Volumes/Data"  # macOS: "/" is the sealed system volume

SELF_CTX_WINDOW_DEFAULT = 1_000_000
# Claude Code transcripts for every session (a pane's path, '/' and '.' -> '-').
TRANSCRIPT_BASE = os.environ.get("TRANSCRIPT_BASE") or os.path.expanduser(
    "~/.trusty-tools/trusty-mpm/claude-config/projects")
SELF_CTX_THRESHOLD_DEFAULT = 25  # ruling b7 2026-10-07 (was 40)


def _load_by_path(name, filename):
    # A hyphenated script name isn't importable, so load it by path.
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# Pure pane parsing and alert evaluation live in fleet-classify.py (#8891, #8906).
# These names stay fleet-poll attributes, as they were before the split.
fleet_classify = _load_by_path("fleet_classify", "fleet-classify.py")
IDLE_AFTER = fleet_classify.IDLE_AFTER
REEMIT = fleet_classify.REEMIT
CTX_WARN = fleet_classify.CTX_WARN
DETAIL_MAX = fleet_classify.DETAIL_MAX
DONE_SUMMARY = fleet_classify.DONE_SUMMARY
SECRET = fleet_classify.SECRET
fp = fleet_classify.fp
clean = fleet_classify.clean
find_box = fleet_classify.find_box
_screen_lines = fleet_classify._screen_lines
parse_pane = fleet_classify.parse_pane
alert = fleet_classify.alert
evaluate = fleet_classify.evaluate
fleet_plan_alerts = fleet_classify.fleet_plan_alerts
dedup = fleet_classify.dedup
iso = fleet_classify.iso
CTX_HIGH = fleet_classify.CTX_HIGH
input_state = fleet_classify.input_state
self_ctx = _load_by_path("self_ctx", "self-ctx.py")
quiet_sessions = _load_by_path("quiet_sessions", "quiet-sessions.py")
watchdog = _load_by_path("architect_watchdog", "architect-watchdog.py")
usage = _load_by_path("usage", "usage.py")
alert_triage = _load_by_path("alert_triage", "alert_triage.py")
token_watch = _load_by_path("token_watch", "token_watch.py")
dashboard_feed = _load_by_path("dashboard_feed", "dashboard_feed.py")
owner_answers = _load_by_path("owner_answers", "owner_answers.py")
user_state = _load_by_path("user_state", "user_state.py")
dashboard_store = _load_by_path("dashboard_store", "dashboard_store.py")
SELF_CTX_DIR = os.environ.get("SELF_CTX_DIR") or self_ctx.default_dir()
QUIET_SESSIONS_FILE = (os.environ.get("ARCHITECT_QUIET_SESSIONS_FILE")
                       or os.path.join(ROOT, "local", "quiet-sessions.txt"))

# --- pane parsing (pure) ----------------------------------------------------


# --- alert evaluation (pure) ------------------------------------------------


def run_rc_restore(session):
    """Run rc-restore.sh for one session. Returns (status, detail): status is
    "restored" (detail = URL), "skipped" or "failed" (detail = reason)."""
    try:
        r = subprocess.run([RC_SCRIPT, session], capture_output=True, text=True,
                           timeout=RC_TIMEOUT)
    except subprocess.TimeoutExpired:
        return "failed", f"timeout after {RC_TIMEOUT}s"
    except OSError as e:
        return "failed", f"cannot run rc-restore.sh: {e}"
    lines = [l for l in r.stdout.splitlines() if l.startswith(session + " ")]
    tail = lines[-1].split(" ", 2)[2] if lines and lines[-1].count(" ") >= 2 else ""
    if r.returncode == 0:
        return "restored", tail
    if r.returncode == 2:
        return "skipped", tail
    return "failed", tail or f"exit {r.returncode}"


def rc_step(state, session, pane, info, now, runner=run_rc_restore,
            idle_boxes=("empty", "suggestion")):
    """Alerts for one pane's Remote Control state. Pure but for `runner`.

    Why: an account change drops Remote Control on every PM at once; the owner wants
    it restored without anyone typing (2026-10-02 17:27Z).
    What: no `/rc failed` marker clears the session's state. With it, a run
    happens at most once per RC_DEBOUNCE s (state["rc"][session]["last"]); a
    busy pane or one holding typed input is a skip, not a failure: it retries
    next poll and alerts only after RC_SKIPS_ALERT consecutive skips.
    Test: test_rc_marker_detected, test_rc_debounce_and_skip_alert
    """
    book = state.setdefault("rc", {})
    if not info.get("rc_failed"):
        book.pop(session, None)
        return []
    e = book.setdefault(session, {"last": 0, "skips": 0})
    if now - e["last"] < RC_DEBOUNCE:
        return []
    if info["busy"] or info["box"] not in idle_boxes:
        status, detail = "skipped", "pane busy" if info["busy"] else f"input {info['box']}"
    else:
        status, detail = runner(session)
    if status == "skipped":
        e["skips"] += 1
        if e["skips"] >= RC_SKIPS_ALERT:
            return [alert("rc_failed", "skips", f"rc restore skipped {e['skips']} polls: "
                          f"{detail}", session, pane, now)]
        return []
    e["last"], e["skips"] = now, 0
    kind = "rc_restored" if status == "restored" else "rc_failed"
    return [alert(kind, fp(f"{kind}{now}"), detail or status, session, pane, now)]


def architect_rc_step(state, pane, info, now, runner=run_rc_restore, capturer=None):
    """(alerts, ran, fanout) for the Architect's own pane after a /login.

    Why: the owner, 2026-10-07: a login to a different account must rename the
    Architect to tm-architect again; SKIP_SESSIONS keeps it out of the PM loop.
    What: triggers on `/rc failed` or a `✻<account>` field that differs from
    state["architect_account"] (first sight only records it; the record moves
    only after a restore succeeds, so a failed one retries). Acts through
    rc_step (debounce, skip alert) but only on a strictly `empty` box, and
    guarded() re-captures right before the runner so a peer message that
    started a turn since the cycle's capture is never typed over. `ran` is
    True once the runner was invoked (keys were typed: no wake this cycle);
    `fanout` is True after a successful restore caused by an account change.
    Test: test_architect_account_change_detected, test_architect_idle_gating,
    test_architect_recheck_before_invoke, test_architect_fanout_after_account_restore
    """
    capturer = capturer or capture
    acct, rec = info.get("account"), state.get("architect_account")
    if acct and rec is None:
        state["architect_account"] = rec = acct
    changed = bool(acct and rec and acct != rec)
    ran = []

    def guarded(session):
        try:
            fresh = parse_pane(capturer(pane))
        except TmuxError as e:
            return "skipped", f"re-check capture failed: {e}"
        if fresh["busy"] or fresh["dialog"] or fresh["box"] != "empty":
            return "skipped", "pane not idle on re-check"
        ran.append(True)
        return runner(session)

    trigger = dict(info, rc_failed=bool(changed or info.get("rc_failed")))
    alerts = rc_step(state, SUPERVISOR, pane, trigger, now, guarded, idle_boxes=("empty",))
    restored = any(a["kind"] == "rc_restored" for a in alerts)
    if restored and acct:
        state["architect_account"] = acct
    return alerts, bool(ran), restored and changed


def usage_alerts(state, now, idle_panes, dry_run=False, step=None):
    """Usage sampling and `usage` alerts (scripts/usage.py). A failure here is
    logged and yields no alerts: it must never break the poll loop."""
    try:
        return [alert("usage", fp_, detail, now=now)
                for fp_, detail in (step or usage.poll_step)(state, now, idle_panes, dry_run)]
    except Exception as e:
        log(f"usage sampler ERROR {type(e).__name__}: {e}", dry_run)
        return []


def token_alerts(now, dry_run=False, step=None):
    """Deterministic token watcher (scripts/token_watch.py, no LLM) -> `token` alerts.
    A failure is logged and yields no alerts: it must never break the poll loop."""
    try:
        out = []
        for r in (step or token_watch.step)(now=now, dry_run=dry_run):
            a = alert("token", r["fingerprint"], r["detail"], session=r["session"], now=now)
            a["display"] = f"[{r['session']}]: token {a['detail']}"
            out.append(a)
        return out
    except Exception as e:
        log(f"token watcher ERROR {type(e).__name__}: {e}", dry_run)
        return []


def pane_live(info, ps, now):
    """Live state from the poller's own classification: waiting (dialog or
    question), working (busy), else idle; idle_min from ps['idle_since']."""
    if info["dialog"] or info["question"]:
        state = "waiting"
    elif info["busy"]:
        state = "working"
    else:
        state = "idle"
    since = ps.get("idle_since")
    return {"state": state, "idle_min": int((now - since) // 60) if since is not None else 0}


def dashboard_apply(feed, now, root=None):
    """Apply the feed to the dashboard store (dashboard_feed.apply_feed). Returns None when
    it applied (version conflicts are logged and retried next poll), or one `dashboard`
    alert carrying the first error when it failed. The fingerprint is the error text, so
    a persistent failure alerts once."""
    root = root or os.environ.get("DASHBOARD_ROOT") or dashboard_store.DEFAULT_ROOT
    try:
        res = dashboard_feed.apply_feed(feed, root)
    except Exception as e:
        res = {"wrote": [], "conflicts": [], "errors": [f"{type(e).__name__}: {e}"]}
    if res["conflicts"]:
        log(f"dashboard apply: version conflict, retry next poll: {', '.join(res['conflicts'])}")
    if not res["errors"]:
        return None
    first = res["errors"][0]
    log(f"dashboard apply ERROR {first}")
    return alert("dashboard", hashlib.sha1(first.encode()).hexdigest()[:12],
                 f"feed apply failed ({len(res['errors'])} error(s)): {first}", now=now)


def owner_answer_alerts(now, root=None):
    """One `owner_answer` alert per dashboard item answered (or cancelled) and not yet
    relayed. Fingerprint = item id + answered_at/cancelled_at, so a re-answer alerts again.
    A store read error is logged and yields no alerts; it never stops the cycle."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner answers ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] in ("feedback", "owner_unpark_due", "owner_command", "wont_do", "effort", "status_request"):
            continue
        text = clean(it["text"])
        text = text[:77] + "..." if len(text) > 80 else text
        verb = "cancelled" if it["kind"] == "cancelled" else "answered"
        hhmm = it["at"][11:16] + "Z" if len(it["at"]) >= 16 else "?"
        a = alert("owner_answer", f"{it['id']}@{it['at']}", f"{it['id']} {verb} {hhmm}", now=now)
        a["display"] = f"[owner] {it['id']} {verb} {hhmm}" + (f": {text}" if text else "")
        out.append(a)
    return out


FEEDBACK_KIND = {"question": "decision", "action": "ask", "task": "Architect task"}


def owner_feedback_alerts(now, root=None):
    """One `owner_feedback` alert per dashboard feedback (Feedback button) still status new.
    Fingerprint = item id + feedback.at. A store read error is logged and yields no alerts."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner feedback ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "feedback":
            continue
        note = clean(it["text"])
        note = note[:77] + "..." if len(note) > 80 else note
        should = FEEDBACK_KIND.get(it.get("to", ""), "")
        head = f"[owner] feedback on {it['id']}"
        a = alert("owner_feedback", f"{it['id']}@{it['at']}", f"feedback on {it['id']}", now=now)
        a["display"] = head + (f": should be {should}" if should else "") + (f" \u2014 {note}" if note else "")
        out.append(a)
    return out


def owner_wont_do_alerts(now, root=None):
    """One `owner_wont_do` alert per dashboard item the owner marked Won't do and the Architect has
    not yet acknowledged. Fingerprint = item id + wont_do.at. A store read error is logged only."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner wont_do ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "wont_do":
            continue
        why = clean(it["text"])
        why = why[:77] + "..." if len(why) > 80 else why
        a = alert("owner_wont_do", f"{it['id']}@{it['at']}", f"{it['id']} won't do", now=now)
        a["display"] = f"[owner] {it['id']} won't do: {why}"
        out.append(a)
    return out


def user_state_alerts(now, root=None):
    """One `user_state` alert per change of the owner's presence state (dashboard settings pane, meta/settings).
    Fingerprint = user_state_at; nothing while the state was never set (the default, active, is not a change).
    Display `[dashboard]: user state -> <state>`."""
    cur = user_state.read(root)
    if not cur["at"]:
        return []
    a = alert("user_state", cur["at"], f"user state -> {cur['state']}", now=now)
    a["display"] = f"[dashboard]: user state -> {cur['state']}"
    return [a]


def owner_status_alerts(now, root=None):
    """One `status_request` alert per open dashboard task the owner asked a status update on (Status Update button)
    and the Architect has not yet answered (status_request cleared). Fingerprint = item id + status_request.at.
    A store read error is logged and yields no alerts."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner status ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "status_request":
            continue
        title = clean(it["text"])
        title = title[:77] + "..." if len(title) > 80 else title
        a = alert("status_request", f"{it['id']}@{it['at']}", f"status update requested: {it['id']}", now=now)
        a["display"] = f"[dashboard]: status update requested: {it['id']}" + (f" {title}" if title else "")
        out.append(a)
    return out


def owner_effort_alerts(now, root=None):
    """One `owner_effort` alert per goal effort tune (More/Less Effort button) still status new.
    Fingerprint = doc id + at. A store read error is logged and yields no alerts."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner effort ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "effort":
            continue
        hhmm = it["at"][11:16] + "Z" if len(it["at"]) >= 16 else "?"
        a = alert("owner_effort", f"{it['id']}@{it['at']}", it["text"], now=now)
        a["display"] = f"[owner] {it['text']} {hhmm}"
        out.append(a)
    return out


def owner_command_alerts(now, root=None):
    """One `owner_command` alert per session command (Activate / Prioritize / Park / Restart buttons) still status new.
    Fingerprint = session id + command.at. A store read error is logged and yields no alerts."""
    try:
        items = owner_answers.pending(root)
    except Exception as e:
        log(f"owner command ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "owner_command":
            continue
        who = it.get("session") or it["id"]
        what = {"prioritize": f"prioritize {it['goal']}", "park": "park", "restart": "restart"}.get(it["type"], "activate")
        note = clean(it["text"])
        note = note[:77] + "..." if len(note) > 80 else note
        a = alert("owner_command", f"{it['id']}@{it['at']}", f"{what} {who}", now=now)
        ident = "" if it["type"] in ("park", "restart") else f" ({it['id']})"
        a["display"] = f"[owner] {what} {who}{ident}" + (f": {note}" if note else "")
        out.append(a)
    return out

CLAUDE_CMD = re.compile(r"^(\d+\.\d+[\w.+-]*|claude|tm)$")   # a version-number command, claude or tm


def exit_reason(session_panes):
    """Why a session with a dashboard record is down, or None while it runs Claude or tm.
    No panes = tmux session gone; every pane dead = pane dead; the pane in front (active
    first) running a plain shell = "pane runs <cmd>, not Claude". Any other command is up."""
    if not session_panes:
        return "tmux session gone"
    live_panes = [p for p in session_panes if p.get("dead") != "1"]
    if not live_panes:
        return "pane dead"
    front = sorted(live_panes, key=lambda p: p.get("active") != "11")[0]
    cmd = front["cmd"]
    if cmd in SHELLS or cmd.startswith("-"):
        return f"pane runs {cmd}, not Claude"
    return None


def exited_step(state, panes, now, dry_run=False, root=None):
    """Mark each recorded session that has exited and raise one `session_exited` alert per exit.
    Writes sessions/<S> state "exited" and status "Exited: <reason> (detected HH:MMZ)" when they
    differ (never in dry-run); the detected time lives in state["exited"] so the status and the
    alert fingerprint (session id + detected time) stay stable until the session comes back.
    A session the owner parked is skipped, the Architect's own sessions are skipped, and the step
    never relaunches anything. A store error is logged and yields no alerts."""
    try:
        store = dashboard_store.Store(root or os.environ.get("DASHBOARD_ROOT") or dashboard_store.DEFAULT_ROOT)
        docs = store.list_docs("sessions")
    except Exception as e:
        log(f"exited step ERROR {type(e).__name__}: {e}", dry_run)
        return []
    by = {}
    for p in panes:
        by.setdefault(p["session"], []).append(p)
    prev, kept, out = state.get("exited", {}), {}, []
    for d in docs:
        data, name = d["data"], str(d["data"].get("name", ""))
        if (not name.startswith("tm-") or name in SKIP_SESSIONS or data.get("state") == "parked"
                or quiet_sessions.is_test_fixture_session(name)):
            continue
        reason = exit_reason(by.get(name, []))
        if reason is None:
            continue
        at = prev.get(name, {}).get("at") or iso(now)
        kept[name] = {"at": at}
        status = f"Exited: {reason} (detected {at[11:16]}Z)"
        if (data.get("state"), data.get("status")) != ("exited", status) and not dry_run:
            try:
                store.update("sessions", d["id"], {"state": "exited", "status": status, "updated": iso(now)})
            except Exception as e:
                log(f"exited step write {d['id']} ERROR {type(e).__name__}: {e}")
        out.append(alert("session_exited", f"{d['id']}@{at}", f"{name} exited: {reason}", now=now))
        out[-1]["display"] = f"[fleet] {name} exited: {reason} (detected {at[11:16]}Z)"
    state["exited"] = kept
    return out


def owner_unpark_alerts(now, root=None):
    """One `owner_unpark_due` alert per parked dashboard item whose wake date has passed.
    Fingerprint = item id + parked.until, so a re-park alerts again. Read errors are logged only."""
    try:
        items = owner_answers.pending(root, datetime.fromtimestamp(now, timezone.utc))
    except Exception as e:
        log(f"owner unpark ERROR {type(e).__name__}: {e}")
        return []
    out = []
    for it in items:
        if it["kind"] != "owner_unpark_due":
            continue
        a = alert("owner_unpark_due", f"{it['id']}@{it['at']}", f"parked {it['id']} is due", now=now)
        a["display"] = f"[owner] parked {it['id']} is due: {it['updates']} updates since parked"
        out.append(a)
    return out


def run_dashboard_feeder(live, runner=None):
    """Run dashboard_feed.py in a subprocess (DASHBOARD_TIMEOUT s) and return the
    feed it wrote, or None. A failure is logged and never stops the cycle."""
    try:
        live_path = os.path.join(INBOX, "dashboard-live.json")
        with open(live_path, "w") as f:
            json.dump(live, f)
        (runner or subprocess.run)([sys.executable, DASHBOARD_SCRIPT, "--out", DASHBOARD_FEED,
                                    "--live", live_path], timeout=DASHBOARD_TIMEOUT,
                                   capture_output=True, check=True)
        with open(DASHBOARD_FEED) as f:
            return json.load(f)
    except Exception as e:
        log(f"dashboard feeder ERROR {type(e).__name__}: {e}")
        return None


# --- I/O ---------------------------------------------------------------------

class TmuxError(Exception):
    pass


def log(msg, dry_run=False):
    line = f"{iso()} {msg}"
    if dry_run:
        print(line)
        return
    with open(LOG, "a") as f:
        f.write(line + "\n")


def tmux(*args):
    cmd = ["tmux", *(["-S", SOCKET] if SOCKET else []), *args]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise TmuxError(f"{args[0]}: {e}") from e
    if r.returncode != 0:
        raise TmuxError(f"{args[0]}: {r.stderr.strip()[:200]}")
    return r.stdout


def list_panes():
    out = tmux("list-panes", "-a", "-F",
               "#{session_name}\t#{pane_id}\t#{pane_current_command}\t#{window_active}#{pane_active}"
               "\t#{pane_current_path}\t#{pane_dead}")
    panes = []
    for line in out.splitlines():
        parts = line.split("\t", 5)
        if len(parts) >= 4:
            parts += [""] * (6 - len(parts))
            panes.append(dict(zip(("session", "pane", "cmd", "active", "path", "dead"), parts)))
    return panes


_transcript_cache = {}  # transcript path -> ((mtime, size), tokens)

# Claude Code's live per-process session registry: <pid>.json with sessionId and
# tmux ("<session>:@<win>.%<pane>"). It follows /clear and resume, unlike a cwd.
SESSIONS_DIR = os.environ.get("SESSIONS_DIR") or os.path.expanduser(
    "~/.trusty-tools/trusty-mpm/claude-config/sessions")


def pane_session_id(pane):
    """sessionId of the live Claude process in tmux `pane` (e.g. "%2146"), or None.
    Several PMs share one cwd, so the cwd cannot pick a transcript (the 2026-10-05
    "ctx 45%" x5 false alerts). Newest live entry wins; dead pids are ignored."""
    best = None
    try:
        names = os.listdir(SESSIONS_DIR)
    except OSError:
        return None
    for n in names:
        if not n.endswith(".json"):
            continue
        try:
            with open(os.path.join(SESSIONS_DIR, n)) as f:
                d = json.load(f)
            if not str(d.get("tmux", "")).endswith("." + pane):
                continue
            os.kill(int(d["pid"]), 0)
            if best is None or (d.get("updatedAt") or 0) >= (best.get("updatedAt") or 0):
                best = d
        except PermissionError:  # alive, owned by another user
            if best is None or (d.get("updatedAt") or 0) >= (best.get("updatedAt") or 0):
                best = d
        except (OSError, ValueError, TypeError, KeyError):
            continue
    return best.get("sessionId") if best else None


def transcript_ctx_pct(pane):
    """Context % of the Claude session running in tmux `pane` (last assistant
    usage / window), or None when its own transcript cannot be resolved: never
    another session's. Used when the status line carries no `ctx N%` (after an
    auto-compact, a narrow pane that truncates it, or `ctx>200k`). Cached per
    transcript by (mtime, size), so a 10-minute tick re-reads only a changed file."""
    try:
        window = int(os.environ.get("PANE_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT))
        sid = pane_session_id(pane)
        if not sid or window <= 0:
            return None
        matches = glob.glob(os.path.join(glob.escape(TRANSCRIPT_BASE), "*", f"{glob.escape(sid)}.jsonl"))
        if not matches:
            return None
        newest = max(matches, key=os.path.getmtime)
        st = os.stat(newest)
        key = (st.st_mtime, st.st_size)
        hit = _transcript_cache.get(newest)
        if hit and hit[0] == key:
            tokens = hit[1]
        else:
            usage = self_ctx.tail_last_assistant_usage(newest)
            tokens = None if not usage else (
                (usage.get("input_tokens") or 0) + (usage.get("cache_read_input_tokens") or 0)
                + (usage.get("cache_creation_input_tokens") or 0))
            _transcript_cache[newest] = (key, tokens)
        return None if tokens is None else int(100 * tokens / window)
    except (OSError, ValueError, TypeError) as e:
        log(f"transcript ctx ERROR {type(e).__name__}: {e}")
        return None


def capture(pane):
    return tmux("capture-pane", "-e", "-p", "-S", "-80", "-t", pane)


def send_to_supervisor(pane, *keys):
    """The only send path. Re-checks that `pane` belongs to tm-architect first."""
    owner = tmux("display-message", "-p", "-t", pane, "#{session_name}").strip()
    if owner != SUPERVISOR:
        raise TmuxError(f"refusing to send to {pane} (session {owner!r})")
    tmux("send-keys", "-t", pane, *keys)


def read_events(state):
    """New permission_prompt / agent_needs_input lines since the saved offset."""
    size = os.path.getsize(EVENTS) if os.path.exists(EVENTS) else 0
    off = state.get("events_offset")
    if off is None or size < off:   # first run: skip backlog; smaller file: rotated
        off = size if off is None else 0
    evs = []
    if size > off:
        with open(EVENTS) as f:
            f.seek(off)
            chunk = f.read()
            off = f.tell()
        for line in chunk.splitlines():
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            s = ev.get("tmux_session") or ""
            if ev.get("type") in EVENT_TYPES and s not in SKIP_SESSIONS:
                evs.append(alert(ev["type"], fp(line), ev.get("message") or ev["type"],
                                 s, ev.get("tmux_pane") or "", now=time.time()))
    state["events_offset"] = off
    return evs


def swap_used_mb():
    if sys.platform == "darwin":
        out = subprocess.run(["sysctl", "-n", "vm.swapusage"], capture_output=True,
                             text=True, timeout=5).stdout
        m = re.search(r"used = ([\d.]+)([KMG])", out)
        if not m:
            return None
        return float(m.group(1)) * {"K": 1 / 1024, "M": 1, "G": 1024}[m.group(2)]
    with open("/proc/meminfo") as f:
        mi = {l.split(":")[0]: int(l.split()[1]) for l in f if ":" in l}
    return (mi.get("SwapTotal", 0) - mi.get("SwapFree", 0)) / 1024


def _env_float(name, default):
    try:
        return float(os.environ.get(name, default))
    except ValueError:
        return float(default)


def data_volume_path():
    """The volume that holds user data: the macOS data volume, else "/"."""
    return DATA_VOLUME if os.path.isdir(DATA_VOLUME) else "/"


def mem_free_pct():
    """System-wide memory free % from macOS `memory_pressure`; None if unavailable."""
    try:
        out = subprocess.run(["memory_pressure"], capture_output=True, text=True,
                             timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    m = re.search(r"System-wide memory free percentage:\s*(\d+)%", out)
    return float(m.group(1)) if m else None


def builder_band(load15, cores, free_gib, mem_free, swap_mb, prev=None):
    """"stop", "hold" or "open" from the host inputs. mem_free None skips that test.
    With prev (the stored band) the open/hold load boundary has hysteresis (the owner,
    2026-10-01 18:4xZ, x1.5): open from hold/stop only below 1.0 x cores (16 on 16), back
    to hold from open only above 1.25 x cores (20); stop above 1.875 x cores (30);
    between the edges prev stands. prev None is stateless (hold line 1.125 x cores)."""
    stop_f = _env_float("BAND_STOP_LOAD_FACTOR", BAND_STOP_LOAD_FACTOR)
    hold_f = _env_float("BAND_HOLD_LOAD_FACTOR", BAND_HOLD_LOAD_FACTOR)
    stop_gib = _env_float("BAND_STOP_FREE_GIB", BAND_STOP_FREE_GIB)
    hold_gib = _env_float("BAND_HOLD_FREE_GIB", BAND_HOLD_FREE_GIB)
    stop_mem = _env_float("BAND_STOP_MEM_FREE_PCT", BAND_STOP_MEM_FREE_PCT)
    if (load15 > stop_f * cores or free_gib < stop_gib
            or (mem_free is not None and mem_free < stop_mem) or (swap_mb or 0) > 0):
        return "stop"
    if free_gib < hold_gib:
        return "hold"
    if prev is None:
        return "hold" if load15 >= hold_f * cores else "open"
    if prev == "open":
        rehold_f = _env_float("BAND_REHOLD_LOAD_FACTOR", BAND_REHOLD_LOAD_FACTOR)
        return "hold" if load15 > rehold_f * cores else "open"
    open_f = _env_float("BAND_OPEN_LOAD_FACTOR", BAND_OPEN_LOAD_FACTOR)
    return "open" if load15 < open_f * cores else "hold"


def band_alerts(state, now, load15, cores, free_gib, mem_free, swap):
    """One "band" alert when the band differs from state["band"]; persists the new band.
    No stored band (first run) emits once."""
    old = state.get("band")
    band = builder_band(load15, cores, free_gib, mem_free, swap, prev=old)
    if band == old:
        return []
    state["band"] = band
    mem = "n/a" if mem_free is None else f"{mem_free:.0f}%"
    detail = (f"builder band {old or 'none'} -> {band} (load15 {load15:.1f}/{cores} cores, "
              f"{free_gib:.0f} GiB free, mem {mem} free, swap {swap or 0:.0f})")
    return [alert("band", f"{old}->{band}", detail, now=now)]


def host_alerts(now, state=None):
    out = []
    load1, load5, load15 = os.getloadavg()
    cores, factor = os.cpu_count() or 1, float(os.environ.get("LOAD_FACTOR", LOAD_FACTOR_DEFAULT))
    if load15 > factor * cores:
        out.append(alert("load", "load", f"load {load1:.1f}/{load5:.1f}/{load15:.1f} "
                         f"(15m > {factor}x{cores} cores)", now=now))
    du = shutil.disk_usage(data_volume_path())
    pct = 100 * du.used / du.total
    if pct > DISK_HIGH:
        out.append(alert("disk", "disk", f"data disk {pct:.0f}% used", now=now))
    try:
        swap = swap_used_mb()
    except (OSError, subprocess.SubprocessError, ValueError):
        swap = None
    if swap:
        out.append(alert("swap", "swap", f"swap in use: {swap:.0f} MB", now=now))
    if state is not None:
        out += band_alerts(state, now, load15, cores, du.free / 2**30, mem_free_pct(), swap)
    return out


def self_ctx_alerts(now, dry_run=False):
    """One "self_ctx" alert when the supervisor's own context (self_ctx.py,
    tailing its transcript) is at/over the threshold. A missing or unreadable
    transcript logs and yields no alert; self_ctx.py never raises for that."""
    try:
        window = int(os.environ.get("SELF_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT))
        threshold = int(os.environ.get("SELF_CTX_THRESHOLD", SELF_CTX_THRESHOLD_DEFAULT))
        tokens = self_ctx.self_ctx_tokens(SELF_CTX_DIR)
    except (OSError, ValueError, TypeError) as e:
        log(f"self_ctx ERROR {type(e).__name__}: {e}", dry_run)
        return []
    if tokens is None or window <= 0:
        return []
    pct = 100 * tokens / window
    if pct < threshold:
        return []
    return [alert("self_ctx", str(threshold), f"self ctx {pct:.0f}% ({tokens}/{window} tokens)",
                  SUPERVISOR, now=now)]


def _norm(text):
    """Strip all whitespace (including the box's leading non-breaking space)
    so a message that wraps across screen rows compares equal to itself."""
    return re.sub(r"\s+", "", text.replace("\xa0", ""))


def _box_full_text(capture):
    """Normalized text of the whole input box: the ❯ row plus any wrapped
    continuation rows that carry no rule of their own. A narrow pane (the
    supervisor's own is ~47 columns) wraps a typed message across several
    screen rows, and only the first carries the ❯ glyph input_state.classify()
    keys on — this reads the box find_box() bounds instead, so wrapping never
    makes a message that IS present look unconfirmed. "" if no box is found."""
    lines = _screen_lines(capture)
    box = find_box(lines)
    if not box:
        return ""
    top, bottom = box
    return _norm("".join(lines[top + 1:bottom]).replace("❯", ""))


def _own_pointer(text):
    """True if normalized box text looks like a wake() pointer this poller
    typed itself ('[poll HH:MMZ] N new alerts -> path'), including one split
    across wrapped rows. Never true for a human draft that merely starts with
    the same characters without the rest of the shape — a false positive here
    would delete a real draft, which wake() must never do."""
    return text.startswith("[poll") and "->" in text and any(
        k in text for k in ("newalerts", "alertsneedyou", "alertneedsyou"))


def wake(n, panes, dry_run, logged=None):
    """Type the pointer into tm-architect's input box. Returns a log phrase.

    Why: 2026-09-27/28 poll.log — the supervisor pane is narrow enough that a
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
    sup = sorted((p for p in panes if p["session"] == SUPERVISOR),
                 key=lambda p: p["active"] != "11")
    if not sup:
        return "skipped: no tm-architect pane"
    target = sup[0]["pane"]
    raw = capture(target)
    info = parse_pane(raw)
    if info["dialog"]:
        return "skipped: dialog open"
    if not info["has_box"]:
        # Usually the supervisor busy on its own turn: Claude Code's TUI drops
        # the rule-bounded box find_box() requires while it works, even though
        # a stale "❯" line elsewhere can still make input_state.classify()
        # guess "empty"/"suggestion" — that guess is not this skip's real
        # cause, so it must never appear in the log in its place.
        return "skipped: no input box found"
    if info["box"] not in ("empty", "suggestion"):
        if _own_pointer(_box_full_text(raw)):
            return "skipped: own stale pointer in box"
        return f"skipped: input {info['box']}"
    stamp = f"[poll {datetime.now(timezone.utc):%H:%M}Z]"
    if logged is None:
        msg = f"{stamp} {n} new alerts -> {ALERTS}"
    else:  # triage: n names only the alerts that need the Architect
        msg = (f"{stamp} {n} alert{'s need' if n != 1 else ' needs'} you "
               f"({logged} logged) -> {ALERTS}")
    if dry_run:
        return f"dry-run: would send to {target}: {msg}"
    send_to_supervisor(target, "-l", msg)
    time.sleep(0.5)
    if _norm(msg) not in _box_full_text(capture(target)):
        send_to_supervisor(target, *(["BSpace"] * len(msg)))
        time.sleep(0.3)
        empty = input_state.classify(capture(target))[0] in ("empty", "suggestion")
        return ("failed: text not confirmed in the input box; cleaned up, box empty"
                 if empty else
                 "failed: text not confirmed in the input box; cleanup left box non-empty")
    send_to_supervisor(target, "Enter")
    time.sleep(1)
    left = input_state.classify(capture(target))[1]
    return f"sent to {target}" + (" (still in box after Enter)" if msg[:40] in left else "")


def dispatch_wake(state, panes, new_count, dry_run, logged=None):
    """Carry `new_count` into state["wake_pending"] and attempt one wake()
    when anything is pending; clears only on a confirmed send, so a skipped
    or failed attempt leaves the FULL total for the next cycle to retry.
    Returns the log phrase."""
    state["wake_pending"] = state.get("wake_pending", 0) + new_count
    if logged is not None:
        state["wake_logged"] = state.get("wake_logged", 0) + logged
    if not state["wake_pending"]:
        state.pop("wake_logged", None)  # nothing woken: the logged count is only a footnote
        return "none"
    try:
        result = wake(state["wake_pending"], panes, dry_run,
                      state.get("wake_logged", 0) if logged is not None else None)
    except TmuxError as e:
        result = f"failed: {e}"
    if result.startswith("sent"):
        state["wake_pending"] = 0
        state.pop("wake_logged", None)
        if not dry_run:
            state["wake_sent_at"] = time.time()  # the watchdog's silent clock
    return result


def triage_step(new, now, state, dry_run):
    """Triage the cycle's new alerts: (alerts that wake the Architect, logged count).

    Why: most alerts are known and cost a full Architect turn (alert_triage.py).
    What: rules plus one batched Haiku call; one inbox/triage.jsonl line per alert
    (not written in --dry-run, where the gate is not called and its alerts wake).
    Any exception wakes for every alert, never silent.
    Test: test_cycle_step_writes_all_alerts_triage_and_counts_woken
    """
    if not new:
        return [], 0
    try:
        rules = alert_triage.load_rules(TRIAGE_RULES)
        rc = state.setdefault("triage_rc", {})
        def no_call(prompt):
            raise RuntimeError("dry-run: Haiku gate not called")
        recs = alert_triage.triage(new, rules, now, rc, runner=no_call if dry_run else None,
                                   pane_fn=lambda s: tmux("capture-pane", "-t", f"={s}:", "-p", "-S", "-30"))
        if not dry_run:
            alert_triage.append_records(os.environ.get("TRIAGE_LOG") or TRIAGE, recs)
    except Exception as e:
        log(f"triage ERROR {type(e).__name__}: {e}; waking for all {len(new)}", dry_run)
        return list(new), 0
    woken = [a for a, r in zip(new, recs) if r["decision"] == "wake"]
    if dry_run:
        for a, r in zip(new, recs):
            print(f"  triage {r['decision']} {r['path']}: {a['display'][:90]}")
    return woken, len(new) - len(woken)


# --- architect watchdog (I/O around architect-watchdog.py) -------------------

def notify_macos(title, message):
    """macOS notification via osascript. Never raises. Message text only: no
    contact data."""
    esc = lambda t: t.replace("\\", "\\\\").replace('"', '\\"')
    script = f'display notification "{esc(message)}" with title "{esc(title)}"'
    try:
        subprocess.run(["osascript", "-e", script], capture_output=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as e:
        log(f"notify ERROR {type(e).__name__}: {e}")


def run_architect_relaunch(lister=None, runner=None, tmux_fn=None):
    """Relaunch tm-architect. Returns ("ok"|"failed"|"aborted", detail).

    Why: the dead-Architect branch of the watchdog needs one real relaunch.
    What: re-lists panes and aborts if a live (non-shell) tm-architect pane
    now exists; creates the detached tmux session in ROOT when it is missing;
    runs session-relaunch.sh (shell -> `tm` -> /rename -> /remote-control ->
    /tm-session-resume); then confirms the pane left the shell.
    Test: test_relaunch_aborts_when_live, test_relaunch_creates_missing_session,
    test_relaunch_fails_on_script_error
    """
    lister, runner, tmux_fn = lister or list_panes, runner or subprocess.run, tmux_fn or tmux

    def live():
        mine = [p for p in lister() if p["session"] == SUPERVISOR]
        return mine, [p for p in mine if p["cmd"] not in SHELLS and not p["cmd"].startswith("-")]

    try:
        mine, alive = live()
        if alive:
            return "aborted", "a live claude process exists"
        if not mine:
            tmux_fn("new-session", "-d", "-s", SUPERVISOR, "-c", ROOT)
        r = runner([RELAUNCH_SCRIPT, SUPERVISOR], capture_output=True, text=True,
                   timeout=watchdog.RELAUNCH_TIMEOUT)
        if r.returncode != 0:
            return "failed", f"session-relaunch.sh exit {r.returncode}"
        return ("ok", "session-relaunch.sh exit 0") if live()[1] else (
            "failed", "pane still at a shell after relaunch")
    except subprocess.TimeoutExpired:
        return "failed", f"timeout after {watchdog.RELAUNCH_TIMEOUT}s"
    except (OSError, TmuxError) as e:
        return "failed", f"{type(e).__name__}: {e}"


def architect_transcript_mtime():
    """mtime of the Architect's newest interactive transcript, or None."""
    try:
        path = self_ctx.newest_transcript(SELF_CTX_DIR)
        return os.path.getmtime(path) if path else None
    except OSError:
        return None


def architect_busy(panes, capturer=None):
    """(busy, spinner text) of the Architect's live pane; (False, "") if none or unreadable.

    Why: the frozen check needs "a turn is running" and the spinner text, which
    only the pane shows. What: parse_pane on a read-only capture of the first
    non-shell tm-architect pane; no keys.
    Test: test_architect_busy_reads_spinner
    """
    capturer = capturer or capture
    for p in panes:
        if p["session"] == SUPERVISOR and p["cmd"] not in SHELLS and not p["cmd"].startswith("-"):
            try:
                info = parse_pane(capturer(p["pane"]))
            except TmuxError as e:
                log(f"architect capture ERROR {e}")
                return False, ""
            if DONE_SUMMARY in info["status"]:
                return False, ""  # completed turn at the prompt; a live Monitor row is not a turn
            return info["busy"], info["status"]
    return False, ""


def watchdog_alerts(state, panes, now, dry_run=False):
    """(alerts, log phrase) for the Architect watchdog this cycle."""
    book = state.setdefault("watchdog", {})
    busy, spinner = architect_busy(panes)
    found, phrase, relaunched = watchdog.step(
        book, panes, now, session=SUPERVISOR, shells=SHELLS,
        wake_at=state.get("wake_sent_at"), transcript_mtime=architect_transcript_mtime(),
        silent_min=watchdog.silent_minutes(), relauncher=run_architect_relaunch,
        dry_run=dry_run, busy=busy, spinner=spinner, frozen_min=watchdog.frozen_minutes())
    if relaunched:
        state["wake_sent_at"] = None  # the new session starts a fresh silent clock
    return [alert(k, f, d, SUPERVISOR, now=now) for k, f, d in found], phrase


# --- one cycle ---------------------------------------------------------------

def plan_window_renames(listing):
    """Return (session, index, old_name) for each tm-* window not named after its session.

    `listing` is `tmux list-windows -a -F '#{session_name}\t#{window_index}\t#{window_name}'`.
    """
    out = []
    for line in listing.splitlines():
        parts = line.split("\t")
        if len(parts) == 3 and parts[0].startswith("tm-") and parts[2] != parts[0]:
            out.append((parts[0], parts[1], parts[2]))
    return out


def rename_windows(dry_run=False, runner=None):
    """Name every tm-* window after its session (tmux-level, no keys). Never raises, never alerts."""
    runner = runner or tmux
    try:
        todo = plan_window_renames(runner(
            "list-windows", "-a", "-F", "#{session_name}\t#{window_index}\t#{window_name}"))
    except TmuxError as e:
        log(f"window-rename list ERROR {e}", dry_run)
        return 0
    done = 0
    for s, idx, old in todo:
        if dry_run:
            print(f"  would rename window {s}:{idx} {old!r} -> {s!r}")
            continue
        try:
            runner("rename-window", "-t", f"={s}:{idx}", s)
            log(f"window-rename {s}:{idx} {old!r} -> {s!r}")
            done += 1
        except TmuxError as e:
            log(f"window-rename {s}:{idx} ERROR {e}")
    return done


def architect_rc(state, panes, now, dry_run, candidates):
    """Run architect_rc_step on the live tm-architect pane. Returns (ran, fanout).
    Appends its alerts to `candidates`. A capture failure is logged, never raised."""
    sup = sorted((p for p in panes if p["session"] == SUPERVISOR
                  and p["cmd"] not in SHELLS and not p["cmd"].startswith("-")),
                 key=lambda p: p["active"] != "11")
    if not sup:
        return False, False
    pid = sup[0]["pane"]
    try:
        info = parse_pane(capture(pid))
    except TmuxError as e:
        log(f"architect rc capture ERROR {e}", dry_run)
        return False, False
    if dry_run:
        if info["rc_failed"]:
            print(f"  {SUPERVISOR} shows /rc failed: would run rc-restore.sh")
        return False, False
    found, ran, fanout = architect_rc_step(state, pid, info, now)
    candidates += found
    return ran, fanout


def cycle(state, dry_run=False, now=None):
    now = time.time() if now is None else now
    try:
        panes = list_panes()
    except TmuxError as e:
        log(f"panes=? ERROR {e}; cycle skipped", dry_run)
        return
    rename_windows(dry_run)
    state.setdefault("panes", {})
    watch_found, watch_phrase = watchdog_alerts(state, panes, now, dry_run)
    watched = [p for p in panes
               if p["session"].startswith("tm-") and p["session"] not in SKIP_SESSIONS]
    candidates, infos, current, idle_panes, live = [], [], {}, [], {}
    arch_ran, fanout = architect_rc(state, panes, now, dry_run, candidates)
    for p in watched:
        s, pid = p["session"], p["pane"]
        current[s] = pid
        ps = state["panes"].setdefault(f"{s}|{pid}", {})
        if p["cmd"] in SHELLS or p["cmd"].startswith("-"):
            ps["idle_since"] = None
            live[s] = {"state": "exited", "idle_min": 0}
            if not quiet_sessions.is_test_fixture_session(s):
                candidates.append(alert("exited", f"shell-{p['cmd']}",
                                        f"pane runs {p['cmd']}, not Claude", s, pid, now))
            continue
        try:
            info = parse_pane(capture(pid))
        except TmuxError as e:
            log(f"{s} {pid} capture ERROR {e}", dry_run)
            continue
        if info["ctx"] is None:  # compacted or truncated status line: measure the transcript
            info["ctx"] = transcript_ctx_pct(pid)
        infos.append((s, info))
        if (info["box"] in ("empty", "suggestion") and not info["busy"] and not info["dialog"]
                and not info["rc_failed"] and not info["limit"]):
            idle_panes.append((s, pid))  # a /usage probe may type here (usage.poll_step)
        found = evaluate(info, ps, now, s, pid)
        live[s] = pane_live(info, ps, now)
        if dry_run:
            if info["rc_failed"]:
                print(f"  {s} shows /rc failed: would run rc-restore.sh")
        else:
            forced = dict(info, rc_failed=True) if fanout else info
            found += rc_step(state, s, pid, forced, now)
        candidates += found
        if dry_run:
            print(f"  {s} {pid} cmd={p['cmd']} box={info['box']} agents={info['agents']} "
                  f"busy={info['busy']} dialog={bool(info['dialog'])} ctx={info['ctx']} "
                  f"plan={info['plan5h']}/{info['planwk']} q={bool(info['question'])} "
                  f"alerts={[a['kind'] for a in found] or '-'}")
    for s, pid in state.get("sessions", {}).items():
        if s not in current and not quiet_sessions.is_test_fixture_session(s):
            candidates.append(alert("exited", "gone", "session disappeared", s, pid, now))
    state["sessions"] = current
    live_keys = {f"{s}|{pid}" for s, pid in current.items()}  # not `live`: that dict feeds the dashboard
    state["panes"] = {k: v for k, v in state["panes"].items() if k in live_keys}
    state["rc"] = {k: v for k, v in state.get("rc", {}).items() if k in current or k == SUPERVISOR}
    candidates += fleet_plan_alerts(infos, now)
    candidates += usage_alerts(state, now, idle_panes, dry_run)
    candidates += token_alerts(now, dry_run)
    if not dry_run:
        feed = run_dashboard_feeder(live)
        da = dashboard_apply(feed, now) if feed else None
        if da:
            candidates.append(da)
    candidates += owner_answer_alerts(now)
    candidates += owner_feedback_alerts(now)
    candidates += owner_unpark_alerts(now)
    candidates += owner_command_alerts(now)
    candidates += owner_wont_do_alerts(now)
    candidates += owner_effort_alerts(now)
    candidates += owner_status_alerts(now)
    candidates += user_state_alerts(now)
    candidates += exited_step(state, panes, now, dry_run)
    candidates += read_events(state)
    candidates += host_alerts(now, state)
    candidates += self_ctx_alerts(now, dry_run)
    candidates += watch_found
    quiet = quiet_sessions.read_quiet_sessions(QUIET_SESSIONS_FILE)
    candidates = [a for a in candidates if not (a["kind"] == "idle" and a["session"] in quiet)]
    new, suppressed = dedup(state, candidates, now)
    if dry_run:
        for a in new:
            print(f"  would alert: {a['display']}")
            if a["kind"].startswith("architect_"):
                print(f"  would notify: {a['display']}")
    elif new:
        with open(ALERTS, "a") as f:
            for a in new:
                f.write(json.dumps(a, ensure_ascii=False) + "\n")
        for a in new:
            if a["kind"].startswith("architect_"):
                notify_macos("Architect watchdog", a["display"])
    woken, logged = triage_step(new, now, state, dry_run)
    if arch_ran:  # keys went into the Architect pane this cycle: wake next cycle
        state["wake_pending"] = state.get("wake_pending", 0) + len(woken)
        state["wake_logged"] = state.get("wake_logged", 0) + logged
        result = "deferred: architect rc-restore typed this cycle"
    else:
        result = dispatch_wake(state, panes, len(woken), dry_run, logged)
    log(f"panes={len(watched)} new={len(new)} woken={len(woken)} logged={logged} "
        f"suppressed={suppressed} wake={result} {watch_phrase}", dry_run)


def load_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def save_state(state):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


def main(argv=None):
    ap = argparse.ArgumentParser(description="Deterministic fleet poller.")
    ap.add_argument("--once", action="store_true", help="run one cycle and exit")
    ap.add_argument("--interval", type=int, default=60, help="seconds between cycles")
    ap.add_argument("--dry-run", action="store_true",
                    help="one cycle that captures and classifies only: prints to "
                         "stdout, writes no file, sends no keys")
    args = ap.parse_args(argv)
    if not args.dry_run:
        os.makedirs(INBOX, exist_ok=True)
    state = load_state()
    if args.dry_run:
        state = copy.deepcopy(state)
    else:
        log(f"poll start interval={args.interval}s")
    while True:
        try:
            cycle(state, args.dry_run)
            if not args.dry_run:
                save_state(state)
        except Exception as e:  # one bad cycle must not stop the poller
            log(f"ERROR {type(e).__name__}: {e}", args.dry_run)
        if args.once or args.dry_run:
            return
        time.sleep(args.interval)


if __name__ == "__main__":
    main()

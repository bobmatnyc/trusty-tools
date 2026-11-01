#!/usr/bin/env python3
"""fleet-poll.py — deterministic fleet poller; wakes the Architect only on need.

Why: the owner's ruling (2026-09-25) makes the Architect's polling deterministic.
A Claude session woke every 15 minutes and read every pane with inference even
when nothing needed action. This script decides without any LLM whether
anything needs the Architect, and wakes the Architect's pane only then.
What: every --interval seconds it lists the tmux panes (the default server's
and, unless TMUX_SOCKET pins one, every live `tm-ws-*` workspace server's,
#8928), captures each tm-*
PM pane, classifies it (input box via scripts/input-state.py `classify`,
dialog, question, idle, ctx, plan-limit, error, exited — see quiet_sessions,
below, for two exceptions), reads new permission_prompt / agent_needs_input
lines of inbox/events.jsonl, checks host load (5-min avg > LOAD_FACTOR x
cores), disk and swap, self_ctx_alerts (self-ctx.py: the Architect's own
context in tokens, which has no `ctx N%` line to read, against the same
context bar), owner_answer_alerts (dashboard answers not yet relayed), and
appends each NEW alert as a JSON line to inbox/alerts.jsonl. Dedup state is inbox/poll-state.json: an
alert with the same (session, pane, kind, fingerprint) is not re-emitted
until it clears for one cycle, or its kind's re-emit interval passes.
`dispatch_wake` types the one-line pointer into the Architect's pane and
sends keys to no other pane. quiet_sessions.py drops "idle" for a parked
session and "exited" for a disposable test-fixture one. The pure parsing and
alert evaluation live in scripts/fleet-classify.py; the settings, I/O, alert
steps and wake live in scripts/fleet_poll_{cfg,io,alerts,wake}.py, and the
self-refresh step in fleet_poll_selfrefresh.py (#10571). This file keeps the
cycle, the state file and main, and re-exports every moved name. Each cycle
also runs the Architect watchdog (fleet_poll_watchdog.py, architect-watchdog.py),
the Intent Ledger cadence jobs (il-cron.py) and the clear-signal check
(ctx-watch.py), and appends their findings as alerts.
Config (env, all optional): ARCHITECT_SESSION (default tm-architect),
ARCHITECT_POLL_SESSION (default <ARCHITECT_SESSION>-poll),
ARCHITECT_INBOX_DIR (default <root>/inbox), ARCHITECT_QUIET_SESSIONS_FILE
(default <root>/quiet-sessions.txt), TMUX_SOCKET, LOAD_FACTOR, SELF_CTX_DIR,
CONTEXT_BAR_TOKENS (default 250000), CONTEXT_WINDOW_TOKENS (default 1000000);
see README.md.
Test: tests/test_fleet_poll.py, tests/test_quiet_sessions.py,
tests/test_fleet_poll_workspace_servers.py, tests/test_fleet_poll_split.py,
tests/test_fleet_poll_lock.py, tests/test_fleet_poll_owner_answers.py
Usage: scripts/fleet-poll.py [--once] [--interval N] [--dry-run]
       (normally run via scripts/start-fleet-poll.sh)
Exit: 0 after --once or --dry-run; 3 when another live poller holds
inbox/poll.lock (checked before the first log line or state write).
"""
import argparse
import copy
import importlib.util
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))

# #9997: each context-bar warning is logged once per poller process.
_LOGGED_BAR_WARNINGS = set()
_HELD_LOCK = []  # #10571: the poll.lock fd main() holds


def _load_by_path(name, filename):
    # A hyphenated script name isn't importable, so load it by path.
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class _Poller:
    """Late-bound view of this module's globals, handed to each fleet_poll_* module (#10571).

    A moved function reads the poller's names when it runs, not when it loads, so rebinding
    `fleet-poll.tmux`, `fleet-poll.LOG` or `fleet-poll.wake` still steers it, as before the split.
    """

    def __getattr__(self, name):
        try:
            return globals()[name]
        except KeyError:
            raise AttributeError(name) from None


# Settings (#10571 split). These names stay fleet-poll attributes.
_cfg = _load_by_path("fleet_poll_cfg", "fleet_poll_cfg.py")
ROOT, INBOX, EVENTS, ALERTS = _cfg.ROOT, _cfg.INBOX, _cfg.EVENTS, _cfg.ALERTS
STATE, LOG, SOCKET, WORKSPACE_PREFIX = _cfg.STATE, _cfg.LOG, _cfg.SOCKET, _cfg.WORKSPACE_PREFIX
LOCK = _cfg.LOCK
ARCHITECT, POLL_SESSION, SKIP_SESSIONS = _cfg.ARCHITECT, _cfg.POLL_SESSION, _cfg.SKIP_SESSIONS
SHELLS, EVENT_TYPES = _cfg.SHELLS, _cfg.EVENT_TYPES
DISK_HIGH, LOAD_FACTOR_DEFAULT = _cfg.DISK_HIGH, _cfg.LOAD_FACTOR_DEFAULT
QUIET_SESSIONS_FILE, RELAUNCH_SCRIPT = _cfg.QUIET_SESSIONS_FILE, _cfg.RELAUNCH_SCRIPT

# Pure pane parsing and alert evaluation live in fleet-classify.py (#8891).
# These names stay fleet-poll attributes, as they were before the split.
fleet_classify = _load_by_path("fleet_classify", "fleet-classify.py")
input_state = fleet_classify.input_state
IDLE_AFTER, REEMIT = fleet_classify.IDLE_AFTER, fleet_classify.REEMIT
fp, iso, alert = fleet_classify.fp, fleet_classify.iso, fleet_classify.alert
find_box, _screen_lines = fleet_classify.find_box, fleet_classify._screen_lines
parse_pane, evaluate = fleet_classify.parse_pane, fleet_classify.evaluate
fleet_plan_alerts, dedup = fleet_classify.fleet_plan_alerts, fleet_classify.dedup
clean = fleet_classify.clean

self_ctx = _load_by_path("self_ctx", "self-ctx.py")
quiet_sessions = _load_by_path("quiet_sessions", "quiet-sessions.py")
owner_answers = _load_by_path("owner_answers", "owner_answers.py")  # #10670: the answer store
watchdog = _load_by_path("architect_watchdog", "architect-watchdog.py")
il_cron = _load_by_path("il_cron", "il-cron.py")
ctx_watch = il_cron.ctx_watch
SELF_CTX_DIR = os.environ.get("SELF_CTX_DIR") or self_ctx.default_dir()

# I/O, alert steps, wake and self-refresh (#10571 split), each bound to this module's names.
_io = _load_by_path("fleet_poll_io", "fleet_poll_io.py")
_alerts = _load_by_path("fleet_poll_alerts", "fleet_poll_alerts.py")
_wake = _load_by_path("fleet_poll_wake", "fleet_poll_wake.py")
_watchdog = _load_by_path("fleet_poll_watchdog", "fleet_poll_watchdog.py")
_selfrefresh = _load_by_path("fleet_poll_selfrefresh", "fleet_poll_selfrefresh.py")
for _mod in (_io, _alerts, _wake, _watchdog, _selfrefresh):
    _mod.bind(_Poller())
TmuxError, log, tmux, socket_dir = _io.TmuxError, _io.log, _io.tmux, _io.socket_dir
workspace_servers, list_panes, _rows = _io.workspace_servers, _io.list_panes, _io._rows
capture, capture_pane, send_to_architect = _io.capture, _io.capture_pane, _io.send_to_architect
read_events, swap_used_mb, host_alerts = _io.read_events, _io.swap_used_mb, _io.host_alerts
acquire_poll_lock = _io.acquire_poll_lock
resolve_context_bar, self_ctx_alerts = _alerts.resolve_context_bar, _alerts.self_ctx_alerts
il_cron_alerts, ctx_watch_alerts = _alerts.il_cron_alerts, _alerts.ctx_watch_alerts
owner_answer_alerts, ANSWER_KINDS = _alerts.owner_answer_alerts, _alerts.ANSWER_KINDS
ANSWER_KEY = _alerts.ANSWER_KEY
run_architect_relaunch, architect_busy = _watchdog.run_architect_relaunch, _watchdog.architect_busy
architect_transcript_mtime, watchdog_alerts = _watchdog.architect_transcript_mtime, _watchdog.watchdog_alerts
_norm, _box_full_text, _own_pointer = _wake._norm, _wake._box_full_text, _wake._own_pointer
wake, dispatch_wake, self_clear_running = _wake.wake, _wake.dispatch_wake, _wake.self_clear_running
self_refresh_step = _selfrefresh.self_refresh_step


# --- one cycle ---------------------------------------------------------------

def cycle(state, dry_run=False, now=None):
    now = time.time() if now is None else now
    try:
        panes = list_panes()
    except TmuxError as e:
        log(f"panes=? ERROR {e}; cycle skipped", dry_run)
        return
    state.setdefault("panes", {})
    watch_found, watch_phrase = watchdog_alerts(state, panes, now, dry_run)  # #10571 (prototype :1889)
    bar = resolve_context_bar(dry_run)
    watched = [p for p in panes
               if p["session"].startswith("tm-") and p["session"] not in SKIP_SESSIONS]
    candidates, infos, current = [], [], {}
    for p in watched:
        s, pid = p["session"], p["pane"]
        current[s] = pid
        pane_st = state["panes"].setdefault(f"{s}|{pid}", {})
        if p["cmd"] in SHELLS or p["cmd"].startswith("-"):
            pane_st["idle_since"] = None
            if not quiet_sessions.is_test_fixture_session(s):
                candidates.append(alert("exited", f"shell-{p['cmd']}",
                                        f"pane runs {p['cmd']}, not Claude", s, pid, now))
            continue
        try:
            info = parse_pane(capture_pane(p))
        except TmuxError as e:
            log(f"{s} {pid} capture ERROR {e}", dry_run)
            continue
        infos.append((s, info))
        found = evaluate(info, pane_st, now, s, pid, bar)
        candidates += found
        if dry_run:
            print(f"  {s} {pid} cmd={p['cmd']} box={info['box']} agents={info['agents']} "
                  f"busy={info['busy']} turn_active={info['turn_active']} "
                  f"dialog={bool(info['dialog'])} ctx={info['ctx']} "
                  f"plan={info['plan5h']}/{info['planwk']} q={bool(info['question'])} "
                  f"alerts={[a['kind'] for a in found] or '-'}")
    for s, pid in state.get("sessions", {}).items():
        if s not in current and not quiet_sessions.is_test_fixture_session(s):
            candidates.append(alert("exited", "gone", "session disappeared", s, pid, now))
    state["sessions"] = current
    live = {f"{s}|{pid}" for s, pid in current.items()}
    state["panes"] = {k: v for k, v in state["panes"].items() if k in live}
    candidates += fleet_plan_alerts(infos, now)
    candidates += il_cron_alerts(now, dry_run)      # #10571 (prototype :1942)
    candidates += ctx_watch_alerts(now, dry_run)    # #10571 (prototype :1943)
    candidates += read_events(state)
    candidates += host_alerts(now)
    candidates += self_ctx_alerts(now, dry_run)
    candidates += watch_found
    answers, failure = owner_answer_alerts(now, dry_run, ever_read=state.get("answers_read", False))
    if answers is not None:
        state["answers_read"] = True
    candidates += answers or []
    if failure:
        # #10670: a failed read must reach the Architect; owner_* always wakes (alert_triage).
        # The fingerprint is the error class, so one failure episode alerts once.
        candidates.append(alert("owner_answer_error", failure,
                                f"cannot read owner answers ({failure}); see poll.log", now=now))
    # #10670: dedup clears a key absent from a cycle. A failed read says nothing about an
    # answer, so hold its keys: it stays unconsumed and keeps its re-emit clock.
    held = {} if answers is not None else {
        k: v for k, v in state.get("alerts", {}).items() if k.startswith(ANSWER_KEY)}
    quiet = quiet_sessions.read_quiet_sessions(QUIET_SESSIONS_FILE)
    candidates = [a for a in candidates if not (a["kind"] == "idle" and a["session"] in quiet)]
    new, suppressed = dedup(state, candidates, now)
    state["alerts"].update(held)
    if dry_run:
        for a in new:
            print(f"  would alert: {a['display']}")
    elif new:
        with open(ALERTS, "a") as f:
            for a in new:
                f.write(json.dumps(a, ensure_ascii=False) + "\n")
    result = dispatch_wake(state, panes, len(new), dry_run,
                           sum(1 for a in new if a["kind"] == "owner_answer"))
    # #10599 PR-1's slot (prototype :2000): never type into the box twice in a cycle.
    if not result.startswith("sent"):
        watch_phrase = f"{watch_phrase} {self_refresh_step(state, panes, now, dry_run)}".rstrip()
    log(f"panes={len(watched)} new={len(new)} suppressed={suppressed} wake={result} {watch_phrase}".rstrip(),
        dry_run)


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
        # #10571: a second live poller stops here, before its first log line or state write.
        held, detail = acquire_poll_lock(LOCK)
        if held == "busy":
            print(f"fleet-poll: {detail}; exiting", file=sys.stderr)
            return 3
        if held == "unknown":
            log(f"WARNING poll lock: {detail}; polling without it")
        else:
            _HELD_LOCK.append(detail)  # the fd: the lock lasts as long as this process
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
            return 0
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())

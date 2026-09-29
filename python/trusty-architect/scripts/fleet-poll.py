#!/usr/bin/env python3
"""fleet-poll.py — deterministic fleet poller; wakes the Architect only on need.

Why: the owner's ruling (2026-09-25) makes the Architect's polling deterministic.
A Claude session woke every 15 minutes and read every pane with inference even
when nothing needed action. This script decides without any LLM whether
anything needs the Architect, and wakes the Architect's pane only then.
What: every --interval seconds it lists the tmux panes, captures each tm-*
PM pane, classifies it (input box via scripts/input-state.py `classify`,
dialog, question, idle, ctx, plan-limit, error, exited — see quiet_sessions,
below, for two exceptions), reads new permission_prompt / agent_needs_input
lines of inbox/events.jsonl, checks host load (5-min avg > LOAD_FACTOR x
cores), disk and swap, self_ctx_alerts (self-ctx.py: the Architect's own
context, which has no `ctx N%` line to read), and appends each NEW alert as
a JSON line to inbox/alerts.jsonl. Dedup state is inbox/poll-state.json: an
alert with the same (session, pane, kind, fingerprint) is not re-emitted
until it clears for one cycle, or its kind's re-emit interval passes.
`dispatch_wake` types the one-line pointer into the Architect's pane and
sends keys to no other pane. quiet_sessions.py drops "idle" for a parked
session and "exited" for a disposable test-fixture one. The pure parsing and
alert evaluation live in scripts/fleet-classify.py; this file does the I/O.
Config (env, all optional): ARCHITECT_SESSION (default tm-architect),
ARCHITECT_POLL_SESSION (default <ARCHITECT_SESSION>-poll),
ARCHITECT_INBOX_DIR (default <root>/inbox), ARCHITECT_QUIET_SESSIONS_FILE
(default <root>/quiet-sessions.txt), TMUX_SOCKET, LOAD_FACTOR, SELF_CTX_DIR,
SELF_CTX_WINDOW, SELF_CTX_THRESHOLD; see README.md.
Test: tests/test_fleet_poll.py, tests/test_quiet_sessions.py
Usage: scripts/fleet-poll.py [--once] [--interval N] [--dry-run]
       (normally run via scripts/start-fleet-poll.sh)
"""
import argparse
import copy
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
# ARCHITECT_INBOX_DIR relocates the queues and logs, e.g. in tests.
INBOX = os.environ.get("ARCHITECT_INBOX_DIR") or os.path.join(ROOT, "inbox")
EVENTS = os.path.join(INBOX, "events.jsonl")
ALERTS = os.path.join(INBOX, "alerts.jsonl")
STATE = os.path.join(INBOX, "poll-state.json")
LOG = os.path.join(INBOX, "poll.log")
SOCKET = os.environ.get("TMUX_SOCKET", "")

# The Architect's own tmux session: the only pane this poller sends keys to.
ARCHITECT = os.environ.get("ARCHITECT_SESSION") or "tm-architect"
POLL_SESSION = os.environ.get("ARCHITECT_POLL_SESSION") or f"{ARCHITECT}-poll"
SKIP_SESSIONS = {ARCHITECT, POLL_SESSION}
SHELLS = {"bash", "zsh", "sh", "fish", "dash", "ksh", "tcsh", "csh", "login"}
EVENT_TYPES = {"permission_prompt", "agent_needs_input"}  # idle_prompt is not an alert

DISK_HIGH = 90
LOAD_FACTOR_DEFAULT = 1.5  # x cores, vs. the 5-min load avg; env LOAD_FACTOR overrides

SELF_CTX_WINDOW_DEFAULT = 1_000_000
SELF_CTX_THRESHOLD_DEFAULT = 40


def _load_by_path(name, filename):
    # A hyphenated script name isn't importable, so load it by path.
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# Pure pane parsing and alert evaluation live in fleet-classify.py (#8891).
# These names stay fleet-poll attributes, as they were before the split.
fleet_classify = _load_by_path("fleet_classify", "fleet-classify.py")
input_state = fleet_classify.input_state
IDLE_AFTER, REEMIT = fleet_classify.IDLE_AFTER, fleet_classify.REEMIT
fp, iso, alert = fleet_classify.fp, fleet_classify.iso, fleet_classify.alert
find_box, _screen_lines = fleet_classify.find_box, fleet_classify._screen_lines
parse_pane, evaluate = fleet_classify.parse_pane, fleet_classify.evaluate
fleet_plan_alerts, dedup = fleet_classify.fleet_plan_alerts, fleet_classify.dedup

self_ctx = _load_by_path("self_ctx", "self-ctx.py")
quiet_sessions = _load_by_path("quiet_sessions", "quiet-sessions.py")
SELF_CTX_DIR = os.environ.get("SELF_CTX_DIR") or self_ctx.default_dir()
QUIET_SESSIONS_FILE = (os.environ.get("ARCHITECT_QUIET_SESSIONS_FILE")
                       or os.path.join(ROOT, "quiet-sessions.txt"))


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
               "#{session_name}\t#{pane_id}\t#{pane_current_command}\t#{window_active}#{pane_active}")
    panes = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) == 4:
            panes.append(dict(zip(("session", "pane", "cmd", "active"), parts)))
    return panes


def capture(pane):
    return tmux("capture-pane", "-e", "-p", "-S", "-80", "-t", pane)


def send_to_architect(pane, *keys):
    """The only send path. Re-checks that `pane` belongs to the Architect first."""
    owner = tmux("display-message", "-p", "-t", pane, "#{session_name}").strip()
    if owner != ARCHITECT:
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


def host_alerts(now):
    out = []
    load1, load5, load15 = os.getloadavg()
    cores, factor = os.cpu_count() or 1, float(os.environ.get("LOAD_FACTOR", LOAD_FACTOR_DEFAULT))
    if load5 > factor * cores:
        out.append(alert("load", "load", f"load {load1:.1f}/{load5:.1f}/{load15:.1f} "
                         f"(5m > {factor}x{cores} cores)", now=now))
    du = shutil.disk_usage("/")
    pct = 100 * du.used / du.total
    if pct > DISK_HIGH:
        out.append(alert("disk", "disk", f"root disk {pct:.0f}% used", now=now))
    try:
        swap = swap_used_mb()
    except (OSError, subprocess.SubprocessError, ValueError):
        swap = None
    if swap:
        out.append(alert("swap", "swap", f"swap in use: {swap:.0f} MB", now=now))
    return out


def self_ctx_alerts(now, dry_run=False):
    """One "self_ctx" alert when the Architect's own context (self_ctx.py,
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
                  ARCHITECT, now=now)]


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
    sup = sorted((p for p in panes if p["session"] == ARCHITECT),
                 key=lambda p: p["active"] != "11")
    if not sup:
        return f"skipped: no {ARCHITECT} pane"
    target = sup[0]["pane"]
    raw = capture(target)
    info = parse_pane(raw)
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
        if _own_pointer(_box_full_text(raw)):
            return "skipped: own stale pointer in box"
        return f"skipped: input {info['box']}"
    msg = f"[poll {datetime.now(timezone.utc):%H:%M}Z] {n} new alerts -> {ALERTS}"
    if dry_run:
        return f"dry-run: would send to {target}: {msg}"
    send_to_architect(target, "-l", msg)
    time.sleep(0.5)
    if _norm(msg) not in _box_full_text(capture(target)):
        send_to_architect(target, *(["BSpace"] * len(msg)))
        time.sleep(0.3)
        empty = input_state.classify(capture(target))[0] in ("empty", "suggestion")
        return ("failed: text not confirmed in the input box; cleaned up, box empty"
                 if empty else
                 "failed: text not confirmed in the input box; cleanup left box non-empty")
    send_to_architect(target, "Enter")
    time.sleep(1)
    left = input_state.classify(capture(target))[1]
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
        result = wake(state["wake_pending"], panes, dry_run)
    except TmuxError as e:
        result = f"failed: {e}"
    if result.startswith("sent"):
        state["wake_pending"] = 0
    return result


# --- one cycle ---------------------------------------------------------------

def cycle(state, dry_run=False, now=None):
    now = time.time() if now is None else now
    try:
        panes = list_panes()
    except TmuxError as e:
        log(f"panes=? ERROR {e}; cycle skipped", dry_run)
        return
    state.setdefault("panes", {})
    watched = [p for p in panes
               if p["session"].startswith("tm-") and p["session"] not in SKIP_SESSIONS]
    candidates, infos, current = [], [], {}
    for p in watched:
        s, pid = p["session"], p["pane"]
        current[s] = pid
        ps = state["panes"].setdefault(f"{s}|{pid}", {})
        if p["cmd"] in SHELLS or p["cmd"].startswith("-"):
            ps["idle_since"] = None
            if not quiet_sessions.is_test_fixture_session(s):
                candidates.append(alert("exited", f"shell-{p['cmd']}",
                                        f"pane runs {p['cmd']}, not Claude", s, pid, now))
            continue
        try:
            info = parse_pane(capture(pid))
        except TmuxError as e:
            log(f"{s} {pid} capture ERROR {e}", dry_run)
            continue
        infos.append((s, info))
        found = evaluate(info, ps, now, s, pid)
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
    live = {f"{s}|{pid}" for s, pid in current.items()}
    state["panes"] = {k: v for k, v in state["panes"].items() if k in live}
    candidates += fleet_plan_alerts(infos, now)
    candidates += read_events(state)
    candidates += host_alerts(now)
    candidates += self_ctx_alerts(now, dry_run)
    quiet = quiet_sessions.read_quiet_sessions(QUIET_SESSIONS_FILE)
    candidates = [a for a in candidates if not (a["kind"] == "idle" and a["session"] in quiet)]
    new, suppressed = dedup(state, candidates, now)
    if dry_run:
        for a in new:
            print(f"  would alert: {a['display']}")
    elif new:
        with open(ALERTS, "a") as f:
            for a in new:
                f.write(json.dumps(a, ensure_ascii=False) + "\n")
    result = dispatch_wake(state, panes, len(new), dry_run)
    log(f"panes={len(watched)} new={len(new)} suppressed={suppressed} wake={result}", dry_run)


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

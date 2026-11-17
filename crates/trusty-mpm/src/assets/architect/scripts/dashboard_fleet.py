"""dashboard_fleet.py: the Live view's fleet and session-status rules (split from dashboard_harness.py, #10571;
bodies moved verbatim).

Why: dashboard_harness.py passed the 500-SLOC cap; the status mapping, the tmux probe and GET /api/fleet are one
cohesive block.
What: collect_messages, resolve_target, dir_mtime, architect_state, map_state, the four STATUSES with
session_status and fleet_status (POLLER, the Architect's own poller session, is never a managed session),
tmux_sessions, and the factory task helpers. dashboard_harness.py re-exports every name.
Test: tests/test_dashboard_harness.py, tests/test_dashboard_routes.py
"""
import glob
import os
import re
import subprocess
import time
from datetime import datetime, timezone

from dashboard_harness_base import ACTIVITY_S, IDLE_PARK_MIN, SLOW_TOKENS_1H, WINDOW_S, _iso, one_line, tail_json


def collect_messages(inbox, now):
    """(messages, sources) from pm-messages (in) and relays (out), newest last, within WINDOW_S."""
    msgs, src = [], {}
    for name, fname, direction in (("messages", "pm-messages.jsonl", "in"), ("relays", "relays.jsonl", "out")):
        rows = tail_json(os.path.join(inbox, fname))
        if rows is None:
            src[name] = "missing"
            continue
        n = 0
        for r in rows:
            t = _iso(r.get("ts"))
            if t is None or now - t > WINDOW_S or not r.get("session"):
                continue
            n += 1
            msgs.append({"ts": r["ts"], "epoch": int(t), "dir": direction, "session": str(r["session"]),
                         "kind": str(r.get("kind") or ("relay" if direction == "out" else "msg")), "text": one_line(r.get("text"))})
        src[name] = "ok" if n else "empty"
    msgs.sort(key=lambda m: m["epoch"])
    return msgs, src


def resolve_target(to, names):
    """The tmux session a SendMessage `to` names (exact, tm-<to>, or name minus -NN); the raw value when none match."""
    if to in names:
        return to
    for n in names:
        if n == "tm-" + to or re.sub(r"-\d+$", "", n) in (to, "tm-" + to):
            return n
    return to


def dir_mtime(projects_dir, d):
    """Newest write time under a transcript project dir (main transcripts and subagent transcripts); 0 when none."""
    best = 0
    try:
        for e in os.scandir(os.path.join(projects_dir, d)):
            if e.is_file() and e.name.endswith(".jsonl"):
                best = max(best, e.stat().st_mtime)
            elif e.is_dir():
                for p in glob.glob(os.path.join(e.path, "subagents", "agent-*.jsonl")):
                    best = max(best, os.stat(p).st_mtime)
    except OSError:
        pass
    return best


def architect_state(last_write, last_msg, user_state, now):
    """ok when its transcript or a message changed within ACTIVITY_S; park only when sleeping and idle that long; else slow."""
    if max(last_write or 0, last_msg or 0) >= now - ACTIVITY_S:
        return "ok"
    return "park" if user_state == "sleeping" else "slow"


def map_state(raw, idle_min, tokens_1h, blocked, agents=(), messaged=False):
    if blocked:
        return "stop"
    # ar-Tf9: a session whose subagents are writing transcripts is working even when the main thread reads idle.
    live_ag = [a for a in agents if a.get("state") in ("ok", "slow")]
    if live_ag:
        return "ok" if any(a["state"] == "ok" for a in live_ag) else "slow"
    if raw is None:
        return "slow" if messaged else "park"
    if raw == "working":
        return "slow" if tokens_1h is not None and tokens_1h < SLOW_TOKENS_1H else "ok"
    return "slow" if messaged or (idle_min or 0) <= IDLE_PARK_MIN else "park"


STATUSES = ("working", "waiting", "paused", "inactive")
# The Architect's own poller is infrastructure, never a managed session; named as fleet_poll_cfg.py names it (#10571).
POLLER = (os.environ.get("ARCHITECT_POLL_SESSION")
          or f"{os.environ.get('ARCHITECT_SESSION') or 'tm-architect'}-poll")
PAUSED_DOC = ("parked", "refreshing")   # doc states meaning "deliberately held, session alive"
GONE_KEEP_S = 24 * 3600            # a session doc whose tmux session is gone stays listed (inactive) this long
def tmux_sessions(run=subprocess.run):
    """Set of live tmux session names (`tmux ls`), or None when tmux cannot be asked (no server counts as an empty set)."""
    try:
        r = run(["tmux", "list-sessions", "-F", "#{session_name}"], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.SubprocessError):
        return None
    if r.returncode != 0:
        return set() if "no server running" in (r.stderr or "") else None
    return {x.strip() for x in r.stdout.splitlines() if x.strip()}


def session_status(doc_state, alive, raw=None, agents_running=False):
    """One of STATUSES. inactive: no live tmux session, or its pane no longer runs Claude (raw "exited").
    paused: a live session the Architect or the owner parked or is refreshing (doc state). working: a live pane or subagent is running a tool.
    waiting: everything else alive (idle at the prompt, blocked, waiting on a decision or CI). Test: scripts/test_dashboard_status.py"""
    if not alive or raw == "exited":
        return "inactive"
    if doc_state in PAUSED_DOC:
        return "paused"
    if agents_running or raw == "working" or (raw is None and doc_state == "working"):
        return "working"
    return "waiting"


def _iso_epoch(v):
    try:
        return datetime.strptime(str(v)[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    except ValueError:
        return 0.0


def fleet_status(session_docs, goal_docs, tmux, live, now=None):
    """{"sessions": {name: {status, alive, managed}}, "count", "inactive"}. Managed = a live `tm-*` tmux session (the poller excluded), plus a
    session doc with no live tmux session that changed within GONE_KEEP_S (a session that just ended stays visible as inactive, then retires).
    With tmux unknown (None) a name counts as alive when the poller's live file reports it. goal_docs is unused and kept for callers.
    Test: scripts/test_dashboard_status.py"""
    now = time.time() if now is None else now
    live = live or {}
    docs = {d.get("name"): d for d in session_docs if isinstance(d, dict) and d.get("name")}
    names = set(docs) | set(live) | (tmux or set())
    out = {}
    for n in sorted(names):
        if not n.startswith("tm-") or n == POLLER:
            continue
        alive = (n in tmux) if tmux is not None else n in live
        doc = docs.get(n) or {}
        st = session_status(doc.get("state"), alive, (live.get(n) or {}).get("state"))
        if alive or (n in docs and now - _iso_epoch(doc.get("updated")) <= GONE_KEEP_S):
            out[n] = {"status": st, "alive": alive, "managed": True}
    return {"sessions": out, "count": len(out), "inactive": sum(1 for v in out.values() if v["status"] == "inactive")}


FACTORY_TASK = "Monitor boards for roadmap and bug/maintenance issues and address"   # the owner ruling 2026-10-24: the standing task of every factory session
FACTORY_DONE = ("done", "closed", "parked", "archived")
def factory_sessions(goal_docs):
    """Session names listed by a live goal of kind 'factory' (the dashboard store records factory this way, not by name)."""
    return {s for g in goal_docs or () if isinstance(g, dict) and g.get("kind") == "factory" and g.get("lifecycle") not in FACTORY_DONE
            for s in g.get("sessions") or () if isinstance(s, str)}


def session_tasks(name, item_docs, factory):
    """[{id, title, state}] for the pie: the session's task items (state open|blocked|done), plus the standing factory task
    (state 'factory') for a factory session. Withdrawn tasks (wont_do) are left out."""
    out = [{"id": "factory", "title": FACTORY_TASK, "state": "factory"}] if name in factory else []
    for i in item_docs:
        if not isinstance(i, dict) or i.get("kind") != "task" or i.get("session") != name or i.get("status") not in ("open", "done"):
            continue
        st = "done" if i["status"] == "done" else "blocked" if i.get("blocks_session") else "open"
        out.append({"id": str(i.get("id") or ""), "title": one_line(i.get("title"), 80), "state": st})
    return out



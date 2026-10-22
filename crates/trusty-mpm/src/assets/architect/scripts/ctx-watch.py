#!/usr/bin/env python3
"""ctx-watch.py: evaluate clear signals S1-S5 per session (SPEC-IL-01 section 7).

Why: continuous context management needs a deterministic, no-LLM decision of
which sessions are clear candidates (the owner 2026-10-17 04:14Z).
What: per session listed in local/il-schedule.json it reads the IL session record
(`<project>/.trusty-mpm/datg/[<session>/]state.json`), the session's own transcript
(live registry in SESSIONS_DIR -> TRANSCRIPT_BASE/*/<sid>.jsonl, last assistant usage:
ctx = input + cache_read + cache_creation) and, when the IL holds no ctx_use, the last
`CTX-USE:` line in the transcript. It prints candidates (JSON lines, or --json list).
Side effects: only inbox/ctx-watch-state.json (debounce), local/usage/ctx-proxy.jsonl
(calibration rows, never a trigger) and nothing else. It never sends keys.
Signals: S1 boundary + ctx >= floor + self-reported stale >= limit; S2 ctx >= bar at a
boundary (action "ask-boundary" from the approach line up); S3 idle 10-50 min of the cache
TTL at a boundary; S4 restored idle history (once per sessionId); S5 ctx >= guard.
"At a boundary" = IL boundary_at is set and the last assistant turn is no later than
boundary_at + boundary_slack_s (boundary_at has minute resolution).
Thresholds come from il-schedule.json "thresholds" (+ per-session "thresholds").
CLI: ctx-watch.py [--json] [--now ISO] [--no-state] [--session NAME]
Env (tests): IL_SCHEDULE, SESSIONS_DIR, TRANSCRIPT_BASE, SUPERVISOR_INBOX_DIR,
CTX_PROXY_LOG, CTX_CYCLE_JSONL.
Test: scripts/test_ctx_watch.py
"""
import argparse
import glob
import importlib.util
import json
import os
import re
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


self_ctx = _load("self_ctx", "self-ctx.py")

DEFAULT_THRESHOLDS = {
    "floor_ctx": 150_000, "stale_pct": 70, "bar_ctx": 250_000, "approach_ctx": 220_000,
    "guard_ctx": 400_000, "s2_refire_s": 900, "s3_min_idle_s": 600, "s3_max_idle_s": 3000,
    "s4_idle_s": 3600, "boundary_slack_s": 180, "fixed_base": 83_000,
    "il_max_age_s": 900, "rate_per_hour": 1, "rate_per_day": 6, "s2_after_clear_s": 1800,
}
CTX_USE_RE = re.compile(r"CTX-USE:\s*active\s*~(\d{1,2})%\s*·\s*stale\s*~(\d{1,2})%"
                        r"(?:\s*·\s*keep:\s*(.*))?")


def paths():
    inbox = os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox")
    return {
        "schedule": os.environ.get("IL_SCHEDULE") or os.path.join(ROOT, "local", "il-schedule.json"),
        "state": os.path.join(inbox, "ctx-watch-state.json"),
        "proxy": os.environ.get("CTX_PROXY_LOG") or os.path.join(ROOT, "local", "usage", "ctx-proxy.jsonl"),
        "cycle_jsonl": os.environ.get("CTX_CYCLE_JSONL")
        or os.path.join(ROOT, "local", "usage", "ctx-cycle.jsonl"),
        "sessions_dir": os.environ.get("SESSIONS_DIR") or os.path.expanduser(
            "~/.trusty-tools/trusty-mpm/claude-config/sessions"),
        "transcripts": os.environ.get("TRANSCRIPT_BASE") or os.path.expanduser(
            "~/.trusty-tools/trusty-mpm/claude-config/projects"),
    }


def parse_iso(s):
    """ISO string (minute or second resolution, Z or offset) -> epoch seconds, or None."""
    try:
        d = datetime.fromisoformat(str(s).replace("Z", "+00:00"))
    except (ValueError, TypeError):
        return None
    return (d if d.tzinfo else d.replace(tzinfo=timezone.utc)).timestamp()


def iso(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def load_schedule(path=None):
    return load_json(path or paths()["schedule"], {})


def thresholds_for(sched, entry):
    t = dict(DEFAULT_THRESHOLDS)
    t.update(sched.get("thresholds") or {})
    t.update((entry or {}).get("thresholds") or {})
    return t


def datg_dir(project, session):
    """Per-PM directory `datg/<session>/` when present (shared project, spec Q3), else `datg/`."""
    base = os.path.join(project, ".trusty-mpm", "datg")
    per = os.path.join(base, session)
    return per if os.path.isdir(per) else base


def state_path_for(entry):
    return entry.get("state_path") or os.path.join(
        datg_dir(entry["project"], entry["name"]), "state.json")


def read_il(entry):
    """The IL `session` record of this session ({} when absent) and whether state.json read."""
    st = load_json(state_path_for(entry), None)
    if not isinstance(st, dict):
        return {}, False
    sess = st.get("session")
    return (sess if isinstance(sess, dict) else {}), True


# --- transcript access -------------------------------------------------------

def registry_entry(session, sessions_dir=None):
    """Newest live registry entry whose tmux field is `<session>:...`, or None."""
    sessions_dir = sessions_dir or paths()["sessions_dir"]
    best = None
    try:
        names = os.listdir(sessions_dir)
    except OSError:
        return None
    for n in names:
        if not n.endswith(".json"):
            continue
        try:
            with open(os.path.join(sessions_dir, n)) as f:
                d = json.load(f)
            if not str(d.get("tmux", "")).startswith(session + ":"):
                continue
            try:
                os.kill(int(d["pid"]), 0)
            except PermissionError:
                pass
            if best is None or (d.get("updatedAt") or 0) >= (best.get("updatedAt") or 0):
                best = d
        except (OSError, ValueError, TypeError, KeyError):
            continue
    return best


def transcript_for(sid, base=None):
    base = base or paths()["transcripts"]
    matches = glob.glob(os.path.join(glob.escape(base), "*", f"{glob.escape(sid)}.jsonl"))
    return max(matches, key=os.path.getmtime) if matches else None


def _ctx_of(usage):
    return ((usage.get("input_tokens") or 0) + (usage.get("cache_read_input_tokens") or 0)
            + (usage.get("cache_creation_input_tokens") or 0))


def last_turn(path):
    """(ctx tokens, epoch of the last assistant turn) from the transcript tail, or (None, None)."""
    def want(o):
        if o.get("type") != "assistant":
            return None
        u = (o.get("message") or {}).get("usage")
        return {"ctx": _ctx_of(u), "ts": parse_iso(o.get("timestamp"))} if u else None
    r = self_ctx._scan_tail(path, want, self_ctx.USAGE_TAIL_START)
    return (r["ctx"], r["ts"]) if r else (None, None)


def turn_at_or_before(path, epoch):
    """ctx of the newest assistant turn at or before `epoch` (scans the whole file backward)."""
    def want(o):
        if o.get("type") != "assistant":
            return None
        u = (o.get("message") or {}).get("usage")
        ts = parse_iso(o.get("timestamp"))
        return {"ctx": _ctx_of(u)} if u and ts is not None and ts <= epoch else None
    r = self_ctx._scan_tail(path, want, self_ctx.USAGE_TAIL_START)
    return r["ctx"] if r else None


def parse_ctx_use(text):
    m = CTX_USE_RE.search(text or "")
    if not m:
        return None
    return {"active": int(m.group(1)), "stale": int(m.group(2)), "keep": (m.group(3) or "").strip()[:160]}


def transcript_ctx_use(path):
    """Last CTX-USE line in an assistant message of the transcript: {active, stale, keep, at}."""
    def want(o):
        if o.get("type") != "assistant":
            return None
        content = (o.get("message") or {}).get("content")
        text = content if isinstance(content, str) else " ".join(
            c.get("text", "") for c in content or [] if isinstance(c, dict))
        if "CTX-USE:" not in text:
            return None
        r = parse_ctx_use(text)
        if r:
            r["at"] = iso(parse_iso(o.get("timestamp")) or 0)
        return r
    return self_ctx._scan_tail(path, want, self_ctx.USAGE_TAIL_START)


def proxy_stale(path, boundary_epoch, ctx, base):
    """Calibration proxy only (never a trigger): share of the current context that was already
    present at the last boundary turn, an upper bound of the inactive-segment share."""
    if not boundary_epoch or not ctx or ctx <= base:
        return None
    before = turn_at_or_before(path, boundary_epoch + 60)
    if before is None:
        return None
    return max(0, min(99, int(100 * max(0, before - base) / (ctx - base))))


# --- evaluation --------------------------------------------------------------

def cleared_since(cycle_jsonl, session, epoch):
    """True when ctx-cycle.jsonl holds a successful live clear of `session` at or after `epoch`."""
    try:
        with open(cycle_jsonl) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if (r.get("session") == session and r.get("exit") == 0 and r.get("live")
                        and (parse_iso(r.get("ts")) or 0) >= epoch):
                    return True
    except OSError:
        pass
    return False


def cand(session, signal, detail, action, fp, ctx):
    return {"session": session, "signal": signal, "action": action, "ctx": ctx,
            "fingerprint": fp, "detail": detail}


def evaluate_session(entry, sched, now, wstate, pane_idle=None, p=None):
    """Candidates for one session plus its proxy row. Mutates `wstate` (debounce)."""
    p = p or paths()
    name = entry["name"]
    th = thresholds_for(sched, entry)
    signals = set(entry.get("signals") or ("S1", "S2", "S3", "S4", "S5"))
    reg = registry_entry(name, p["sessions_dir"])
    if not reg:
        return [], None
    tpath = transcript_for(reg.get("sessionId", ""), p["transcripts"])
    if not tpath:
        return [], None
    ctx, last_ts = last_turn(tpath)
    if ctx is None:
        return [], None
    il, il_ok = read_il(entry)
    agents = bool(il.get("agents"))
    bnd = parse_iso(il.get("boundary_at"))
    cu = il.get("ctx_use") if isinstance(il.get("ctx_use"), dict) else None
    if not cu:
        cu = transcript_ctx_use(tpath)
    cu_at = parse_iso((cu or {}).get("at"))
    # a self-report older than the boundary is ignored (spec section 7)
    cu_valid = bool(cu) and cu_at is not None and bnd is not None and cu_at >= bnd
    reported_stale = cu.get("stale") if cu_valid else None
    at_boundary = bnd is not None and last_ts is not None and last_ts <= bnd + th["boundary_slack_s"]
    idle = None if last_ts is None else max(0, now - last_ts)
    if pane_idle is not None and name in pane_idle and not pane_idle[name]:
        idle = 0  # the pane classifier says the session is not idle
    out = []
    consumed = bnd is not None and cleared_since(p["cycle_jsonl"], name, bnd)
    bkey = il.get("boundary_at") or "none"
    if "S1" in signals and cu_valid and at_boundary and not consumed and ctx >= th["floor_ctx"] \
            and reported_stale is not None and reported_stale >= th["stale_pct"]:
        out.append(cand(name, "S1", f"boundary {bkey}, ctx {ctx // 1000}k, self-reported stale "
                        f"{reported_stale}%", "clear", f"S1|{name}|{bkey}", ctx))
    if "S2" in signals and ctx >= th["approach_ctx"] and not consumed:
        last = wstate.setdefault("s2_last", {}).get(name, 0)
        if now - last >= th["s2_refire_s"]:
            if ctx >= th["bar_ctx"] and at_boundary:
                out.append(cand(name, "S2", f"context bar: ctx {ctx // 1000}k at boundary {bkey}",
                                "clear", f"S2|{name}|{int(now // th['s2_refire_s'])}", ctx))
            else:
                out.append(cand(name, "S2", f"approaching: ctx {ctx // 1000}k, reach a task boundary",
                                "ask-boundary", f"S2a|{name}|{int(now // th['s2_refire_s'])}", ctx))
            wstate["s2_last"][name] = now
    if "S3" in signals and at_boundary and not agents and idle is not None \
            and ctx >= th["floor_ctx"] and th["s3_min_idle_s"] <= idle <= th["s3_max_idle_s"]:
        out.append(cand(name, "S3", f"idle {int(idle // 60)} min before cache expiry, ctx {ctx // 1000}k",
                        "clear", f"S3|{name}|{int(last_ts)}", ctx))
    sid = reg.get("sessionId", "")
    if "S4" in signals and sid not in wstate.setdefault("s4", {}):
        wstate["s4"][sid] = now
        if idle is not None and idle > th["s4_idle_s"] and ctx >= th["floor_ctx"] \
                and not il.get("task"):
            out.append(cand(name, "S4", f"restored idle history: idle {int(idle // 60)} min, "
                            f"ctx {ctx // 1000}k, no open task", "clear", f"S4|{name}|{sid}", ctx))
    if "S5" in signals and ctx >= th["guard_ctx"] and not consumed:
        out.append(cand(name, "S5", f"compaction guard: ctx {ctx // 1000}k",
                        "report" if agents else "clear", f"S5|{name}|{int(now // 3600)}", ctx))
    proxy = {"ts": iso(now), "session": name, "ctx": ctx, "boundary_at": il.get("boundary_at"),
             "reported_stale": reported_stale, "proxy_stale": proxy_stale(
                 tpath, bnd, ctx, th["fixed_base"]), "method": "pre-boundary-share",
             "il_readable": il_ok, "fired": [c["signal"] for c in out]}
    return out, proxy


def evaluate(sched, now, wstate, pane_idle=None, only=None, p=None):
    """(candidates, proxy rows) over every session of the schedule."""
    cands, rows = [], []
    for entry in sched.get("sessions") or []:
        if only and entry.get("name") != only:
            continue
        try:
            c, row = evaluate_session(entry, sched, now, wstate, pane_idle, p)
        except (OSError, KeyError, ValueError) as e:  # one bad session must not stop the rest
            print(f"ctx-watch: {entry.get('name')}: {type(e).__name__}: {e}", file=sys.stderr)
            continue
        cands += c
        if row:
            rows.append(row)
    return cands, rows


def save_json_atomic(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f)
    os.replace(tmp, path)


def step(now=None, pane_idle=None, only=None, write=True):
    """Poller entry: candidates for this cycle; persists debounce state and proxy rows."""
    now = time.time() if now is None else now
    p = paths()
    sched = load_schedule(p["schedule"])
    wstate = load_json(p["state"], {})
    cands, rows = evaluate(sched, now, wstate, pane_idle, only, p)
    if write:
        save_json_atomic(p["state"], wstate)
        if rows:
            os.makedirs(os.path.dirname(p["proxy"]), exist_ok=True)
            with open(p["proxy"], "a") as f:
                for r in rows:
                    f.write(json.dumps(r) + "\n")
    return cands


def main(argv=None):
    ap = argparse.ArgumentParser(description="Evaluate clear signals S1-S5 (SPEC-IL-01 section 7).")
    ap.add_argument("--json", action="store_true", help="one JSON list instead of one line per candidate")
    ap.add_argument("--now", help="ISO time to evaluate at (default: now)")
    ap.add_argument("--no-state", action="store_true", help="write no debounce state or proxy rows")
    ap.add_argument("--session", help="only this session")
    a = ap.parse_args(argv)
    now = parse_iso(a.now) if a.now else time.time()
    if now is None:
        print("ctx-watch: bad --now", file=sys.stderr)
        return 2
    cands = step(now=now, only=a.session, write=not a.no_state)
    if a.json:
        print(json.dumps(cands, indent=1))
    else:
        for c in cands:
            print(json.dumps(c))
    return 0


if __name__ == "__main__":
    sys.exit(main())

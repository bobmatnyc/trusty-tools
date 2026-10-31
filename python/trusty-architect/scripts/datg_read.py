"""datg_read.py: the read-only datg side: summary, dispatch check, freshness, stamp, digest.

Why: datg.py passed the 500-SLOC cap; #10161 P3 split it by section, bodies moved verbatim.
What: summary, dispatch_mode/check_dispatch, log_hook_warning, op_fresh, turn_tools,
stamp_turn (the Stop hook's stale_il record) and build_digest. datg.py re-exports every name.
Test: tests/test_datg.py
"""
import json
import os
import re
import sys
import time
from datetime import datetime, timezone

from datg_log import append_log
from datg_store import (DEFAULT_BLOCK_AFTER, SIZE_WARN, DatgError, Locked, _atomic_write, _jl, _size, load_state,
                        now_iso, parse_iso, parse_ts, paths, per_pm_session, read_log)

# ---- read-only side -------------------------------------------------------------

def summary(st):
    goals = sorted((g for g in st["goals"] if g["status"] == "active"), key=lambda g: g["rank"])
    openi = [i["id"] for i in st["items"] if i["status"] == "open"]
    ans = [i["id"] for i in st["items"] if i["status"] == "answered"]
    parts = []
    if goals:
        parts.append(f"TOP {goals[0]['id']} {goals[0]['title'][:60]}")
    if openi:
        parts.append("OPEN " + ",".join(openi[:6]))
    if ans:
        parts.append("ANSWERED " + ",".join(ans[:6]))
    return "; ".join(parts)


def dispatch_mode(st, now=None):
    cfg = st.get("config") or {}
    if cfg.get("mode") in ("warn", "block"):
        return cfg["mode"]
    try:
        after = parse_iso(cfg.get("block_after") or DEFAULT_BLOCK_AFTER)
    except ValueError:
        after = parse_iso(DEFAULT_BLOCK_AFTER)
    return "block" if (now or datetime.now(timezone.utc)) >= after else "warn"


def check_dispatch(st, prompt, now=None):
    """Return (decision, message); decision is allow|warn|block. Pure."""
    active = {g["id"] for g in st["goals"] if g["status"] == "active"}
    if not active:
        return "allow", ""
    code = st["code"]
    m = re.search(rf"Goal:\s*({re.escape(code)}-G[0-9a-f]{{2}})\b", prompt or "")
    if m and m.group(1) in active:
        return "allow", ""
    top = sorted((g for g in st["goals"] if g["status"] == "active"), key=lambda g: g["rank"])[0]
    why = f"names non-active goal {m.group(1)}" if m else "names no goal id"
    msg = f"DATG: dispatch {why}. Start the prompt with 'Goal: {top['id']}' (top) or another active goal."
    return dispatch_mode(st, now), msg


def log_hook_warning(project, msg):
    """Append a warn-mode warning to log.jsonl; the hook stays fail open and writes no state."""
    try:
        append_log(project, {"ts": now_iso(), "session": os.environ.get("DATG_SESSION", ""),
                             "kind": "hook_warning", "text": msg})
    except OSError:
        pass


# ---- freshness, stamp, digest ---------------------------------------------------

MUTATING_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit", "Agent", "Task", "SendMessage",
                  "CronCreate", "CronDelete"}
MUTATING_BASH = re.compile(
    r"\bgit\s+(commit|push|merge|rebase|reset|tag|checkout|switch|cherry-pick|apply|am)\b"
    r"|\bgh\s+(pr|issue|release|run)\s+(create|merge|close|reopen|edit|comment|review|rerun|cancel)\b"
    r"|\bcargo\s+publish\b|\btmux\s+send-keys\b|(^|[;&|]\s*)(rm|mv)\s")


def _stamp_path(project):
    return os.path.join(paths(project)[0], "stamp.json")


def _read_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            r = json.load(f)
        return r if isinstance(r, dict) else {}
    except (OSError, ValueError):
        return {}


def _to_epoch(v):
    """ISO minute/second string or integer epoch -> float epoch seconds; DatgError otherwise."""
    if isinstance(v, (int, float)) or str(v).isdigit():
        return float(v)
    t = parse_ts(v)
    if not t:
        raise DatgError(f"bad time {v!r}; use ISO YYYY-MM-DDTHH:MMZ or epoch seconds")
    return t.timestamp()


def op_fresh(project, since=None, max_age=None, now=None):
    """Return (exit_code, message): 0 fresh, 1 stale, 2 unreadable."""
    now = now if now is not None else time.time()
    try:
        ses = load_state(project).get("session") or {}
        stamp = _read_json(_stamp_path(project))
        floor = _to_epoch(since) if since is not None else stamp.get("changed_epoch")
        if since is None and isinstance(stamp.get("v"), int) and load_state(project)["v"] > stamp["v"]:
            floor = None  # a write landed after the Stop that recorded the change
    except DatgError as e:
        return 2, f"unreadable: {e}"
    w_at = parse_ts(ses.get("written_at"))
    if not w_at:
        return 1, "stale: no session write recorded"
    w = ses.get("written_epoch") if isinstance(ses.get("written_epoch"), int) else w_at.timestamp()
    exact = isinstance(ses.get("written_epoch"), int)
    if not exact and floor is not None:
        floor = floor - floor % 60  # written_at has minute precision
    info = f"written_at={ses['written_at']}"
    if floor is not None and (w <= floor if exact else w < floor):
        return 1, f"stale: state changed after the last write; {info}"
    if max_age is not None and now - w > max_age * 60:
        return 1, f"stale: write older than {max_age} min; {info}"
    return 0, f"fresh: {info}"


def turn_tools(path, tail_bytes=2_000_000):
    """tool_use blocks of the last assistant turn of a transcript jsonl; None when unreadable."""
    try:
        with open(path, "rb") as f:
            if os.fstat(f.fileno()).st_size > tail_bytes:
                f.seek(-tail_bytes, 2)
                f.readline()
            lines = f.read().decode("utf-8", "replace").splitlines()
    except OSError:
        return None
    tools = []
    for ln in reversed(lines):
        try:
            rec = json.loads(ln)
        except ValueError:
            continue
        content = (rec.get("message") or {}).get("content") if isinstance(rec, dict) else None
        if rec.get("type") == "user":
            if isinstance(content, str) or (isinstance(content, list) and any(
                    isinstance(b, dict) and b.get("type") != "tool_result" for b in content)):
                break
        elif rec.get("type") == "assistant" and isinstance(content, list):
            tools += [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
    return tools


def stamp_turn(project, payload, now=None):
    """Stop hook: log `stale_il` when the finished turn changed state and no datg.py write followed."""
    tools = turn_tools(payload["transcript_path"]) if payload.get("transcript_path") else None
    cmds = [str((t.get("input") or {}).get("command", "")) for t in tools or [] if t.get("name") == "Bash"]
    ours = any("datg.py" in c for c in cmds)
    changed = any(t.get("name") in MUTATING_TOOLS for t in tools or []) or any(
        MUTATING_BASH.search(c) for c in cmds if "datg.py" not in c)
    with Locked(project):
        st = load_state(project)
        sp = _stamp_path(project)
        prev = _read_json(sp)
        wrote = ours or (isinstance(prev.get("v"), int) and st["v"] > prev["v"])
        new = {"v": st["v"], "stale_turns": int(prev.get("stale_turns") or 0)}
        if prev.get("changed_epoch"):
            new["changed_epoch"] = prev["changed_epoch"]
        if wrote:
            new["stale_turns"] = 0
            new.pop("changed_epoch", None)
        elif changed:
            new["stale_turns"] += 1
            new["changed_epoch"] = int(now if now is not None else time.time())
            try:
                append_log(project, {"ts": now_iso(), "session": os.environ.get("DATG_SESSION", ""),
                                     "kind": "stale_il", "n": new["stale_turns"],
                                     "text": f"turn changed state without an IL write ({new['stale_turns']} in a row)"})
            except OSError as e:
                print(f"datg: log write failed ({e})", file=sys.stderr)
        _atomic_write(sp, _jl(new))
    return new


def _short(v, n):
    v = " ".join(str(v or "").split())
    return v if len(v) <= n else v[:n - 1] + "…"


def _digest_sections(project, st):
    ses = st.get("session") or {}
    name = per_pm_session(project) or os.environ.get("DATG_SESSION") or st["code"]
    code, msg = op_fresh(project)
    fresh = {0: "fresh", 1: "STALE"}.get(code, "unknown")
    ctx = (ses.get("ctx_use") or {}).get("ctx")
    head = [f"IL DIGEST {name} ({st['code']}) v{st['v']} written_at={ses.get('written_at', 'never')} IL={fresh}"
            + (f" ctx_before_clear={ctx // 1000}k" if isinstance(ctx, int) else "")]
    size = _size(paths(project)[1])
    if size > SIZE_WARN:
        head.append(f"SIZE: state.json {size} B over the {SIZE_WARN} B bar; run datg.py prune")
    sess = ["SESSION"]
    if not ses.get("task") or not ses.get("next"):   # #10579: a HOLD is an extra line, never a substitute
        sess.append("  none recorded")
    for k in ("hold", "merge_hold", "task", "next"):   # #10579: the holds are read first
        if ses.get(k):
            sess.append(f"  {k}: {ses[k]}")
    for w in ses.get("waiting_on") or []:
        sess.append(f"  waiting_on {w.get('who')}: {w.get('what')} (since {w.get('since')})")
    if "owed" in ses:
        for o in ses["owed"]:
            sess.append(f"  owed: {o['check']} = {o['expect']} @ {o.get('ref', '')}")
        if not ses["owed"]:
            sess.append("  owed: none (stated)")
    else:
        sess.append("  owed: NOT RECORDED")
    if "agents" in ses:
        sess.append("  agents: " + ("; ".join(f"{a['id']} [{a.get('goal', '')}] {a.get('what', '')}"
                                              for a in ses["agents"]) or "none"))
    if "crons" in ses:
        sess += [f"  cron {c['id']} [{c['schedule']}] {c['prompt']}" for c in ses["crons"]] or []
        if not ses["crons"]:
            sess.append("  crons: none")
    if ses.get("boundary_at"):
        sess.append(f"  boundary_at: {ses['boundary_at']}")
    cu = ses.get("ctx_use")
    if cu:
        sess.append(f"  ctx_use: active {cu['active']}% stale {cu['stale']}% ctx {cu['ctx']} at {cu['at']} keep: {cu['keep']}")
    goals = ["GOALS"] + [f"  {g['id']} r{g['rank']} {g['title']}" + (f" -> {g['finish']}" if g.get("finish") else "")
                         for g in sorted((g for g in st["goals"] if g["status"] == "active"), key=lambda g: g["rank"])]
    items = ["ITEMS (open/answered)"]
    for i in st["items"]:
        if i["status"] in ("open", "answered"):
            rec = next((o["l"] for o in i.get("opts") or [] if o.get("rec")), "")
            items.append(f"  {i['id']} [{i['status']}]{' esc' if i.get('esc') else ''} {i['title']}"
                         + (f" | answer: {i['answer']}" if i.get("answer") else "")
                         + (f" | rec: {rec}" if rec and i["status"] == "open" else ""))
    tasks = [f"  {t['id']} [{t['status']}] {t['goal']} {t['title']}" for t in st["tasks"] if t["status"] != "done"]
    recent = [x for x in read_log(project, 262144) if x[1] in ("escalated", "answered", "done", "drift")][-10:]
    log = ["RECENT"] + [f"  {r.get('t') or r.get('ts')} {r.get('id', '')} {k} {_short(r.get('note') or r.get('text'), 80)}"
                        for _, k, r in recent]
    briefs = sorted(set(re.findall(r"[\w./~-]*briefs?/[\w./-]+\.md", json.dumps(st, ensure_ascii=False))))
    refs = [f"  ref {r['key']}: {r['value']}" for r in ses.get("refs") or []]
    ptr = ["POINTERS"] + refs + [f"  archive: {os.path.join(paths(project)[0], 'archive.jsonl')}"] + [f"  brief: {b}" for b in briefs[:8]]
    return head, sess, goals, items, tasks, log, ptr, (["POINTERS"] + refs if refs else [])


def build_digest(project, max_tokens=3000):
    """Bounded resume digest (tokens = chars / 4). Cut order: RECENT, oldest tasks, briefs+archive pointers, items, ref pointers, tail."""
    st = load_state(project)
    head, sess, goals, items, tasks, log, ptr, ptr_refs = _digest_sections(project, st)
    limit = max(200, max_tokens * 4) - 60  # room for the TRUNCATED line

    def render(tk, with_log, with_ptr, it):
        parts = [head, sess, goals, it, (["TASKS (open/running)"] + tk) if tk else [], log if with_log else [],
                 ptr if with_ptr is True else ptr_refs if with_ptr == "refs" else []]
        return "\n".join(x for p in parts for x in p)

    full = render(tasks, True, True, items)
    if len(full) <= limit + 60:
        return full
    out, tk, it = None, list(tasks), list(items)
    for step in ("log", "tasks", "ptr", "items", "cut"):
        if step == "log":
            out = render(tk, False, True, it)
        elif step == "tasks":
            while tk and len(render(tk, False, True, it)) > limit:
                tk.pop(0)  # oldest first
            out = render(tk, False, True, it)
        elif step == "ptr":
            out = render(tk, False, "refs", it)   # briefs and archive go first; refs stay until items are cut
        elif step == "items":
            while len(it) > 1 and len(render(tk, False, "refs", it)) > limit:
                it.pop()
            out = render(tk, False, "refs", it)
            if len(out) > limit:
                out = render(tk, False, False, it)
        else:
            out = out[:limit]
        if len(out) <= limit:
            break
    return out + f"\nTRUNCATED: {len(full) - len(out)} bytes dropped"

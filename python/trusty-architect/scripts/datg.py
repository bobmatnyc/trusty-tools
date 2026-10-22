#!/usr/bin/env python3
"""datg.py: per-PM goals/decisions/asks/tasks store and hook logic (stdlib only).

Why: pilot of the PM D/A/T/G design (local/briefs/2026-10-09-pm-datg-prototype-design.md,
the owner D93, 2026-10-09 13:28Z): keep a PM on ranked goals. Only the PM writes, one call per turn.
What: <project>/.trusty-mpm/datg/state.json (snapshot, replaced atomically: temp file +
os.replace, under flock on .lock) and log.jsonl (append-only, one line per change).
Verbs: init, add, update, close, batch, session set, prune, rotate, stamp (write); show,
check-dispatch, digest, fresh, archive-show, hook (read-only apart from log lines).
A write refuses (exit 2, files untouched) when state.json is malformed or a field is invalid.
`hook` serves SessionStart (rules + digest), Stop (stamp) and PreToolUse(Agent|Task); every
error path prints nothing, exit 0.
Intent Ledger (docs/specs/SPEC-IL-01.md), interface choices where the spec is open:
- Layout: legacy <project>/.trusty-mpm/datg/ keeps working. `--session NAME` (init forces it)
  selects <project>/.trusty-mpm/datg/NAME/; $DATG_SESSION (set by datg-hook.sh) or the tmux
  session of $TMUX_PANE selects it only when that directory already holds a state.json.
- session record: `session set [--task T] [--next N] [--wait who:what ..] [--owe check=expect@ref ..]
  [--agent id:goal:what ..] [--cron id=schedule=prompt ..] [--boundary] [--ctx-use A:S:CTX:keep] [--ref KEY=VALUE ..]`.
  `--ref` pointers (the Google Doc, its source md, the relay marker) live in session.refs [{key, value}]: KEY
  ^[a-z0-9][a-z0-9_.-]{0,31}$, VALUE one line <= 200 chars, <= 12 refs; an existing KEY is replaced, `none` clears.
  The digest prints them first under POINTERS and cuts them after items. Omitted fields are kept; a repeatable flag replaces its list; value `none` states an empty list
  (`--owe none` sets owed_none). Batch op: {"op":"session","task":..,"waiting_on":[{who,what}],
  "owed":[{check,expect,ref}],"agents":[..],"crons":[..],"boundary":true,"ctx_use":{..}}.
  Every commit stamps session.written_at (ISO minute) and written_epoch (int seconds).
- close / `update --status done` / prune move D, A, T items to archive.jsonl in the same commit
  (archive first, line {archived_at, closed_at, kind, item, cv}; cv = state v of the commit, a
  line with cv > state v is an orphan of a failed commit and is dropped under the next lock).
  `prune [--dry-run] [--answered-hours 24] [--require-outcome]` archives every done item/task and
  every answered item older than 24 h (spec says "with outcome"; the pilot's 36 answered items
  have none, so outcome is required only with --require-outcome). `archive-show ID` prints one line.
- Size bar: state.json > 8 KB logs `size_warning` on crossing; a write that leaves it > 16 KB and
  larger than before is refused (exit 2, largest items named) after one automatic prune attempt.
- `rotate`: log lines older than 7 days go to log-<YYYYMMDD>.jsonl (rotation day). Both log shapes
  (t/ev and ts/kind) are read.
- `digest [--max-tokens 3000] [--no-log]`: bounded resume digest on stdout (tokens = chars/4);
  appends a `digest` log line unless --no-log.
- `fresh [--since ISO|EPOCH] [--max-age MIN]`: stdout one line; exit 0 fresh, 1 stale, 2 unreadable.
  Fresh = written_at not before --since (default: the last unanswered change recorded by `stamp`,
  none = fresh) and, with --max-age, not older than MIN minutes.
- `stamp` (Stop hook; hook JSON on stdin with transcript_path): if the finished turn used a
  state-changing tool (Edit/Write/Agent/Task/SendMessage/Cron*, or Bash git/gh mutations) and no
  datg.py write followed, append {ts, session, kind: stale_il, n} to log.jsonl (n = consecutive
  stale turns) and record stamp.json changed_epoch for `fresh`.
Escalation: `update --esc architect` (or a batch op) also queues a `decision` through the D10
intake (scripts/pm-notify.sh, override DATG_NOTIFY_CMD); a failure is logged, never fatal.
A warn-mode hook warning is appended to log.jsonl as kind hook_warning (ts, session, text).
Dispatch policy: warn until config.block_after (default 2026-10-12T13:30Z), then deny a
dispatch whose prompt lacks "Goal: <code>-G<hex2>" naming an active goal; config.mode
("warn"|"block") overrides the date. No network, no secrets (locations only in items).
Test: scripts/test_datg.py
"""
import argparse
import fcntl
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

import datg_store  # noqa: E402  (#10161: main() sets datg_store._SESSION)
from datg_store import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    CODE_RE, DEFAULT_BLOCK_AFTER, DatgError, EVENTS, ID_RE, Locked, PREFIXES, RULES, SESSION_MAX, SESSION_RE,
    SIZE_REFUSE, SIZE_WARN, STATUS, TITLE_MAX, VAGUE, WHO, _atomic_write, _dump, _jl, _largest, _size,
    _tmux_session, answered_times, commit, datg_dir, load_state, log_rec, now_iso, parse_iso, parse_ts, paths,
    per_pm_session, prune_state, read_log, reconcile_archive,
)
from datg_ops import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    REFS_MAX, REF_KEY, _all_ids, _bad, _check_goal, _check_opts, _find, _int, _lim, _merge_refs, _next_id,
    _opts, _rows, _title, apply_add, apply_op, apply_session, apply_update, collect_closed, op_init,
    write_ops_ev,
)
from datg_log import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    append_log, archive_lookup, notify_escalations, op_prune, op_rotate, pm_session, write_ops,
)


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
    if not ses.get("task") and not ses.get("next"):
        sess.append("  none recorded")
    for k in ("task", "next"):
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


def hook_output(event, st, payload, project, now=None):
    """Return the hook JSON dict for the event, or None for no output."""
    if event == "SessionStart":
        py = os.path.abspath(__file__)
        pm = per_pm_session(project)
        text = RULES.format(py=py + (f" --session {pm}" if pm else ""), code=st["code"])
        s = summary(st)
        try:
            dig = "\n" + build_digest(project)
        except Exception:
            dig = ""  # fail open: rules and summary still go out
        return {"hookSpecificOutput": {"hookEventName": "SessionStart",
                                       "additionalContext": text + (" " + s if s else "") + dig}}
    if event == "Stop":
        stamp_turn(project, payload)
        return None
    if event == "PreToolUse" and payload.get("tool_name") in ("Agent", "Task"):
        ti = payload.get("tool_input") or {}
        dec, msg = check_dispatch(st, ti.get("prompt") or "", now)
        if dec == "block":
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                           "permissionDecision": "deny",
                                           "permissionDecisionReason": msg}}
        if dec == "warn":
            log_hook_warning(project, msg)
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                           "additionalContext": msg + " (warn mode)"}}
    return None


def run_hook(stdin_text):
    """Fail open: any problem returns "" (no output)."""
    try:
        payload = json.loads(stdin_text or "{}")
        if not isinstance(payload, dict) or payload.get("agent_id"):
            return ""
        project = os.environ.get("CLAUDE_PROJECT_DIR") or payload.get("cwd") or os.getcwd()
        st = load_state(project)
        out = hook_output(payload.get("hook_event_name"), st, payload, project)
        return json.dumps(out, separators=(",", ":")) if out else ""
    except Exception:
        return ""


def cmd_show(project, brief):
    st = load_state(project)
    if brief:
        return summary(st) or "no open items"
    return json.dumps(st, indent=1, ensure_ascii=False)


def build_parser():
    p = argparse.ArgumentParser(prog="datg.py", description=__doc__.split("\n")[0])
    p.add_argument("--project", default=os.getcwd(), help="project root (default: cwd)")
    p.add_argument("--session", help="per-PM directory datg/<session>/ (shared project dirs)")
    sub = p.add_subparsers(dest="verb", required=True)
    s = sub.add_parser("init")
    s.add_argument("--code", required=True)
    s = sub.add_parser("add")
    s.add_argument("kind", choices=PREFIXES)
    s.add_argument("--title", required=True)
    s.add_argument("--goal")
    s.add_argument("--rank", type=int)
    s.add_argument("--finish")
    s.add_argument("--maps")
    s.add_argument("--opts", help="comma list, trailing * marks the recommendation")
    s.add_argument("--to")
    s.add_argument("--id")
    s = sub.add_parser("update")
    s.add_argument("id")
    for f in ("title", "status", "answer", "outcome", "esc", "to", "note"):
        s.add_argument(f"--{f}")
    s.add_argument("--rank", type=int)
    s = sub.add_parser("close")
    s.add_argument("id")
    s.add_argument("--outcome", required=True)
    s = sub.add_parser("batch", help="JSON list of ops on stdin: one write per turn")
    s = sub.add_parser("show")
    s.add_argument("--brief", action="store_true")
    s = sub.add_parser("check-dispatch")
    s.add_argument("--prompt", help="default: stdin")
    sub.add_parser("hook", help="Claude Code hook entry; JSON on stdin")
    s = sub.add_parser("session", help="set fields of the session record")
    s.add_argument("action", choices=("set",))
    s.add_argument("--task")
    s.add_argument("--next")
    s.add_argument("--wait", action="append", metavar="WHO:WHAT", help="repeatable; 'none' = empty list")
    s.add_argument("--owe", action="append", metavar="CHECK=EXPECT@REF", help="repeatable; 'none' = nothing owed")
    s.add_argument("--agent", action="append", metavar="ID:GOAL:WHAT", help="repeatable; 'none'")
    s.add_argument("--cron", action="append", metavar="ID=SCHEDULE=PROMPT", help="repeatable; 'none'")
    s.add_argument("--boundary", action="store_true")
    s.add_argument("--ctx-use", metavar="ACTIVE:STALE:CTX:KEEP")
    s.add_argument("--ref", action="append", metavar="KEY=VALUE", help="repeatable; same KEY replaces; 'none' clears")
    s = sub.add_parser("prune", help="archive done items/tasks and answered items older than 24 h")
    s.add_argument("--dry-run", action="store_true")
    s.add_argument("--answered-hours", type=float, default=24)
    s.add_argument("--require-outcome", action="store_true")
    sub.add_parser("rotate", help="move log lines older than 7 days to log-<YYYYMMDD>.jsonl")
    s = sub.add_parser("archive-show")
    s.add_argument("id")
    s = sub.add_parser("digest", help="bounded resume digest")
    s.add_argument("--max-tokens", type=int, default=3000)
    s.add_argument("--no-log", action="store_true")
    s = sub.add_parser("fresh", help="exit 0 fresh, 1 stale, 2 unreadable")
    s.add_argument("--since", help="ISO YYYY-MM-DDTHH:MMZ or epoch seconds")
    s.add_argument("--max-age", type=float, help="minutes")
    sub.add_parser("stamp", help="Stop hook step; hook JSON on stdin")
    return p


def session_op(a):
    """Build the batch-style `session` op from the CLI flags (strings parsed here, validated in apply_session)."""
    op = {"op": "session", "task": a.task, "next": a.next, "boundary": a.boundary}

    def lst(vals, parse):
        return [] if vals == ["none"] else [parse(v) for v in vals]

    def need(v, sep, n, fmt):
        parts = v.split(sep, n - 1)
        if len(parts) != n:
            raise DatgError(f"expected {fmt}, got {v!r}")
        return parts

    if a.wait is not None:
        op["waiting_on"] = lst(a.wait, lambda v: dict(zip(("who", "what"), need(v, ":", 2, "WHO:WHAT"))))
    if a.owe is not None:
        def owe(v):
            check, expect_ref = need(v, "=", 2, "CHECK=EXPECT@REF")
            expect, _, ref = expect_ref.partition("@")
            return {"check": check, "expect": expect, "ref": ref}
        op["owed"] = lst(a.owe, owe)
    if a.agent is not None:
        op["agents"] = lst(a.agent, lambda v: dict(zip(("id", "goal", "what"), need(v, ":", 3, "ID:GOAL:WHAT"))))
    if a.cron is not None:
        op["crons"] = lst(a.cron, lambda v: dict(zip(("id", "schedule", "prompt"), need(v, "=", 3, "ID=SCHEDULE=PROMPT"))))
    if a.ref is not None:
        op["refs"] = lst(a.ref, lambda v: dict(zip(("key", "value"), need(v, "=", 2, "KEY=VALUE"))))
    if a.ctx_use:
        act, stale, ctx, keep = need(a.ctx_use, ":", 4, "ACTIVE:STALE:CTX:KEEP")
        try:
            op["ctx_use"] = {"active": int(act), "stale": int(stale), "ctx": int(ctx), "keep": keep}
        except ValueError:
            raise DatgError("ctx-use: ACTIVE, STALE, CTX must be integers")
    return op


def main(argv=None):
    a = build_parser().parse_args(argv)
    datg_store._SESSION = a.session  # #10161: _SESSION lives in datg_store, which reads it
    try:
        if a.verb == "hook":
            out = run_hook(sys.stdin.read())
            if out:
                print(out)
            return 0
        if a.verb == "fresh":
            code, msg = op_fresh(a.project, a.since, a.max_age)
            print(msg)
            return code
        if a.verb == "init":
            print(op_init(a.project, a.code))
        elif a.verb == "add":
            op = {"op": "add", "kind": a.kind, "title": a.title, "goal": a.goal, "rank": a.rank,
                  "finish": a.finish, "maps": a.maps, "opts": _opts(a.opts), "to": a.to, "id": a.id}
            print(write_ops(a.project, [op]))
        elif a.verb == "update":
            op = {k: getattr(a, k) for k in ("title", "status", "answer", "outcome", "esc", "to", "note", "rank")}
            op.update({"op": "update", "id": a.id})
            evs, st = write_ops_ev(a.project, [op])
            print(", ".join(e["id"] for e in evs))
            notify_escalations(a.project, evs, st)
        elif a.verb == "close":
            print(write_ops(a.project, [{"op": "close", "id": a.id, "outcome": a.outcome}]))
        elif a.verb == "batch":
            try:
                ops = json.loads(sys.stdin.read())
            except ValueError as e:
                raise DatgError(f"batch input is not JSON ({e})")
            if not isinstance(ops, list):
                raise DatgError("batch input must be a JSON list")
            evs, st = write_ops_ev(a.project, ops)
            print(", ".join(e["id"] for e in evs))
            notify_escalations(a.project, evs, st)
        elif a.verb == "session":
            print(write_ops(a.project, [session_op(a)]))
        elif a.verb == "prune":
            print(op_prune(a.project, a.dry_run, a.answered_hours, a.require_outcome))
        elif a.verb == "rotate":
            print(op_rotate(a.project))
        elif a.verb == "archive-show":
            print(archive_lookup(a.project, a.id))
        elif a.verb == "digest":
            print(build_digest(a.project, a.max_tokens))
            if not a.no_log:
                try:
                    append_log(a.project, {"ts": now_iso(), "session": os.environ.get("DATG_SESSION", ""), "kind": "digest"})
                except OSError as e:
                    print(f"datg: log write failed ({e})", file=sys.stderr)
        elif a.verb == "stamp":
            try:
                payload = json.loads(sys.stdin.read() or "{}")
            except ValueError:
                payload = {}
            stamp_turn(a.project, payload if isinstance(payload, dict) else {})
        elif a.verb == "show":
            print(cmd_show(a.project, a.brief))
        elif a.verb == "check-dispatch":
            prompt = a.prompt if a.prompt is not None else sys.stdin.read()
            dec, msg = check_dispatch(load_state(a.project), prompt)
            print(dec + (": " + msg if msg else ""))
            return 2 if dec == "block" else 0
    except DatgError as e:
        print(f"datg: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())

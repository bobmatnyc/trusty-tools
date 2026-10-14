#!/usr/bin/env python3
"""datg.py: per-PM goals/decisions/asks/tasks store and hook logic (stdlib only).

Why: pilot of the PM D/A/T/G design (local/briefs/2026-10-09-pm-datg-prototype-design.md,
the owner D93, 2026-10-09 13:28Z): keep a PM on ranked goals. Only the PM writes, one call per turn.
What: <project>/.trusty-mpm/datg/state.json (snapshot, replaced atomically: temp file +
os.replace, under flock on .lock) and log.jsonl (append-only, one line per change).
Verbs: init, add, update, close, batch (write); show, check-dispatch, hook (read-only).
A write refuses (exit 2, files untouched) when state.json is malformed or a field is invalid.
`hook` serves SessionStart and PreToolUse(Agent|Task); every error path prints nothing, exit 0.
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
import sys
from datetime import datetime, timezone

DEFAULT_BLOCK_AFTER = "2026-10-12T13:30Z"
PREFIXES = ("G", "D", "A", "T")
STATUS = {
    "G": ("active", "done", "parked"),
    "D": ("open", "answered", "done"),
    "A": ("open", "answered", "done"),
    "T": ("open", "running", "done"),
}
EVENTS = ("open", "escalated", "answered", "done", "drift", "reprio", "update")
CODE_RE = re.compile(r"^[a-z][a-z0-9]{1,7}$")
ID_RE = re.compile(r"^([a-z][a-z0-9]{1,7})-([GDAT])([0-9a-f]{2})$")
TITLE_MAX = 200

RULES = (
    "DATG rules (state: .trusty-mpm/datg/state.json). You are the only writer: one "
    "`python3 {py} <verb>` call at the end of EVERY turn. "
    "1) Every Agent dispatch prompt starts 'Goal: {code}-Gxx'; no goal id, no dispatch. "
    "2) Handbacks end with: Goals (rank order), Open D/A (ids), Next. "
    "3) Owner questions are D, owner actions are A (options, recommendation first); cite ids, never repeat. "
    "4) Before dispatch: does it serve the top active goal? If none, record a D 'drift:'. "
    "5) Unsettled D/A: set esc=architect and SendMessage tm-architect. "
    "6) Relayed answer: write it in, act, close with a verified outcome. "
    "Never change goal ranks. No secret values."
)


class DatgError(Exception):
    """Invalid input or unwritable state; the CLI maps it to exit 2."""


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


def parse_iso(s):
    return datetime.strptime(s, "%Y-%m-%dT%H:%MZ").replace(tzinfo=timezone.utc)


def datg_dir(project):
    return os.path.join(project, ".trusty-mpm", "datg")


def paths(project):
    d = datg_dir(project)
    return d, os.path.join(d, "state.json"), os.path.join(d, "log.jsonl")


def load_state(project):
    """Return the parsed state; raise DatgError when missing or malformed."""
    _, sp, _ = paths(project)
    try:
        with open(sp, encoding="utf-8") as f:
            st = json.load(f)
    except FileNotFoundError:
        raise DatgError("state.json missing; run init")
    except (OSError, ValueError) as e:
        raise DatgError(f"state.json malformed ({e}); refusing to write")
    ok = (isinstance(st, dict) and CODE_RE.match(str(st.get("code", "")))
          and isinstance(st.get("v"), int) and isinstance(st.get("next"), dict)
          and all(isinstance(st.get(k), list) for k in ("goals", "items", "tasks")))
    if not ok:
        raise DatgError("state.json malformed (bad shape); refusing to write")
    return st


def _atomic_write(path, text):
    tmp = f"{path}.tmp.{os.getpid()}"
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise


def commit(project, st, events):
    """Bump v, replace state.json atomically, append one log line per event."""
    d, sp, lp = paths(project)
    st["v"] += 1
    _atomic_write(sp, json.dumps(st, separators=(",", ":"), ensure_ascii=False) + "\n")
    with open(lp, "a", encoding="utf-8") as f:
        t = now_iso()
        for ev in events:
            rec = {"t": t, "id": ev["id"], "ev": ev["ev"]}
            if ev.get("note"):
                rec["note"] = ev["note"]
            f.write(json.dumps(rec, separators=(",", ":"), ensure_ascii=False) + "\n")
        f.flush()
        os.fsync(f.fileno())


class Locked:
    """flock on <datg>/.lock for the read-modify-write span."""

    def __init__(self, project):
        self.d = datg_dir(project)

    def __enter__(self):
        os.makedirs(self.d, exist_ok=True)
        self.f = open(os.path.join(self.d, ".lock"), "w")
        fcntl.flock(self.f, fcntl.LOCK_EX)
        return self

    def __exit__(self, *a):
        self.f.close()


def _title(t):
    t = (t or "").strip()
    if not t or len(t) > TITLE_MAX or "\n" in t:
        raise DatgError(f"title required, one line, <= {TITLE_MAX} chars")
    return t


def _find(st, full_id):
    m = ID_RE.match(full_id or "")
    if not m or m.group(1) != st["code"]:
        raise DatgError(f"bad id {full_id!r}; expected {st['code']}-<G|D|A|T><hex2>")
    pre = m.group(2)
    bucket = {"G": "goals", "D": "items", "A": "items", "T": "tasks"}[pre]
    for o in st[bucket]:
        if o.get("id") == full_id:
            return pre, o
    raise DatgError(f"unknown id {full_id}")


def _all_ids(st):
    return {o.get("id") for k in ("goals", "items", "tasks") for o in st[k]}


def _next_id(st, pre, explicit=None):
    used = _all_ids(st)
    if explicit:
        m = ID_RE.match(explicit)
        if not m or m.group(1) != st["code"] or m.group(2) != pre:
            raise DatgError(f"bad id {explicit!r} for {pre}")
        if explicit in used:
            raise DatgError(f"id collision: {explicit} exists")
        return explicit
    n = int(st["next"].get(pre, "01"), 16)
    while n <= 255 and f"{st['code']}-{pre}{n:02x}" in used:
        n += 1
    if n > 255:
        raise DatgError(f"no free {pre} id (01-ff exhausted)")
    st["next"][pre] = f"{n + 1:02x}" if n < 255 else "ff"
    return f"{st['code']}-{pre}{n:02x}"


def _check_goal(st, goal):
    m = ID_RE.match(goal or "")
    if not m or m.group(2) != "G" or goal not in {g["id"] for g in st["goals"]}:
        raise DatgError(f"goal {goal!r} is not a goal of this project")
    g = [g for g in st["goals"] if g["id"] == goal][0]
    if g["status"] != "active":
        raise DatgError(f"goal {goal} is {g['status']}, not active")


def _opts(s):
    out = []
    for p in (s or "").split(","):
        p = p.strip()
        if p:
            out.append({"l": p.rstrip("*"), "rec": p.endswith("*")})
    return out


def op_init(project, code):
    if not CODE_RE.match(code or ""):
        raise DatgError("code must be 2-8 chars, lowercase letters/digits, letter first")
    _, sp, _ = paths(project)
    with Locked(project):
        if os.path.exists(sp):
            raise DatgError("state.json exists; init refuses to overwrite")
        st = {"code": code, "v": 0, "config": {"block_after": DEFAULT_BLOCK_AFTER},
              "goals": [], "items": [], "tasks": [],
              "next": {p: "01" for p in PREFIXES}}
        commit(project, st, [{"id": f"{code}-init", "ev": "open", "note": "init"}])
    return f"initialised {code}"


def apply_add(st, a):
    pre = a["kind"]
    if pre not in PREFIXES:
        raise DatgError("kind must be G, D, A or T")
    title = _title(a.get("title"))
    new_id = _next_id(st, pre, a.get("id"))
    if pre == "G":
        rank = a.get("rank")
        if rank is None:
            rank = len(st["goals"]) + 1
        if not isinstance(rank, int) or rank < 1:
            raise DatgError("rank must be an integer >= 1")
        obj = {"id": new_id, "rank": rank, "title": title, "finish": a.get("finish") or "",
               "status": "active", "maps": a.get("maps") or ""}
        st["goals"].append(obj)
    else:
        _check_goal(st, a.get("goal"))
        if pre == "T":
            obj = {"id": new_id, "goal": a["goal"], "title": title, "status": a.get("status") or "open",
                   "to": a.get("to") or "", "outcome": ""}
            if obj["status"] not in STATUS["T"][:2]:
                raise DatgError("new task status must be open or running")
            st["tasks"].append(obj)
        else:
            obj = {"id": new_id, "kind": "question" if pre == "D" else "action", "status": "open",
                   "goal": a["goal"], "title": title, "opts": a.get("opts") or [],
                   "answer": "", "outcome": "", "esc": None}
            st["items"].append(obj)
    ev = "drift" if pre == "D" and title.lower().startswith("drift:") else "open"
    return {"id": new_id, "ev": ev, "note": title}


def apply_update(st, a):
    pre, o = _find(st, a.get("id"))
    evs = []
    if a.get("title") is not None:
        o["title"] = _title(a["title"])
    if a.get("rank") is not None:
        if pre != "G" or not isinstance(a["rank"], int) or a["rank"] < 1:
            raise DatgError("rank applies to goals, integer >= 1")
        o["rank"] = a["rank"]
        evs.append("reprio")
    if a.get("esc") is not None:
        if pre not in "DA" or a["esc"] not in ("architect", ""):
            raise DatgError("esc applies to D/A, value 'architect' or ''")
        o["esc"] = a["esc"] or None
        if a["esc"]:
            evs.append("escalated")
    if a.get("answer") is not None:
        if pre not in "DA":
            raise DatgError("answer applies to D/A")
        o["answer"] = a["answer"]
        o["status"] = "answered"
        evs.append("answered")
    if a.get("to") is not None:
        if pre != "T":
            raise DatgError("to applies to tasks")
        o["to"] = a["to"]
    if a.get("outcome") is not None:
        if pre == "G":
            raise DatgError("outcome does not apply to goals")
        o["outcome"] = a["outcome"]
    if a.get("status") is not None:
        if a["status"] not in STATUS[pre]:
            raise DatgError(f"status for {pre} must be one of {STATUS[pre]}")
        if a["status"] == "done" and pre != "G" and not o.get("outcome"):
            raise DatgError("done needs a verified one-line outcome")
        o["status"] = a["status"]
        if a["status"] == "done":
            evs.append("done")
    return [{"id": o["id"], "ev": e, "note": a.get("note") or o.get("title", "")} for e in (evs or ["update"])]


def apply_op(st, a):
    verb = a.get("op")
    if verb == "add":
        return [apply_add(st, a)]
    if verb == "update":
        return apply_update(st, a)
    if verb == "close":
        if not a.get("outcome"):
            raise DatgError("close needs --outcome")
        return apply_update(st, {"id": a.get("id"), "outcome": a["outcome"], "status": "done"})
    raise DatgError(f"unknown op {verb!r}")


def write_ops(project, ops):
    """Apply ops to a copy of the state; commit once (all-or-nothing)."""
    with Locked(project):
        st = load_state(project)
        evs = []
        for a in ops:
            if not isinstance(a, dict):
                raise DatgError("each op must be an object")
            evs.extend(apply_op(st, a))
        commit(project, st, evs)
    return ", ".join(e["id"] for e in evs)


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


def hook_output(event, st, payload, project, now=None):
    """Return the hook JSON dict for the event, or None for no output."""
    if event == "SessionStart":
        py = os.path.abspath(__file__)
        text = RULES.format(py=py, code=st["code"])
        s = summary(st)
        return {"hookSpecificOutput": {"hookEventName": "SessionStart",
                                       "additionalContext": text + (" " + s if s else "")}}
    if event == "PreToolUse" and payload.get("tool_name") in ("Agent", "Task"):
        ti = payload.get("tool_input") or {}
        dec, msg = check_dispatch(st, ti.get("prompt") or "", now)
        if dec == "block":
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                           "permissionDecision": "deny",
                                           "permissionDecisionReason": msg}}
        if dec == "warn":
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
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    try:
        if a.verb == "hook":
            out = run_hook(sys.stdin.read())
            if out:
                print(out)
            return 0
        if a.verb == "init":
            print(op_init(a.project, a.code))
        elif a.verb == "add":
            op = {"op": "add", "kind": a.kind, "title": a.title, "goal": a.goal, "rank": a.rank,
                  "finish": a.finish, "maps": a.maps, "opts": _opts(a.opts), "to": a.to, "id": a.id}
            print(write_ops(a.project, [op]))
        elif a.verb == "update":
            op = {k: getattr(a, k) for k in ("title", "status", "answer", "outcome", "esc", "to", "note", "rank")}
            op.update({"op": "update", "id": a.id})
            print(write_ops(a.project, [op]))
        elif a.verb == "close":
            print(write_ops(a.project, [{"op": "close", "id": a.id, "outcome": a.outcome}]))
        elif a.verb == "batch":
            try:
                ops = json.loads(sys.stdin.read())
            except ValueError as e:
                raise DatgError(f"batch input is not JSON ({e})")
            if not isinstance(ops, list):
                raise DatgError("batch input must be a JSON list")
            print(write_ops(a.project, ops))
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

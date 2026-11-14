#!/usr/bin/env python3
"""task_pulse.py: deterministic task pulse, no model (stdlib only). Not wired into the poller or any cron.

Why: the owner 2026-11-10: PMs and the Architect tag work with a Task ID, hand-backs say which task was worked on,
and the task list shows work being done. Design review MB1047 and rulings D09 (warn-only first, refuse after
14 days at 90%+ compliance, earliest 2026-11-24) and the 2026-11-10 refinement (tasks are persistent, few,
and exist only at Architect/PM level; a WIP cap drives them to completion).
What: verbs (all take --dry-run where they write):
  run [--file F | --stdin] [--sender S]   parse the strict tag on the LAST line of each new PM message in
      inbox/pm-messages.jsonl (past a stored cursor) or of the given hand-back text. For a live task:
      dashboard T gets `reported` {pct, by, at, done, est, remaining, flags, label} and `last_msg_at`;
      the IL task gets `pulse` (note) and `last_msg_at`. `progress.pct`, status, updates and closing are never
      touched: PM figures are labelled "reported by <sender>", unverified.
      Orphans (id@MMDD not matching a live task, closed or unknown task, message older than the task) go to
      inbox/task-pulse-orphans.jsonl, malformed tags to inbox/task-pulse-rejects.jsonl; neither is applied.
  signals     last_signal_at on each open dashboard T from its `refs` (owner/repo#N PR, git:<dir>:<branch>,
      ci:owner/repo:branch) via gh and git. Network; run it only when asked.
  stale [--hours N]   open, unparked dashboard tasks with no verified signal in N hours (default 24).
  wip         per ledger: active tasks vs cap, and the quietest active tasks (finish, park or fold).
  compliance [--days 14]   per PM tagged/untagged counts and %, and whether the 90%/14-day refuse bar is met.
  status      counts of rejects, orphans and compliance misses.
Id mapping: the IL id (`ar-T0b`) is the one durable id the owner sees in tags. A dashboard T names its IL task in
`il` (set once: dashboard-db.py update items T93 --json '{"il":"ar-T0b"}'); a tag may carry either id and both
resolve to the same pair. A bare dashboard id (`T93`) is resolved against the dashboard only.
Idempotent: a tag line is applied once per (sender, line) (seen-set in inbox/pulse-state.json), an unchanged
result is not rewritten, one accepted pulse per task per 2 h, dashboard writes are version-pinned with one
retry on conflict. The cursor stops before a line whose write failed so a rerun retries it.
#11101: ported from the Architect prototype; a ledger is pinned through
datg_store._STATE_DIR, the module datg_dir reads since the #10161 split.
Layout: config, file and ledger helpers live in task_pulse_io.py (500-SLOC cap); every name
stays importable as task_pulse.<name>.
Test: tests/test_task_pulse.py
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402
import datg  # noqa: E402
import task_tag  # noqa: E402
from task_pulse_io import (  # noqa: E402,F401  (#11101: split for the 500-SLOC cap; re-exported)
    append_jsonl, dash_by_il, entry_dir, epoch, il_writable, in_ledger, iso_m, iso_s, jpath, ledger_dir,
    ledger_state, ledger_write, make_cfg, read_jsonl, schedule_entries, store_of,
)

RATE_S = 2 * 3600
SEEN_S = 7 * 86400
EARLIEST_REFUSE = "2026-11-24"
REPORTABLE = ("progress", "done", "head", "blocker", "decision")
REF_PR = re.compile(r"^([\w.-]+/[\w.-]+)#(\d+)$")


# ---- pulse state and cursor ---------------------------------------------------------

def load_pstate(cfg):
    try:
        with open(jpath(cfg, "pulse-state.json"), encoding="utf-8") as f:
            st = json.load(f)
        if isinstance(st, dict):
            st.setdefault("offset", 0)
            st.setdefault("seen", {})
            return st
    except (OSError, ValueError):
        pass
    return {"offset": 0, "seen": {}}


def save_pstate(cfg, st):
    if cfg["dry"]:
        return
    os.makedirs(cfg["inbox"], exist_ok=True)
    p = jpath(cfg, "pulse-state.json")
    tmp = f"{p}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(st, f)
    os.replace(tmp, p)


def read_lines(path, off):
    """[(start, end, msg)] for every complete line past off; a partial last line waits; a rotated file restarts at 0."""
    size = os.path.getsize(path) if os.path.exists(path) else 0
    if size < off:
        off = 0
    if size <= off:
        return []
    with open(path, "rb") as f:
        f.seek(off)
        chunk = f.read()
    out, pos = [], off
    for raw in chunk[:chunk.rfind(b"\n") + 1].split(b"\n")[:-1]:
        end = pos + len(raw) + 1
        try:
            msg = json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            msg = None
        if isinstance(msg, dict):
            out.append((pos, end, msg))
        pos = end
    return out


def sender_of(msg):
    return str(msg.get("sender") or msg.get("session") or "?").removeprefix("mcp:")


# ---- resolve a tag to its live task -------------------------------------------------

def resolve(cfg, store, tag, ts):
    """{ok: True, il, il_dir, dash} or {ok: False, reason}. Never writes."""
    dash = il_id = il_dir = il_task = None
    if tag["kind"] == "il":
        il_id = tag["id"]
        il_dir = ledger_dir(cfg, il_id.split("-")[0])
        if not il_dir:
            return {"ok": False, "reason": f"no ledger for code {il_id.split('-')[0]}"}
        try:
            il_task = next((t for t in ledger_state(il_dir)["tasks"] if t["id"] == il_id), None)
        except datg.DatgError as e:
            return {"ok": False, "reason": f"ledger unreadable: {e}"}
        if il_task is None or il_task.get("status") in ("done", "parked"):
            return {"ok": False, "reason": f"unknown, closed or parked task {il_id}"}
        dash = dash_by_il(store, il_id)
        created = il_task.get("created") or (dash["data"].get("created") if dash else None)
        mmdd = datg.task_mmdd({"created": created})
        if not mmdd:
            return {"ok": False, "reason": f"{il_id} has no created stamp to check @{tag['stamp']} against"}
        if mmdd != tag["stamp"]:
            return {"ok": False, "reason": f"{il_id}@{tag['stamp']} but the live {il_id} was created @{mmdd} (reused id)"}
    else:
        try:
            dash = store.get("items", tag["id"])
        except ds.NotFound:
            return {"ok": False, "reason": f"no dashboard task {tag['id']}"}
        x = dash["data"]
        if x.get("kind") != "task" or x.get("status") != "open" or x.get("parked"):
            return {"ok": False, "reason": f"dashboard {tag['id']} is not an open task"}
        created = x.get("created")
        il_id = x.get("il")
        if il_id:
            il_dir = ledger_dir(cfg, il_id.split("-")[0])
            try:
                il_task = next((t for t in ledger_state(il_dir)["tasks"] if t["id"] == il_id), None) if il_dir else None
            except datg.DatgError:
                il_task = None
            if il_task is None or il_task.get("status") in ("done", "parked"):
                il_id = il_dir = None
    c = epoch(created)
    if c is not None and ts < c:
        return {"ok": False, "reason": f"message {iso_s(ts)} is older than the task ({created})"}
    return {"ok": True, "il": il_id if il_task else None, "il_dir": il_dir if il_task else None,
            "il_task": il_task, "dash": dash}


# ---- distrust flags and the writes ---------------------------------------------------

def flags_for(old_rep, pct, last_signal, ts):
    """(flags, band_since). Pure. no_signal: pct rose with no commit/PR/CI signal since the previous report;
    jump30: +30 points or more in one pulse; stuck: 80-95 for 24 h."""
    flags = []
    if pct is None:
        return list(old_rep.get("flags") or []), old_rep.get("band_since")
    prev = old_rep.get("pct")
    if prev is None or pct > prev:
        prev_at = epoch(old_rep.get("at")) if prev is not None else None
        sig = epoch(last_signal) if last_signal else None
        if pct > 0 and (sig is None or (prev_at is not None and sig <= prev_at)):
            flags.append("no_signal")
    if prev is not None and pct - prev >= 30:
        flags.append("jump30")
    since = None
    if 80 <= pct <= 95:
        since = old_rep.get("band_since") if prev is not None and 80 <= prev <= 95 and old_rep.get("band_since") else iso_s(ts)
        if ts - epoch(since) >= 24 * 3600:
            flags.append("stuck")
    return flags, since


def build_reported(old, tag, sender, ts):
    rep = dict(old.get("reported") or {})
    flags, since = flags_for(rep, tag["pct"], old.get("last_signal_at"), ts)
    new = {k: v for k, v in rep.items() if k in ("pct", "done", "est", "remaining")}
    for k in ("pct", "done", "est", "remaining"):
        if tag.get(k) is not None:
            new[k] = tag[k]
    new.update(by=sender, at=iso_s(ts), flags=flags, label=f"reported by {sender}, unverified")
    if since:
        new["band_since"] = since
    return new


def write_dash(cfg, store, dash_id, tag, sender, ts):
    """applied | unchanged | rate-limited | stale-msg | conflict | dry-run, with the figures. Version-pinned, one retry."""
    for _ in range(2):
        doc = store.get("items", dash_id)
        old = doc["data"]
        rep_at, msg_at = epoch((old.get("reported") or {}).get("at")), epoch(old.get("last_msg_at"))
        if msg_at is not None and ts < msg_at:
            return "stale-msg", None
        if rep_at is not None and 0 <= ts - rep_at < RATE_S:
            return "rate-limited", None
        rep = build_reported(old, tag, sender, ts)
        patch = {"reported": rep, "last_msg_at": iso_s(ts)}
        if {k: v for k, v in (old.get("reported") or {}).items() if k != "at"} == {k: v for k, v in rep.items() if k != "at"}:
            return "unchanged", rep
        if cfg["dry"]:
            return "dry-run", rep
        try:
            store.update("items", dash_id, patch, if_version=doc["version"])
            return "applied", rep
        except ds.Conflict:
            continue
    return "conflict", None


def note_of(sender, tag):
    bits = [sender, f"done: {tag['done']}" if tag.get("done") else "", f"left: {tag['remaining']}" if tag.get("remaining") else "",
            f"est {tag['est']}" if tag.get("est") else "", f"{tag['pct']}%" if tag.get("pct") is not None else ""]
    n = "; ".join(b for b in bits if b)
    return n if len(n) <= 200 else n[:199] + "…"


def write_il(cfg, d, task, sender, tag, ts):
    note = note_of(sender, tag)
    if task.get("pulse") == note:
        return "unchanged"
    if cfg["dry"]:
        return "dry-run"
    ledger_write(d, [{"op": "pulse", "id": task["id"], "note": note, "last_msg_at": iso_m(ts)}])
    return "applied"


# ---- one message ----------------------------------------------------------------------

def handle(cfg, store, pstate, sender, ts, text):
    """None (no tag) or {task, status, detail}. status: applied|dry-run|unchanged|dup|rate-limited|stale-msg|orphan|rejected|conflict|error."""
    kind, tag = task_tag.parse_pulse(text)
    if kind == "none":
        return None
    if kind == "reject":
        append_jsonl(cfg, "task-pulse-rejects.jsonl", {"ts": iso_s(ts), "sender": sender, "reason": tag,
                                                      "line": (text or "").strip().splitlines()[-1][:300]})
        return {"task": "-", "status": "rejected", "detail": tag}
    key = hashlib.sha1(f"{sender}|{tag['line']}".encode()).hexdigest()[:16]
    if key in pstate["seen"]:
        return {"task": tag["id"], "status": "dup", "detail": ""}
    try:
        r = resolve(cfg, store, tag, ts)
    except ds.StoreError as e:
        return {"task": tag["id"], "status": "error", "detail": str(e)}
    if not r["ok"]:
        append_jsonl(cfg, "task-pulse-orphans.jsonl", {"ts": iso_s(ts), "sender": sender, "task": tag["id"],
                                                       "stamp": tag.get("stamp"), "reason": r["reason"], "line": tag["line"][:300]})
        return {"task": tag["id"], "status": "orphan", "detail": r["reason"]}
    out, detail = [], ""
    try:
        if r["dash"]:
            st, rep = write_dash(cfg, store, r["dash"]["id"], tag, sender, ts)
            out.append(st)
            if rep:
                detail = f"reported {rep.get('pct', '-')}% by {sender}" + (f" flags={','.join(rep['flags'])}" if rep["flags"] else "")
        if r["il"] and il_writable(cfg, r["il"].split("-")[0]) and (not r["dash"] or out[-1] not in ("rate-limited", "stale-msg")):
            out.append("il:" + write_il(cfg, r["il_dir"], r["il_task"], sender, tag, ts))
    except (ds.StoreError, datg.DatgError, OSError) as e:
        return {"task": tag["id"], "status": "error", "detail": str(e)}
    if "conflict" in out:
        return {"task": tag["id"], "status": "conflict", "detail": detail}
    status = next((s for s in out if s in ("applied", "dry-run")), None) or next((s.split(":")[-1] for s in out if s.startswith("il:") and s != "il:unchanged"), None) \
        or (out[0] if out else "unchanged")
    if status in ("applied", "unchanged") and not cfg["dry"]:
        pstate["seen"][key] = cfg["now"]
    return {"task": tag["id"] + (f" ({r['dash']['id']})" if r["dash"] and tag["kind"] == "il" else ""), "status": status,
            "detail": detail or ",".join(out)}


def run_items(cfg, items):
    """items: [(start, end, sender, ts, text)] -> (results, safe_offset or None). Stops the cursor before a failed line."""
    store = store_of(cfg)
    pulse_st = load_pstate(cfg)
    results, safe = [], None
    failed = False
    for start, end, sender, ts, text in items:
        r = handle(cfg, store, pulse_st, sender, ts, text)
        if r:
            results.append(r)
        if r and r["status"] in ("conflict", "error"):
            failed = True
        if not failed:
            safe = end
    pulse_st["seen"] = {k: v for k, v in pulse_st["seen"].items() if cfg["now"] - v < SEEN_S}
    return results, safe, pulse_st


def cmd_run(cfg, text=None, sender="agent"):
    if text is not None:
        results, _, pulse_st = run_items(cfg, [(None, None, sender, cfg["now"], text)])
        save_pstate(cfg, pulse_st)
        return results
    ps0 = load_pstate(cfg)
    lines = read_lines(os.path.join(cfg["inbox"], "pm-messages.jsonl"), ps0["offset"])
    items = []
    for start, end, m in lines:
        items.append((start, end, sender_of(m), epoch(m.get("ts")) or cfg["now"], str(m.get("text") or "")))
    results, safe, pulse_st = run_items(cfg, items)
    if lines and safe is not None:
        pulse_st["offset"] = safe
    elif lines and items and not any(r["status"] in ("conflict", "error") for r in results):
        pulse_st["offset"] = lines[-1][1]
    save_pstate(cfg, pulse_st)
    return results


# ---- signals, stale, wip, status, compliance -------------------------------------------

def default_runner(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        return r.stdout if r.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def ref_times(ref, runner):
    """Epoch times of the verifiable signals behind one ref (PR update/check, commit, CI run)."""
    out = []
    m = REF_PR.match(ref)
    if m:
        j = runner(["gh", "pr", "view", m.group(2), "-R", m.group(1), "--json", "updatedAt,statusCheckRollup"])
        try:
            d = json.loads(j or "{}")
        except ValueError:
            d = {}
        out += [d.get("updatedAt")] + [c.get("completedAt") for c in d.get("statusCheckRollup") or [] if isinstance(c, dict)]
    elif ref.startswith("git:") and ref.count(":") >= 2:
        _, path, branch = ref.split(":", 2)
        out.append((runner(["git", "-C", path, "log", "-1", "--format=%cI", branch]) or "").strip())
    elif ref.startswith("ci:") and ref.count(":") >= 2:
        _, repo, branch = ref.split(":", 2)
        try:
            runs = json.loads(runner(["gh", "run", "list", "-R", repo, "-b", branch, "-L", "1", "--json", "createdAt,updatedAt"]) or "[]")
        except ValueError:
            runs = []
        out += [x for r in runs if isinstance(r, dict) for x in (r.get("createdAt"), r.get("updatedAt"))]
    return [t for t in (epoch(x) for x in out if x) if t and t > 946684800]


def cmd_signals(cfg):
    store = store_of(cfg)
    runner = cfg["runner"] or default_runner
    res = []
    for d in store.list_docs("items"):
        x = d["data"]
        if x.get("kind") != "task" or x.get("status") != "open" or not x.get("refs"):
            continue
        times = [t for ref in x["refs"] for t in ref_times(str(ref), runner)]
        if not times:
            res.append(f"{d['id']}: no signal found")
            continue
        new = iso_s(max(times))
        if (epoch(x.get("last_signal_at")) or 0) >= max(times):
            res.append(f"{d['id']}: unchanged {x.get('last_signal_at')}")
        elif cfg["dry"]:
            res.append(f"{d['id']}: dry-run last_signal_at {new}")
        else:
            try:
                store.update("items", d["id"], {"last_signal_at": new}, if_version=d["version"])
                res.append(f"{d['id']}: last_signal_at {new}")
            except ds.Conflict:
                res.append(f"{d['id']}: conflict (rerun)")
    return res


def cmd_stale(cfg, hours=24.0):
    out = []
    for d in store_of(cfg).list_docs("items"):
        x = d["data"]
        if x.get("kind") != "task" or x.get("status") != "open" or x.get("parked"):
            continue
        sig = epoch(x.get("last_signal_at"))
        basis = sig if sig is not None else epoch(x.get("created"))
        if basis is None or (cfg["now"] - basis) / 3600 < hours:
            continue
        out.append({"id": d["id"], "il": x.get("il", ""), "signal_age_h": round((cfg["now"] - basis) / 3600, 1),
                    "has_signal": sig is not None, "last_msg_at": x.get("last_msg_at", ""), "session": x.get("session", ""),
                    "title": x.get("title", "")})
    return sorted(out, key=lambda r: -r["signal_age_h"])


def cmd_wip(cfg):
    rows, seen = [], set()
    for e in schedule_entries(cfg):
        d = entry_dir(e)
        if not d or d in seen:
            continue
        seen.add(d)
        try:
            st = ledger_state(d)
        except datg.DatgError:
            continue
        act = [t for t in st["tasks"] if t.get("status") in datg.ACTIVE]

        def fresh(t):
            ts = [epoch(t.get(k)) for k in ("last_signal_at", "last_msg_at", "created")]
            return max([x for x in ts if x is not None] or [0])
        quiet = sorted(act, key=fresh)[:3]
        rows.append({"session": e["name"], "code": st["code"], "active": len(act), "cap": datg.wip_cap(st),
                     "parked": sum(1 for t in st["tasks"] if t.get("status") == "parked"),
                     "quietest": [{"id": t["id"], "last": iso_s(fresh(t)) if fresh(t) else "never", "title": t.get("title", "")} for t in quiet]})
    return rows


def cmd_status(cfg):
    n = {k: len(read_jsonl(jpath(cfg, f))) for k, f in (("rejects", "task-pulse-rejects.jsonl"), ("orphans", "task-pulse-orphans.jsonl"),
                                                       ("misses", "task-compliance.jsonl"))}
    return f"task pulse: {n['rejects']} rejects, {n['orphans']} orphans, {n['misses']} compliance misses"


def cmd_compliance(cfg, days=14, kinds=REPORTABLE):
    """Per PM tagged/untagged counts over the window, and whether the D09 refuse bar is met."""
    lo = cfg["now"] - days * 86400
    per = {}
    for m in read_jsonl(os.path.join(cfg["inbox"], "pm-messages.jsonl")):
        t = epoch(m.get("ts"))
        if t is None or t < lo or t > cfg["now"] or (kinds and str(m.get("kind") or "") not in kinds):
            continue
        row = per.setdefault(sender_of(m), {"tagged": 0, "untagged": 0})
        row["tagged" if task_tag.has_task(str(m.get("text") or "")) else "untagged"] += 1
    rows = []
    for who, r in sorted(per.items()):
        tot = r["tagged"] + r["untagged"]
        rows.append({"pm": who, **r, "total": tot, "pct": round(100 * r["tagged"] / tot, 1)})
    tg, tot = sum(r["tagged"] for r in rows), sum(r["total"] for r in rows)
    pct = round(100 * tg / tot, 1) if tot else 0.0
    misses = [r for r in read_jsonl(jpath(cfg, "task-compliance.jsonl")) if (epoch(r.get("ts")) or 0) >= lo]
    reasons = []
    if cfg["now"] < epoch(EARLIEST_REFUSE):
        reasons.append(f"before {EARLIEST_REFUSE}")
    if pct < 90 or not tot:
        reasons.append(f"{pct}% < 90% over {days} days")
    return {"days": days, "kinds": list(kinds), "per_pm": rows, "tagged": tg, "total": tot, "pct": pct,
            "dispatch_relay_misses": len(misses), "refuse_eligible": not reasons, "blockers": reasons}


# ---- CLI ------------------------------------------------------------------------------------

def build_parser():
    ap = argparse.ArgumentParser(prog="task_pulse.py", description=__doc__.split("\n")[0])
    ap.add_argument("--inbox")
    ap.add_argument("--dash-root")
    ap.add_argument("--schedule")
    ap.add_argument("--ledgers", choices=("none", "own", "all"), help="which IL ledgers pulse notes are written to (default all)")
    sub = ap.add_subparsers(dest="verb", required=True)
    r = sub.add_parser("run")
    r.add_argument("--dry-run", action="store_true")
    r.add_argument("--file")
    r.add_argument("--stdin", action="store_true")
    r.add_argument("--sender", default="agent")
    s = sub.add_parser("signals")
    s.add_argument("--dry-run", action="store_true")
    st = sub.add_parser("stale")
    st.add_argument("--hours", type=float, default=24.0)
    st.add_argument("--json", action="store_true")
    w = sub.add_parser("wip")
    w.add_argument("--json", action="store_true")
    c = sub.add_parser("compliance")
    c.add_argument("--days", type=int, default=14)
    c.add_argument("--json", action="store_true")
    sub.add_parser("status")
    return ap


def main(argv=None):
    a = build_parser().parse_args(argv)
    cfg = make_cfg(inbox=a.inbox, dash_root=a.dash_root, schedule=a.schedule, ledgers=a.ledgers, dry=getattr(a, "dry_run", False))
    try:
        if a.verb == "run":
            text = None
            if a.stdin:
                text = sys.stdin.read()
            elif a.file:
                with open(a.file, encoding="utf-8") as f:
                    text = f.read()
            res = cmd_run(cfg, text, a.sender)
            for r in res:
                print(f"{r['task']}: {r['status']}" + (f" ({r['detail']})" if r["detail"] else ""))
            print(f"{len(res)} tag(s)" + (" [dry-run: nothing written]" if cfg["dry"] else ""))
            return 1 if any(r["status"] in ("conflict", "error") for r in res) else 0
        if a.verb == "signals":
            print("\n".join(cmd_signals(cfg)) or "no tasks with refs")
        elif a.verb == "stale":
            rows = cmd_stale(cfg, a.hours)
            if a.json:
                print(json.dumps(rows, indent=1))
            for r in rows:
                if not a.json:
                    print(f"{r['id']} il={r['il'] or '-'} no signal for {r['signal_age_h']}h"
                          f"{'' if r['has_signal'] else ' (never; age since created)'} last_msg={r['last_msg_at'] or '-'} {r['session']} {r['title']}")
            if not rows and not a.json:
                print(f"no open task is quiet for {a.hours}h")
        elif a.verb == "wip":
            rows = cmd_wip(cfg)
            if a.json:
                print(json.dumps(rows, indent=1))
            for r in rows:
                over = " OVER CAP" if r["active"] > r["cap"] else ""
                print(f"{r['session']} ({r['code']}): active {r['active']}/{r['cap']}, parked {r['parked']}{over}")
                for q in r["quietest"]:
                    print(f"  quiet: {q['id']} last={q['last']} {q['title']}")
        elif a.verb == "compliance":
            c = cmd_compliance(cfg, a.days)
            if a.json:
                print(json.dumps(c, indent=1))
            else:
                for r in c["per_pm"]:
                    print(f"{r['pm']}: {r['tagged']} tagged, {r['untagged']} untagged, {r['pct']}%")
                print(f"all: {c['tagged']}/{c['total']} = {c['pct']}% over {c['days']} days ({'/'.join(c['kinds'])}); "
                      f"{c['dispatch_relay_misses']} dispatch/relay misses; refuse bar: "
                      + ("met" if c["refuse_eligible"] else "not met (" + "; ".join(c["blockers"]) + ")"))
        elif a.verb == "status":
            print(cmd_status(cfg))
    except (ds.StoreError, datg.DatgError, OSError) as e:
        print(f"task_pulse: {type(e).__name__}: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""il-cron.py: deterministic scheduler for Intent Ledger cadence jobs (SPEC-IL-01 section 4).

Why: no cadence job may depend on Architect memory (the 10-15 pilot rescore was missed).
What: reads `jobs` and `sessions` from local/il-schedule.json, runs each job that is due,
keeps per-job state in inbox/il-cron-state.json. An overdue job runs at the next cycle; a
failed job is retried every cycle up to 3 times, then raised once as an alert. `step()` is
called by fleet-poll.py every cycle and runs jobs at most once per hour.
Job entry: {name, due (ISO date/time, first run), every ("1h", "7d"; absent = once),
command}. command is "builtin:<prune|rotate|size-check|pilot-rescore>" or a shell-split argv.
Builtins act on every project of `sessions` through the datg CLI (env DATG_CMD) and
il-archive.py (env IL_ARCHIVE_CMD); `pilot-rescore` writes local/briefs/<date>-t08-pilot-rescore.md
(env IL_BRIEFS_DIR) from each log.jsonl and state.json size.
CLI: il-cron.py [--dry-run] [--force] [--now ISO] [--list]
Env (tests): IL_SCHEDULE, IL_CRON_STATE, DATG_CMD, IL_ARCHIVE_CMD, IL_BRIEFS_DIR.
Test: scripts/test_il_cron.py
"""
import argparse
import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CYCLE_EVERY = 3500       # seconds between scheduler cycles (the poller wakes more often)
MAX_FAILS = 3
JOB_TIMEOUT = 300
if HERE not in sys.path:   # python3 -I drops the script directory
    sys.path.insert(0, HERE)
from datg_store import SIZE_WARN  # noqa: E402  (#10571: the ledger's own warn bar, caps.datg_size_warn; 32 KB by default)


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ctx_watch = _load("ctx_watch", "ctx-watch.py")  # schedule loading, iso parsing, IL paths
parse_iso, iso = ctx_watch.parse_iso, ctx_watch.iso


def state_path():
    return os.environ.get("IL_CRON_STATE") or os.path.join(
        os.environ.get("SUPERVISOR_INBOX_DIR") or os.path.join(ROOT, "inbox"), "il-cron-state.json")


def datg_cmd():
    return shlex.split(os.environ.get("DATG_CMD") or f"{sys.executable} {os.path.join(HERE, 'datg.py')}")


def archive_cmd():
    return shlex.split(os.environ.get("IL_ARCHIVE_CMD")
                       or f"{sys.executable} {os.path.join(HERE, 'il-archive.py')}")


def parse_every(s):
    m = re.fullmatch(r"(\d+)([mhd])", str(s or ""))
    return int(m.group(1)) * {"m": 60, "h": 3600, "d": 86400}[m.group(2)] if m else None


def run(argv):
    """(ok, output tail) of one subprocess; a missing command is a failure, not a crash."""
    try:
        r = subprocess.run(argv, capture_output=True, text=True, timeout=JOB_TIMEOUT)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, f"{type(e).__name__}: {e}"
    return r.returncode == 0, ((r.stdout or "") + (r.stderr or "")).strip()[-300:]


# --- builtin jobs ------------------------------------------------------------

def each_project(sched):
    return [e for e in sched.get("sessions") or [] if e.get("project")]


def initialized(e):
    """True once the project has an IL state.json; before B11 sets it up, a job skips it instead of failing."""
    return os.path.isfile(ctx_watch.state_path_for(e))


def job_over_projects(sched, verb):
    fails, skipped, n = [], [], 0
    for e in each_project(sched):
        if not initialized(e):
            skipped.append(f"skipped {e['name']}: not initialized")
            continue
        n += 1
        ok, out = run(datg_cmd() + ["--project", e["project"], verb])
        if not ok:
            fails.append(f"{e['name']}: {out}")
    return (not fails), "; ".join(fails + [f"{verb} ok on {n} projects"] * (not fails) + skipped)


def job_prune(sched, now):
    ok, out = job_over_projects(sched, "prune")
    ok2, out2 = run(archive_cmd() + ["dashboard", "prune"])
    return ok and ok2, "; ".join(x for x in (out, "" if ok2 else f"il-archive: {out2}") if x)


def job_rotate(sched, now):
    return job_over_projects(sched, "rotate")


def job_size_check(sched, now):
    big = []
    for e in each_project(sched):
        try:
            size = os.path.getsize(ctx_watch.state_path_for(e))
        except OSError:
            continue
        if size > SIZE_WARN:
            big.append(f"{e['name']} {size}B")
    bar = f"{SIZE_WARN // 1024} KB"
    return (not big), (f"state.json over {bar}: " + ", ".join(big)) if big else f"all within {bar}"


def read_log(path):
    """Log lines as dicts; accepts both shapes (`t`/`ev` and `ts`/`kind`)."""
    out = []
    try:
        with open(path) as f:
            for line in f:
                try:
                    d = json.loads(line)
                except ValueError:
                    continue
                out.append({"t": d.get("t") or d.get("ts"), "ev": d.get("ev") or d.get("kind"),
                            "id": d.get("id") or d.get("session")})
    except OSError:
        pass
    return out


def job_pilot_rescore(sched, now):
    rows, totals = [], {}
    for e in each_project(sched):
        d = ctx_watch.datg_dir(e["project"], e["name"])
        evs = read_log(os.path.join(d, "log.jsonl"))
        count = {}
        for x in evs:
            count[x["ev"]] = count.get(x["ev"], 0) + 1
        try:
            size = os.path.getsize(os.path.join(d, "state.json"))
        except OSError:
            size = None
        rows.append((e["name"], count, size))
        for k, v in count.items():
            totals[k] = totals.get(k, 0) + v
    briefs = os.environ.get("IL_BRIEFS_DIR") or os.path.join(ROOT, "local", "briefs")
    os.makedirs(briefs, exist_ok=True)
    path = os.path.join(briefs, f"{iso(now)[:10]}-t08-pilot-rescore.md")
    lines = [f"# T08 pilot rescore (il-cron), {iso(now)}", "",
             "Measures from each project's log.jsonl and state.json. Block-mode false blocks need a "
             "manual read of the `hook_block` lines; the count is only the candidate set.", "",
             "| Session | escalated | drift | hook_warning | hook_block | stale_il | state.json bytes |",
             "| --- | --- | --- | --- | --- | --- | --- |"]
    for name, c, size in rows:
        lines.append(f"| {name} | {c.get('escalated', 0)} | {c.get('drift', 0)} | "
                     f"{c.get('hook_warning', 0)} | {c.get('hook_block', 0)} | {c.get('stale_il', 0)} | "
                     f"{size if size is not None else 'n/a'} |")
    lines += ["", f"Totals: escalated {totals.get('escalated', 0)}, drift {totals.get('drift', 0)}, "
              f"hook_warning {totals.get('hook_warning', 0)}, hook_block {totals.get('hook_block', 0)}."]
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    return True, f"wrote {path}"


BUILTINS = {"prune": job_prune, "rotate": job_rotate, "size-check": job_size_check,
            "pilot-rescore": job_pilot_rescore}


def run_job(job, sched, now):
    cmd = job.get("command") or f"builtin:{job['name']}"
    if cmd.startswith("builtin:"):
        fn = BUILTINS.get(cmd[8:])
        if not fn:
            return False, f"unknown builtin {cmd[8:]}"
        return fn(sched, now)
    return run(shlex.split(cmd))


# --- scheduling --------------------------------------------------------------

def next_due(job, js):
    """Epoch when the job is next due, or None when a one-shot job has finished or given up."""
    every = parse_every(job.get("every"))
    if js.get("fails"):
        return 0  # failed last time: retry at this cycle
    if js.get("last_ok") is not None or js.get("gave_up_at") is not None:
        if every is None:
            return None
        base = js.get("last_ok") if js.get("last_ok") is not None else js["gave_up_at"]
        return base + every
    return parse_iso(job.get("due")) or 0


def cycle(sched, state, now, dry_run=False, force=False):
    """Run due jobs. Returns alerts [(fingerprint, detail)]. Mutates `state`."""
    alerts = []
    jobs_state = state.setdefault("jobs", {})
    for job in sched.get("jobs") or []:
        name = job.get("name")
        if not name or job.get("enabled") is False:
            continue
        js = jobs_state.setdefault(name, {"fails": 0})
        due = next_due(job, js)
        if due is None or (now < due and not force):
            continue
        if dry_run:
            print(f"  would run {name} (due {iso(due)})")
            continue
        ok, out = run_job(job, sched, now)
        js["last_run"], js["last_result"] = now, ("ok" if ok else "fail") + ": " + out[:200]
        if ok:
            js["last_ok"], js["fails"] = now, 0
        else:
            js["fails"] += 1
            if js["fails"] >= MAX_FAILS:
                js["gave_up_at"], js["fails"] = now, 0
                js.pop("last_ok", None)
                alerts.append((f"il-cron|{name}|{iso(now)[:13]}",
                               f"job {name} failed {MAX_FAILS} times: {out[:150]}"))
        if name == "pilot-rescore" and ok:
            alerts.append((f"il-cron|{name}|done", f"pilot rescore done: {out}"))
        if name == "size-check" and not ok:
            alerts.append((f"il-cron|size|{iso(now)[:13]}", out))
    return alerts


def load_state():
    return ctx_watch.load_json(state_path(), {})


def save_state(state):
    ctx_watch.save_json_atomic(state_path(), state)


def step(now=None, dry_run=False, force=False, sched=None):
    """Poller entry (hourly gate inside). Returns [(fingerprint, detail)]; never raises."""
    now = time.time() if now is None else now
    try:
        sched = sched if sched is not None else ctx_watch.load_schedule()
        state = load_state()
        if not force and now - state.get("last_cycle", 0) < CYCLE_EVERY:
            return []
        alerts = cycle(sched, state, now, dry_run, force)
        if not dry_run:
            state["last_cycle"] = now
            save_state(state)
        return alerts
    except Exception as e:  # the scheduler must never break the poll loop
        return [(f"il-cron|error|{type(e).__name__}", f"il-cron error {type(e).__name__}: {e}")]


def main(argv=None):
    ap = argparse.ArgumentParser(description="Run due Intent Ledger cadence jobs.")
    ap.add_argument("--dry-run", action="store_true", help="print due jobs, run nothing")
    ap.add_argument("--force", action="store_true", help="ignore the hourly gate and due times")
    ap.add_argument("--now", help="ISO time to run at (default: now)")
    ap.add_argument("--list", action="store_true", help="print job state and exit")
    a = ap.parse_args(argv)
    now = parse_iso(a.now) if a.now else time.time()
    if now is None:
        print("il-cron: bad --now", file=sys.stderr)
        return 2
    if a.list:
        print(json.dumps(load_state(), indent=1))
        return 0
    for fp, detail in step(now=now, dry_run=a.dry_run, force=a.force):
        print(f"ALERT {fp}: {detail}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

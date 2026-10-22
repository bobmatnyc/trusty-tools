"""datg_log.py: datg prune, archive lookup, log rotation, raw log lines and escalation notices.

Why: datg.py passed the 500-SLOC cap; #10161 P3 split it by section, bodies moved verbatim.
What: op_prune, archive_lookup, op_rotate, write_ops, append_log, pm_session and
notify_escalations (runs pm-notify.sh beside this file). datg.py re-exports every name.
Test: tests/test_datg.py
"""
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

from datg_ops import write_ops_ev
from datg_store import (DatgError, Locked, _atomic_write, _size, answered_times, commit, load_state, log_rec,
                        now_iso, paths, prune_state)

# ---- logs, archive, prune, rotate ------------------------------------------------

def op_prune(project, dry_run=False, hours=24, require_outcome=False):
    with Locked(project):
        st = load_state(project)
        before = _size(paths(project)[1])
        gone = prune_state(st, answered_times(project), hours, require_outcome)
        ids = [o["id"] for _, o in gone]
        if not gone:
            return "nothing to prune"
        if dry_run:
            return f"dry-run: would archive {len(ids)}: {', '.join(ids)}"
        commit(project, st, [{"id": f"{st['code']}-prune", "ev": "update", "note": f"archived {len(ids)}"}], gone)
        return f"archived {len(ids)} items; state.json {before} -> {_size(paths(project)[1])} B"


def archive_lookup(project, item_id):
    """Last valid archive line for item_id (orphans of failed commits, cv > state v, are skipped)."""
    ap = os.path.join(paths(project)[0], "archive.jsonl")
    try:
        v = load_state(project)["v"]
    except DatgError:
        v = None
    hit = None
    try:
        with open(ap, encoding="utf-8") as f:
            for ln in f:
                try:
                    r = json.loads(ln)
                except ValueError:
                    continue
                cv = r.get("cv")
                orphan = v is not None and isinstance(cv, int) and cv > v
                if (r.get("item") or {}).get("id") == item_id and not orphan:
                    hit = r
    except OSError:
        pass
    if hit is None:
        raise DatgError(f"{item_id} not in archive.jsonl")
    return json.dumps(hit, ensure_ascii=False)


def op_rotate(project, days=7, now=None):
    """Move log lines older than `days` to log-<YYYYMMDD>.jsonl (rotation day); keep the rest."""
    now = now or datetime.now(timezone.utc)
    cutoff = now.timestamp() - days * 86400
    with Locked(project):
        d, _, lp = paths(project)
        try:
            with open(lp, encoding="utf-8") as f:
                lines = f.read().splitlines()
        except OSError:
            return "no log"
        old, keep = [], []
        for ln in lines:
            r = log_rec(ln)
            (old if r and r[0] and r[0].timestamp() < cutoff else keep).append(ln)
        if not old:
            return f"nothing to rotate ({_size(lp)} B, {len(lines)} lines)"
        rp = os.path.join(d, f"log-{now:%Y%m%d}.jsonl")
        prev = ""
        if os.path.exists(rp):
            with open(rp, encoding="utf-8") as f:
                prev = f.read()
        _atomic_write(rp, prev + "\n".join(old) + "\n")
        _atomic_write(lp, "".join(x + "\n" for x in keep))
        return f"rotated {len(old)} lines to {os.path.basename(rp)}; log.jsonl {len(keep)} lines"


def write_ops(project, ops):
    return ", ".join(e["id"] for e in write_ops_ev(project, ops)[0])


def append_log(project, rec):
    """Append one raw JSON line to log.jsonl (O_APPEND, one write)."""
    line = (json.dumps(rec, separators=(",", ":"), ensure_ascii=False) + "\n").encode()
    fd = os.open(paths(project)[2], os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    try:
        os.write(fd, line)
    finally:
        os.close(fd)


def pm_session(code):
    """The sending PM's tmux session: $DATG_SESSION, else the TMUX_PANE session, else the project code."""
    if os.environ.get("DATG_SESSION"):
        return os.environ["DATG_SESSION"]
    pane = os.environ.get("TMUX_PANE")
    if pane:
        try:
            r = subprocess.run(["tmux", "display-message", "-p", "-t", pane, "#{session_name}"],
                               capture_output=True, text=True, timeout=5)
            if r.returncode == 0 and r.stdout.strip():
                return r.stdout.strip()
        except (OSError, subprocess.SubprocessError):
            pass
    return code


def notify_escalations(project, evs, st):
    """Queue each `escalated` event as a D10 `decision`. A failure is logged and never raised."""
    cmd = os.environ.get("DATG_NOTIFY_CMD") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "pm-notify.sh")
    sess = pm_session(st["code"])
    for ev in evs:
        if ev["ev"] != "escalated":
            continue
        item = next((i for i in st["items"] if i.get("id") == ev["id"]), {})
        opts = ", ".join(o["l"] + ("*" if o.get("rec") else "") for o in item.get("opts") or [])
        text = f"{ev['id']} {item.get('title', '')}" + (f" [options: {opts}]" if opts else "")
        try:
            r = subprocess.run([cmd, "--kind", "decision", "--session", sess, text],
                               capture_output=True, text=True, timeout=30)
            err = "" if r.returncode == 0 else f"exit {r.returncode}: {(r.stderr or r.stdout).strip()[:200]}"
        except (OSError, subprocess.SubprocessError) as e:
            err = f"{type(e).__name__}: {e}"
        if err:
            print(f"datg: pm-notify failed for {ev['id']} ({err})", file=sys.stderr)
            try:
                append_log(project, {"ts": now_iso(), "session": sess, "kind": "notify_failure",
                                     "text": f"{ev['id']} pm-notify failed: {err}"})
            except OSError as e:
                print(f"datg: log write failed ({e})", file=sys.stderr)

"""task_pulse_io.py: config, files, IL ledgers and dashboard lookups for task_pulse.py (stdlib only).

Why: #11101 ported task_pulse.py from the Architect prototype; with the split ledger modules it passed the
500-SLOC cap, so its helpers moved here verbatim. task_pulse.py re-exports every name.
What: ISO/epoch time helpers, make_cfg, the inbox JSONL readers and writers, the il-schedule.json ledger
lookup, in_ledger (pins datg_store._STATE_DIR around one ledger call) and dash_by_il.
Test: tests/test_task_pulse.py
"""
import json
import os
import sys
import time
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402
import datg  # noqa: E402
import datg_store  # noqa: E402
import task_tag  # noqa: E402

ROOT = os.path.dirname(HERE)


def iso_s(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def iso_m(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


def epoch(s):
    """Epoch seconds of an ISO timestamp (date, minute or second precision, optional Z); None if unreadable."""
    try:
        return datetime.fromisoformat(str(s).replace("Z", "")).replace(tzinfo=timezone.utc).timestamp()
    except ValueError:
        return None


def make_cfg(**kw):
    c = {"inbox": task_tag.inbox(), "dash_root": os.environ.get("DASHBOARD_ROOT") or ds.DEFAULT_ROOT,
         "schedule": os.path.join(ROOT, "local", "il-schedule.json"), "ledgers": "all", "arch_code": "ar",
         "dry": False, "now": time.time(), "runner": None}
    c.update({k: v for k, v in kw.items() if v is not None})
    return c


def store_of(cfg):
    return ds.Store(root=cfg["dash_root"], actor="task_pulse")


def jpath(cfg, name):
    return os.path.join(cfg["inbox"], name)


def append_jsonl(cfg, name, rec):
    if cfg["dry"]:
        return
    os.makedirs(cfg["inbox"], exist_ok=True)
    with open(jpath(cfg, name), "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def read_jsonl(path):
    out = []
    try:
        with open(path, encoding="utf-8") as f:
            for ln in f:
                try:
                    r = json.loads(ln)
                except ValueError:
                    continue
                if isinstance(r, dict):
                    out.append(r)
    except OSError:
        pass
    return out


def schedule_entries(cfg):
    try:
        with open(cfg["schedule"], encoding="utf-8") as f:
            rows = json.load(f).get("sessions")
    except (OSError, ValueError, AttributeError):
        return []
    return [e for e in rows if isinstance(e, dict) and e.get("name") and e.get("project")] if isinstance(rows, list) else []


def entry_dir(e):
    base = os.path.join(e["project"], ".trusty-mpm", "datg")
    cands = [os.path.dirname(e["state_path"]) if e.get("state_path") else None, os.path.join(base, e["name"]), base]
    return next((c for c in cands if c and os.path.isfile(os.path.join(c, "state.json"))), None)


def ledger_dir(cfg, code):
    for e in schedule_entries(cfg):
        if e.get("code") == code and entry_dir(e):
            return entry_dir(e)
    return None


def in_ledger(d, fn):
    old = datg_store._STATE_DIR
    datg_store._STATE_DIR = d
    try:
        return fn()
    finally:
        datg_store._STATE_DIR = old


def ledger_state(d):
    return in_ledger(d, lambda: datg.load_state("."))


def ledger_write(d, ops):
    return in_ledger(d, lambda: datg.write_ops(".", ops))


def il_writable(cfg, code):
    return cfg["ledgers"] == "all" or (cfg["ledgers"] == "own" and code == cfg["arch_code"])


def dash_by_il(store, il_id):
    for d in store.list_docs("items"):
        x = d["data"]
        if x.get("kind") == "task" and x.get("il") == il_id and x.get("status") == "open":
            return d
    return None

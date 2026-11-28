"""Replay driver for dashboard_ids.py (#12029 P2d): its functions and its command line, case by case on sandbox stores.

Why: dashboard_ids is a library for dashboard-db.py plus a small command line, and every entry reads a store root.
This file is copied beside the frozen dashboard_ids.py, brief_ids.py, ids36.py, dashboard_derive.py,
dashboard_store.py, dashboard_jsonl.py and dashboard_schema.py at record time as di_driver.py. It points the
modules' defaults at the sandbox root, the working directory: the default store is local/owner-queue, the brief
registry local/briefs/registry.jsonl, the media store local/owner-queue/media and the schemas schemas/. CASES.json is
a list of {"op": ...} objects; for each the driver prints `case <i> <op>` then the answer, indented, or the
exception: `<Class> <status> <code>: <message>` for a StoreError, `ValueError: <message>`, `<Class>: errno <n>`
for an OSError. `fill` writes one document per number in a range; `main` runs the module's `__main__` block with
the given argv and prints `exit <code>` after its output. `now` is an ISO string read with datetime.fromisoformat.
examples/replay_port.rs mirrors it as `replay_port dashboard_ids`.
Usage: driver.py CASES.json
"""
import json
import os
import runpy
import sys
from datetime import datetime, timedelta

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import brief_ids  # noqa: E402
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402
import ids36  # noqa: E402

CWD = os.getcwd()
ds.DEFAULT_ROOT = os.path.join(CWD, "local", "owner-queue")
dsch.SCHEMA_DIR = os.path.join(CWD, "schemas")
_REG = os.path.join(CWD, "local", "briefs", "registry.jsonl")
_MEDIA = os.path.join(CWD, "local", "owner-queue", "media")
brief_ids.used.__defaults__ = (_REG, _MEDIA)
brief_ids.allocate.__defaults__ = (_REG, _MEDIA)
import dashboard_ids as di  # noqa: E402

SOURCE = os.path.join(HERE, "dashboard_ids.py")


def dumps(v):
    return json.dumps(v, sort_keys=True, ensure_ascii=False)


def when(c):
    return datetime.fromisoformat(c["now"]) if "now" in c else None


def fill(c):
    """One `<root>/<coll>/<prefix><enc(n)>.json` per n in range(a, b): `data`, plus `stamp` = base + n seconds."""
    a, b = c["range"]
    d = os.path.join(c["root"], c.get("coll", "items"))
    os.makedirs(d, exist_ok=True)
    for n in range(a, b):
        body = dict(c.get("data", {}))
        if "stamp" in c:
            t = datetime.fromisoformat(c["base"].replace("Z", "+00:00")) + timedelta(seconds=c.get("step", 1) * n)
            body[c["stamp"]] = t.strftime("%Y-%m-%dT%H:%M:%SZ")
        body["version"] = 1
        with open(os.path.join(d, f"{c['prefix']}{ids36.enc(n)}.json"), "w", encoding="utf-8") as f:
            f.write(json.dumps(body))
    return [f"filled {b - a}"]


def answer(c):
    op = c["op"]
    root = c.get("root")
    if op == "write":
        os.makedirs(os.path.dirname(c["path"]) or ".", exist_ok=True)
        with open(c["path"], "w", encoding="utf-8") as f:
            f.write(c["text"])
        return ["ok"]
    if op == "fill":
        return fill(c)
    if op == "rm":
        os.remove(c["path"])
        return ["ok"]
    if op == "chmod":
        os.chmod(c["path"], int(c["mode"], 8))
        return ["ok"]
    if op == "env":
        for k, v in c.get("set", {}).items():
            os.environ[k] = v
        for k in c.get("unset", []):
            os.environ.pop(k, None)
        return ["ok"]
    if op == "ls":
        d = c["path"]
        return [" ".join(sorted(os.listdir(d)))] if os.path.isdir(d) else ["absent"]
    if op == "cat":
        try:
            with open(c["path"], encoding="utf-8") as f:
                return [repr(f.read())]
        except FileNotFoundError:
            return ["absent"]
    if op == "used":
        return [repr(sorted(di.used(c["prefix"], root)))]
    if op == "free_count":
        return [repr(di.free_count(c["prefix"], root))]
    if op == "next_id":
        return [repr(di.next_id(c["prefix"], root))]
    if op == "counters_path":
        return [di.counters_path(root)]
    if op == "read_counters":
        return [dumps(di.read_counters(root))]
    if op == "record_issued":
        di.record_issued(c["prefix"], c["id"], root)
        return ["ok"]
    if op == "brief":
        return [di.brief_next(), repr(di.brief_used())]
    if op == "retire_candidates":
        kw = {k: c[k] for k in ("keep_free", "age_days") if k in c}
        return [dumps(di.retire_candidates(c["prefix"], root, now=when(c), **kw))]
    if op == "retire_plan":
        kw = {k: c[k] for k in ("prefixes", "keep_free", "age_days") if k in c}
        return [f"{p} {dumps(rows)}" for p, rows in di.retire_plan(di._snap(root), when(c), **kw)]
    if op == "retire_apply":
        kw = {"prefixes": c["prefixes"]} if "prefixes" in c else {}
        return [dumps(r) for r in di.retire_apply(root, c.get("apply", False), when(c), **kw)]
    if op == "main":
        saved = sys.argv
        sys.argv = ["dashboard_ids.py"] + c["argv"]
        code = 0
        try:
            runpy.run_path(SOURCE, run_name="__main__")
        except SystemExit as e:
            code = e.code if isinstance(e.code, int) else 1
        finally:
            sys.argv = saved
            sys.stdout.flush()
        return [f"exit {code}"]
    raise KeyError(op)


def main(argv):
    if len(argv) != 1:
        sys.stderr.write("usage: driver.py CASES.json\n")
        return 2
    with open(argv[0], encoding="utf-8") as f:
        cases = json.load(f)
    for i, case in enumerate(cases):
        print(f"case {i} {case['op']}", flush=True)
        try:
            lines = answer(case)
        except ds.StoreError as e:
            lines = [f"{type(e).__name__} {e.status} {e.code}: {e}"]
        except OSError as e:
            lines = [f"{type(e).__name__}: errno {e.errno}"]
        except ValueError as e:
            lines = [f"ValueError: {e}"]
        for line in lines:
            print(f"  {line}", flush=True)
        sys.stderr.flush()
    return 0


sys.exit(main(sys.argv[1:]))

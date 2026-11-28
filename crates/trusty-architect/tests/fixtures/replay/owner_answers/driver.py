"""Replay driver for owner_answers.py and pending-answers.py (#12029 P2e), case by case on sandbox stores.

Why: owner_answers is a library (fleet-poll.py, pending-answers.py and dashboard-db.py call it with a root), and
pending-answers.py's store errors need a store the recorder cannot seed (a mode). This file is copied beside the
frozen owner_answers.py, pending-answers.py, dashboard_derive.py, dashboard_qual.py, dashboard_store.py,
dashboard_jsonl.py, dashboard_schema.py and ids36.py at record time as oa_driver.py. It points the default store
at local/owner-queue under the sandbox root, the working directory, and the schemas at schemas/. CASES.json is a
list of {"op": ...} objects; for each the driver prints `case <i> <op>` then the answer, indented, or the
exception: `<Class> <status> <code>: <message>` for a StoreError, `<Class>: errno <n>` for an OSError. `pending`
takes `now` as an ISO string (absent: the clock, which the recorder freezes); `main` runs pending-answers.py's
`__main__` block with the given argv and prints `exit <code>` after its output. examples/replay_port.rs mirrors it
as `replay_port owner_answers`.
Usage: driver.py CASES.json
"""
import json
import os
import runpy
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402

CWD = os.getcwd()
ds.DEFAULT_ROOT = os.path.join(CWD, "local", "owner-queue")
dsch.SCHEMA_DIR = os.path.join(CWD, "schemas")
import owner_answers as oa  # noqa: E402

SOURCE = os.path.join(HERE, "pending-answers.py")


def dumps(v):
    return json.dumps(v, ensure_ascii=False)


def answer(c):
    op = c["op"]
    root = c.get("root")
    if op == "write":
        os.makedirs(os.path.dirname(c["path"]) or ".", exist_ok=True)
        with open(c["path"], "w", encoding="utf-8") as f:
            f.write(c["text"])
        return ["ok"]
    if op == "mkdir":
        os.makedirs(c["path"], exist_ok=True)
        return ["ok"]
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
    if op == "pending":
        now = datetime.fromisoformat(c["now"]) if "now" in c else None
        return [dumps(e) for e in oa.pending(root, now)]
    if op == "resolve_id":
        return [dumps(oa.resolve_id(root, c["ref"]))]
    if op == "qualified":
        return [dumps(e) for e in oa.qualified(root, c["entries"])]
    if op == "main":
        saved = sys.argv
        sys.argv = ["pending-answers.py"] + c["argv"]
        code = 0
        try:
            runpy.run_path(SOURCE, run_name="__main__")
        except SystemExit as e:
            code = e.code if isinstance(e.code, int) else 1
        finally:
            sys.argv = saved
            sys.stdout.flush()
            sys.stderr.flush()
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
        for line in lines:
            print(f"  {line}", flush=True)
        sys.stderr.flush()
    return 0


sys.exit(main(sys.argv[1:]))

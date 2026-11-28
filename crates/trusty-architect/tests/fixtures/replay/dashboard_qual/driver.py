"""Replay driver for dashboard_qual.py (#12029 P2d): qualified ids and id aliases, case by case on sandbox stores.

Why: dashboard_qual has no command line; dashboard-db.py and owner_answers call it with a Store. This file is
copied beside the frozen dashboard_qual.py, dashboard_store.py, dashboard_jsonl.py and dashboard_schema.py at
record time as dq_driver.py. The default store root (the Architect's, S14) is pointed at local/owner-queue under
the sandbox root, the working directory. CASES.json is a list of {"op": ...} objects; for each the driver prints
`case <i> <op>` then the answer, indented, or the exception: `<Class> <status> <code>: <message>` for a
StoreError, `<Class>: errno <n>` for an OSError. examples/replay_port.rs mirrors it as `replay_port dashboard_qual`.
Usage: driver.py CASES.json
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402

ds.DEFAULT_ROOT = os.path.join(os.getcwd(), "local", "owner-queue")
dsch.SCHEMA_DIR = os.path.join(os.getcwd(), "schemas")
import dashboard_qual as dq  # noqa: E402

STORES = {}


def dumps(v):
    return json.dumps(v, sort_keys=True, ensure_ascii=False)


def answer(c):
    op = c["op"]
    if op == "write":
        os.makedirs(os.path.dirname(c["path"]) or ".", exist_ok=True)
        with open(c["path"], "w", encoding="utf-8") as f:
            f.write(c["text"])
        return ["ok"]
    if op == "mkdir":
        os.makedirs(c["path"], exist_ok=True)
        return ["ok"]
    if op == "symlink":
        os.symlink(c["target"], c["path"])
        return ["ok"]
    if op == "new":
        STORES[c.get("name", "s")] = ds.Store(c["root"], fmt=c.get("fmt"))
        return ["ok"]
    if op == "split":
        return [dumps(list(dq.split(c["ref"])))]
    if op == "qualify":
        return [dumps(dq.qualify(c.get("sid"), c["id"]))]
    s = STORES[c.get("store", "s")]
    if op == "store_sid":
        return [dumps(dq.store_sid(s))]
    if op == "alias_rules":
        return [dumps([list(r) for r in dq.alias_rules(s)])]
    if op == "alias_target":
        return [dumps(dq.alias_target(s, c["coll"], c["local"]))]
    if op == "resolve":
        return [dumps(dq.resolve(s, c["coll"], c["ref"]))]
    if op == "with_qualifier":
        docs = s.get(c["coll"], c["id"]) if "id" in c else s.list_docs(c["coll"])
        return [dumps(dq.with_qualifier(s, c["coll"], docs))]
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
    return 0


sys.exit(main(sys.argv[1:]))

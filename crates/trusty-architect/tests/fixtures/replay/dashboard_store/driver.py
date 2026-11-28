"""Replay driver for dashboard_store.py (#12029 P2c): its Store, run case by case on sandbox stores.

Why: dashboard_store has no command line; its callers construct a Store and call get, list_docs, collections,
batch, set, update, delete, add and seed_meta. This file is copied beside the frozen dashboard_store.py,
dashboard_jsonl.py and dashboard_schema.py at record time as dst_driver.py. CASES.json is a list of {"op": ...}
objects (`add` runs on a dir store only: its random id's file is removed after the read-back); for each the driver prints `case <i> <op>` then the answer, indented, or the exception the call raised:
`<Class> <status> <code>: <message>` for a StoreError, `SchemaError: <message>`, `<Class>: errno <n>` for an
OSError (its message can hold a random temp-file name). Every path is relative to the sandbox root, the working
directory, and the schemas are read from `schemas/` under it, so nothing outside it is read or written.
examples/replay_port.rs mirrors it as `replay_port dashboard_store`.
Usage: driver.py CASES.json
"""
import fcntl
import json
import os
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_jsonl as dj  # noqa: E402
import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402

dsch.SCHEMA_DIR = os.path.join(os.getcwd(), "schemas")
STORES = {}


def dumps(v):
    return json.dumps(v, sort_keys=True, ensure_ascii=False)


def doc_line(d):
    return f"{d['id']!r} {d['version']} {dumps(d['data'])}"


def opt(case, key):
    return case.get(key)


def answer(case):
    op = case["op"]
    if op == "env":
        for k, v in case.get("set", {}).items():
            os.environ[k] = v
        for k in case.get("unset", []):
            os.environ.pop(k, None)
        return ["ok"]
    if op == "new":
        s = ds.Store(case["root"], fmt=opt(case, "fmt"), actor=opt(case, "actor"),
                     schema_mode=opt(case, "schema_mode"), prune_log=opt(case, "prune_log"))
        STORES[case.get("name", "s")] = s
        return [f"fmt={s.fmt} actor={s.actor} schema_mode={s.schema_mode}", f"prune_log={s.prune_log_path()}"]
    if op == "default_format":
        return [ds.default_format()]
    if op == "normalize":
        return [dumps(ds.normalize_timestamps(case["value"]))]
    if op == "write":
        os.makedirs(os.path.dirname(case["path"]) or ".", exist_ok=True)
        with open(case["path"], "wb") as f:
            f.write(bytes.fromhex(case["hex"]) if "hex" in case else case["text"].encode("utf-8"))
        return ["ok"]
    if op == "rm":
        os.remove(case["path"])
        return ["ok"]
    if op == "mkdir":
        os.makedirs(case["path"], exist_ok=True)
        return ["ok"]
    if op == "chmod":
        os.chmod(case["path"], int(case["mode"], 8))
        return ["ok"]
    if op == "cat":
        try:
            with open(case["path"], "rb") as f:
                return [repr(f.read().decode("utf-8"))]
        except FileNotFoundError:
            return ["absent"]
    if op == "changes":
        return [dumps(e) for e in dj.read_changes(case["root"])] or ["empty"]
    s = STORES[case.get("store", "s")]
    if op == "hold":
        # Another open file description holds the store lock; it is released after `seconds`.
        lf = open(os.path.join(s.root, ".lock"), "a")
        fcntl.flock(lf, fcntl.LOCK_EX)
        timer = threading.Timer(case["seconds"], lambda: (fcntl.flock(lf, fcntl.LOCK_UN), lf.close()))
        start = time.monotonic()
        timer.start()
        try:
            lines = answer(case["then"])
            waited = time.monotonic() - start >= case["seconds"] * 0.9
        finally:
            timer.join()
        return [f"waited {waited}"] + lines
    if op == "get":
        return [doc_line(s.get(case["coll"], case["id"]))]
    if op == "list":
        docs = s.list_docs(case["coll"], status=opt(case, "status"), order_by=opt(case, "order_by"),
                           desc=case.get("desc", False), limit=opt(case, "limit"))
        return [doc_line(d) for d in docs] or ["empty"]
    if op == "collections":
        return [repr(s.collections())]
    if op == "set":
        return [dumps(s.set(case["coll"], case["id"], case.get("data"), opt(case, "if_version"), opt(case, "actor")))]
    if op == "update":
        return [dumps(s.update(case["coll"], case["id"], case.get("data"), opt(case, "if_version"), opt(case, "actor")))]
    if op == "delete":
        return [dumps(s.delete(case["coll"], case["id"], opt(case, "if_version"), opt(case, "actor")))]
    if op == "batch":
        return [dumps(r) for r in s.batch(case["ops"], opt(case, "actor"))]
    if op == "add":
        r = s.add(case["coll"], case.get("data"), opt(case, "actor"))
        shape = len(r["id"]) == 20 and all(c in "0123456789abcdef" for c in r["id"])
        got = s.get(case["coll"], r["id"])
        os.remove(os.path.join(s.root, case["coll"], r["id"] + ".json"))  # the id is random: keep it out of the writes
        return [f"{r['collection']} v{r['version']} id20hex={shape} {dumps(got['data'])}"]
    if op == "seed_meta":
        return [repr(s.seed_meta(owner_name=case["owner_name"]))]
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
        except dsch.SchemaError as e:
            lines = [f"SchemaError: {e}"]
        except OSError as e:
            lines = [f"{type(e).__name__}: errno {e.errno}"]
        except dj.JsonlError as e:
            lines = [f"JsonlError: {e}"]
        for line in lines:
            print(f"  {line}", flush=True)
        sys.stderr.flush()
    return 0


sys.exit(main(sys.argv[1:]))

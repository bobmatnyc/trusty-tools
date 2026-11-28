"""Replay driver for dashboard_attach.py (#12029 P2h): owner uploads, case by case on sandbox stores.

Why: dashboard_attach has no command line; the dashboard server calls it with a Store, the request body and the
schedule. This file is copied beside the frozen dashboard_attach.py, dashboard_il.py, dashboard_store.py,
dashboard_jsonl.py and dashboard_schema.py at record time as da_driver.py; the schemas are read from
schemas/dashboard under the sandbox root, the working directory. CASES.json is a list of {"op": ...} objects; an
upload body is read from the file the case names (`body`), its length the file's size unless `length` is given.
For each the driver prints `case <i> <op>` then the answer, indented, or the exception:
`<Class> <status> <code>: <message>` for a StoreError, `<Class>: errno <n>` for an OSError, `<Class>: <message>`
for any other. examples/replay_port.rs mirrors it as `replay_port dashboard_attach`.
Usage: driver.py CASES.json
"""
import hashlib
import io
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_schema as dsch  # noqa: E402

dsch.SCHEMA_DIR = os.path.join(os.getcwd(), "schemas", "dashboard")
import dashboard_attach as da  # noqa: E402
import dashboard_il as dil  # noqa: E402
import dashboard_store as ds  # noqa: E402

STORES = {}


def dumps(v):
    return json.dumps(v, sort_keys=True, ensure_ascii=False)


def body(c):
    if "body" not in c:
        return io.BytesIO(b""), c.get("length", 0)
    f = open(c["body"], "rb")
    return f, c.get("length", os.path.getsize(c["body"]))


def answer(c):
    op = c["op"]
    if op == "write":
        os.makedirs(os.path.dirname(c["path"]) or ".", exist_ok=True)
        with open(c["path"], "wb") as f:
            f.write(bytes.fromhex(c["hex"]) if "hex" in c else c["text"].encode("utf-8"))
        return ["ok"]
    if op == "rm":
        os.remove(c["path"])
        return ["ok"]
    if op == "mkdir":
        os.makedirs(c["path"], exist_ok=True)
        return ["ok"]
    if op == "chmod":
        os.chmod(c["path"], int(c["mode"], 8))
        return ["ok"]
    if op == "cat":
        try:
            with open(c["path"], "rb") as f:
                return [repr(f.read().decode("utf-8"))]
        except FileNotFoundError:
            return ["absent"]
    if op == "symlink":
        os.symlink(c["target"], c["path"])
        return ["ok"]
    if op == "ls":
        try:
            return [dumps(sorted(os.listdir(c["path"])))]
        except FileNotFoundError:
            return ["absent"]
    if op == "env":
        for k, v in c.get("set", {}).items():
            os.environ[k] = v
        for k in c.get("unset", []):
            os.environ.pop(k, None)
        return ["ok"]
    if op == "new":
        STORES[c.get("name", "s")] = ds.Store(c["root"], fmt=c.get("fmt"))
        return ["ok"]
    if op == "clean_name":
        return [dumps(da.clean_name(c.get("raw")))]
    if op == "content_type":
        return [dumps(da.content_type(c["name"]))]
    if op == "max_bytes":
        return [str(da.max_bytes())]
    if op == "item_id":
        return [dumps(bool(da.ITEM_ID.match(c["id"])))]
    if op == "session_re":
        return [dumps(bool(dil.SESSION_RE.match(c["s"])))]
    if op == "read_schedule":
        return [dumps(dil._read_schedule(c["path"]))]
    if op == "attach_dir":
        return [dumps(list(da.attach_dir(c.get("session"), c["schedule"], c["architect_il"], c.get("store_root"))))]
    if op == "safe_dir":
        return [dumps(da._safe_dir(c["base"], c["item_id"]))]
    if op == "save":
        f, length = body(c)
        with f:
            return [dumps(list(da.save(f, length, c["name"], c["directory"], c["item_id"],
                                       c.get("limit", da.max_bytes()))))]
    s = STORES[c.get("store", "s")]
    if op == "set":
        s.set(c["coll"], c["id"], c.get("data"))
        return ["ok"]
    if op == "get":
        d = s.get("items", c["id"])
        return [f"{d['version']} {dumps(d['data'])}"]
    if op == "record":
        return [dumps(da.record(s, c["item_id"], c.get("entry"), c.get("actor")))]
    if op == "upload":
        f, length = body(c)
        with f:
            return [dumps(da.handle_upload(s, c["item_id"], c.get("name"), f, length, c["schedule"],
                                           c["architect_il"], c.get("store_root"), c.get("actor")))]
    if op == "serve":
        data, ctype, inline = da.serve_file(s, c["item_id"], c["stored_name"])
        return [f"{len(data)} {hashlib.sha256(data).hexdigest()} {ctype} {inline}"]
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
        except Exception as e:  # noqa: BLE001
            lines = [f"{type(e).__name__}: {e}"]
        for line in lines:
            print(f"  {line}", flush=True)
    return 0


sys.exit(main(sys.argv[1:]))

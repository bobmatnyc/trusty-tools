"""Replay driver for dashboard_updates.py (#12029 P2h): its command line, and its functions case by case.

Why: dashboard_updates.py reads the checkout beside the script (local/owner-queue, inbox, schemas/dashboard), which
is outside the sandbox the recorder allows. This file is copied beside the frozen dashboard_updates.py,
dashboard_store.py, dashboard_jsonl.py and dashboard_schema.py at record time as du_driver.py. It points each of
those defaults at the working directory (the sandbox root, which stands for the checkout) and changes none of the
modules' text. `cli ARGS...` runs the script's main(); `cases CASES.json` runs each {"op": ...} and prints
`case <i> <op>` then the answer, indented, or the exception: `<Class> <status> <code>: <message>` for a
StoreError, `<Class>: errno <n>` for an OSError, `<Class>: <message>` for any other.
examples/replay_port.rs mirrors it as `replay_port dashboard_updates`.
Usage: driver.py cli ARGS... | cases CASES.json
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
CWD = os.getcwd()
import dashboard_schema as dsch  # noqa: E402

dsch.SCHEMA_DIR = os.path.join(CWD, "schemas", "dashboard")
import dashboard_store as ds  # noqa: E402

ds.DEFAULT_ROOT = os.path.join(CWD, "local", "owner-queue")
import dashboard_updates as du  # noqa: E402

du.HERE = os.path.join(CWD, "scripts")
STORES = {}


def dumps(v):
    return json.dumps(v, sort_keys=True, ensure_ascii=False)


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
    if op == "new":
        STORES[c.get("name", "s")] = ds.Store(c["root"], fmt=c.get("fmt"))
        return ["ok"]
    if op == "one_paragraph":
        return [dumps(du.one_paragraph(c["text"], c.get("limit", du.MAX_CHARS)))]
    if op in ("tag", "bare"):
        m = (du.TAG if op == "tag" else du.BARE).search(c["text"])
        return [dumps(m.group(1) if m else None)]
    if op == "handback":
        return [dumps(du.parse_handback(c["text"]))]
    if op == "relay":
        return [dumps(du.relay_entry(c["row"]))]
    if op == "message":
        return [dumps(du.message_entry(c["row"]))]
    if op == "note":
        return [dumps(du.note_entry(c["row"]))]
    if op == "candidates":
        return [dumps(e) for e in du.candidates(c.get("inbox", "inbox"))] or ["empty"]
    if op == "add_note":
        return [dumps(du.add_note(c["task"], c["text"], c.get("by", "architect"), inbox=c.get("inbox", "inbox")))]
    if op == "ingest":
        return [dumps(du.ingest(c.get("root", "local/owner-queue"), c.get("inbox", "inbox"),
                                since_hours=c.get("since_hours", du.SINCE_HOURS)))]
    s = STORES[c.get("store", "s")]
    if op == "set":
        s.set(c["coll"], c["id"], c.get("data"))
        return ["ok"]
    if op == "get":
        d = s.get("items", c["id"])
        return [f"{d['version']} {dumps(d['data'])}"]
    docs = s.list_docs("items")
    if op == "items":
        return [dumps(e) for e in du.item_entries(docs)] or ["empty"]
    if op == "pick":
        return [dumps({t: d["id"] for t, d in du.pick_task_items(docs).items()})]
    if op == "stale":
        return [dumps(du.stale_notes(docs))]
    if op == "annotate":
        return [dumps(d) for d in du.annotate_stale(docs)]
    raise KeyError(op)


def cases(path):
    with open(path, encoding="utf-8") as f:
        todo = json.load(f)
    for i, case in enumerate(todo):
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


def main(argv):
    if argv and argv[0] == "cli":
        return du.main(argv[1:])
    if len(argv) == 2 and argv[0] == "cases":
        return cases(argv[1])
    sys.stderr.write("usage: driver.py cli ARGS... | cases CASES.json\n")
    return 2


sys.exit(main(sys.argv[1:]))

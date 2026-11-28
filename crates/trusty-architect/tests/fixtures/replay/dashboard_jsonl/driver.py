"""Replay driver for dashboard_jsonl.py (#12029 P2b): its command line and its library functions.

Why: the module's command line is `fsck [ROOT]`; its callers use the encoding, the parser, the collection reader,
the change log and fsck. This file is copied beside the frozen module at record time as dj_driver.py. `cli ARGS...`
runs the module's `__main__` block unchanged with those arguments. `CASES.json` is a list of {"op": ...} objects;
for each the driver prints `case <i> <op>` then the answer, indented, or `JsonlError: <message>` when the call
raises it. Every root is relative to the sandbox root, the working directory, so nothing outside it is read or
written. examples/replay_port.rs mirrors it as `replay_port dashboard_jsonl`.
Usage: driver.py cli ARGS... | driver.py CASES.json
"""
import json
import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_jsonl as dj  # noqa: E402


def dumps(v):
    return json.dumps(v, ensure_ascii=False)


def docs_lines(docs):
    return [f"{doc_id!r} {version} {dumps(data)}" for doc_id, (version, data) in docs.items()]


def answer(case):
    op = case["op"]
    if op == "canonical":
        return [dj.canonical(case["value"])]
    if op == "render_line":
        return [repr(dj.render_line(case["id"], case["version"], case["data"], case.get("sv", dj.SV)).decode("utf-8"))]
    if op == "render_file":
        docs = {k: (v[0], v[1]) for k, v in case["docs"].items()}
        return [repr(dj.render_file(docs).decode("utf-8"))]
    if op == "sort_ids":
        return [repr(dj.sort_ids(case["ids"]))]
    if op == "sha256":
        return [dj.sha256(case["text"].encode("utf-8"))]
    if op == "parse":
        blob = bytes.fromhex(case["hex"]) if "hex" in case else case["text"].encode("utf-8")
        return docs_lines(dj.parse_file(blob, case.get("path", "<jsonl>"))) or ["empty"]
    if op == "read_coll":
        return docs_lines(dj.read_coll(case["root"], case["coll"])) or ["empty"]
    if op == "collections":
        return [repr(dj.collections(case["root"]))]
    if op == "append":
        return [f"appended {dj.append_changes(case['root'], case['entries'], case['ts'])}"]
    if op == "changed_keys":
        return [repr(dj.changed_keys(case.get("old"), case.get("new")))]
    if op == "read_changes":
        return [dumps(e) for e in dj.read_changes(case["root"])] or ["empty"]
    if op == "fsck":
        return dj.fsck(case["root"]) or ["ok"]
    raise KeyError(op)


def main(argv):
    if argv[:1] == ["cli"]:
        sys.argv = ["dashboard_jsonl.py"] + argv[1:]
        runpy.run_path(os.path.join(HERE, "dashboard_jsonl.py"), run_name="__main__")
        return 0
    if len(argv) != 1:
        sys.stderr.write("usage: driver.py cli ARGS... | driver.py CASES.json\n")
        return 2
    with open(argv[0], encoding="utf-8") as f:
        cases = json.load(f)
    for i, case in enumerate(cases):
        print(f"case {i} {case['op']}", flush=True)
        try:
            lines = answer(case)
        except dj.JsonlError as e:
            lines = [f"JsonlError: {e}"]
        for line in lines:
            print(f"  {line}", flush=True)
    return 0


sys.exit(main(sys.argv[1:]))

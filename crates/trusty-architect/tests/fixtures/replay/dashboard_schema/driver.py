"""Replay driver for dashboard_schema.py (#12029 P2a): runs a list of cases against the library.

Why: the module has no command line; its callers use schema_name, validate, validate_store, mode
and architect_do_missing. This file is copied beside the frozen module at record time as
ds_driver.py. It reads CASES.json, a list of {"op": ...} objects, and prints `case <i> <op>` then
the answer of each, indented: `ok` or one line per error for `validate` and `store`, the value for
`name`, `mode` and `missing`, and `SchemaError: <message>` when the call raises it. Every schema
directory is the case's `dir` (default `schemas`), relative to the sandbox root, so no default path
beside the module is read. examples/replay_port.rs mirrors it as `replay_port dashboard_schema`.
Usage: driver.py CASES.json
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dashboard_schema as dsch  # noqa: E402


def answer(case):
    op, d = case["op"], case.get("dir", "schemas")
    if op == "name":
        return [str(dsch.schema_name(case["coll"], case.get("id")))]
    if op == "validate":
        return dsch.validate(case["coll"], case["id"], case["data"], d) or ["ok"]
    if op == "store":
        return dsch.validate_store(case["docs"], d) or ["ok"]
    if op == "mode":
        return [str(dsch.mode(case["env"]))]
    if op == "missing":
        return [str(dsch.architect_do_missing(case["data"]))]
    raise KeyError(op)


def main(argv):
    if len(argv) != 1:
        sys.stderr.write("usage: driver.py CASES.json\n")
        return 2
    with open(argv[0], encoding="utf-8") as f:
        cases = json.load(f)
    for i, case in enumerate(cases):
        print(f"case {i} {case['op']}")
        try:
            lines = answer(case)
        except dsch.SchemaError as e:
            lines = [f"SchemaError: {e}"]
        for line in lines:
            print(f"  {line}")
    return 0


sys.exit(main(sys.argv[1:]))

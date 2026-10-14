#!/usr/bin/env python3
"""dashboard-db.py: CLI over the dashboard's file store (stdlib only); the Architect's replacement for ArtifactData.

Why: after the move to the localhost dashboard (the owner, 2026-10-08 17:34Z) the Architect
reads and writes dashboard documents with this command, with or without the server running.
What: get / list / set / update / delete / batch on <root>/<collection>/<id>.json through
dashboard_store (same flock, version check and atomic writes as dashboard-serve.py).
Output is JSON on stdout: get -> {id, version, data}; list -> [{id, version, data}];
writes -> {collection, id, version}; batch -> [results]. Errors go to stderr.
Exit codes: 0 ok, 1 not found, 2 invalid request or usage, 3 version conflict.
--json takes a file path, "-" for stdin, or a literal JSON string (starts with { or [).
batch FILE holds a list of ops, or {"ops": [...]}: {op: set|update|delete, collection,
id, data, if_version}. Root: --root or DASHBOARD_ROOT, default local/owner-queue.
Writes to collection "items" validate `steps`: each is an object with a non-empty string
`title` (fields: id, title, detail, links, values); a step with `text` and no `title` is refused.
Test: scripts/test_dashboard_store.py
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402


def load_json(arg):
    text = arg.strip()
    if arg == "-":
        text = sys.stdin.read()
    elif not text.startswith(("{", "[")):
        try:
            with open(arg, "r", encoding="utf-8") as f:
                text = f.read()
        except OSError as e:
            raise ds.StoreError(f"cannot read {arg}: {e}")
    try:
        return json.loads(text)
    except ValueError as e:
        raise ds.StoreError(f"invalid JSON: {e}")


STEP_FIELDS = "id, title, detail, links, values"


def check_steps(item_id, data):
    """Refuse a malformed `steps` list (the page renders step.title, so a missing one shows blank)."""
    if not isinstance(data, dict) or "steps" not in data:
        return
    steps = data["steps"]
    if not isinstance(steps, list):
        raise ds.StoreError(f"item {item_id}: steps must be a list of objects with fields: {STEP_FIELDS}")
    for i, st in enumerate(steps):
        where = f"item {item_id}: step {i}"
        if not isinstance(st, dict):
            raise ds.StoreError(f"{where} must be an object with fields: {STEP_FIELDS}")
        title = st.get("title")
        if not isinstance(title, str) or not title.strip():
            hint = " ('text' is not rendered; rename it to 'title')" if "text" in st else ""
            raise ds.StoreError(f"{where} needs a non-empty string 'title'{hint}; expected fields: {STEP_FIELDS}")


def check_ops(ops):
    for op in ops if isinstance(ops, list) else []:
        if isinstance(op, dict) and op.get("collection") == "items" and op.get("op") in ("set", "update"):
            check_steps(op.get("id"), op.get("data"))


def emit(obj):
    json.dump(obj, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")


def build_parser():
    ap = argparse.ArgumentParser(description="Read and write Architect dashboard documents.")
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT", ds.DEFAULT_ROOT))
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("get"); g.add_argument("collection"); g.add_argument("id")
    ls = sub.add_parser("list"); ls.add_argument("collection"); ls.add_argument("--status")
    for name in ("set", "update"):
        w = sub.add_parser(name); w.add_argument("collection"); w.add_argument("id")
        w.add_argument("--json", required=True, metavar="FILE_OR_STRING")
        w.add_argument("--if-version", type=int)
    d = sub.add_parser("delete"); d.add_argument("collection"); d.add_argument("id"); d.add_argument("--if-version", type=int)
    b = sub.add_parser("batch"); b.add_argument("file", help="JSON file (or - for stdin): list of ops or {ops: [...]}")
    return ap


def run(args):
    store = ds.Store(args.root)
    if args.cmd == "get":
        return store.get(args.collection, args.id)
    if args.cmd == "list":
        return store.list_docs(args.collection, status=args.status)
    if args.cmd in ("set", "update"):
        fn = store.set if args.cmd == "set" else store.update
        data = load_json(args.json)
        if args.collection == "items":
            check_steps(args.id, data)
        return fn(args.collection, args.id, data, args.if_version)
    if args.cmd == "delete":
        return store.delete(args.collection, args.id, args.if_version)
    ops = load_json(args.file)
    ops = ops["ops"] if isinstance(ops, dict) and "ops" in ops else ops
    check_ops(ops)
    return store.batch(ops)


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        emit(run(args))
    except ds.StoreError as e:
        sys.stderr.write(f"dashboard-db: {e.code}: {e}\n")
        return {404: 1, 409: 3}.get(e.status, 2)
    return 0


if __name__ == "__main__":
    sys.exit(main())

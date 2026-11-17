#!/usr/bin/env python3
"""brief-id.py: durable brief ids (MB + 4 hex digits, never reused; legacy B ids still read). Library: scripts/brief_ids.py.

Usage: brief-id.py next | assign <path> [--project P] | find <id> | list | check
Test: scripts/test_brief_id.py
"""
import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import brief_ids as b  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("next")
    a = sub.add_parser("assign")
    a.add_argument("path")
    a.add_argument("--project", default="architect")
    f = sub.add_parser("find")
    f.add_argument("id")
    sub.add_parser("list")
    sub.add_parser("check")
    args = ap.parse_args(argv)
    if args.cmd == "next":
        print(b.allocate())
    elif args.cmd == "assign":
        bid, created = b.assign(args.path, args.project)
        print(f"{bid}\t{'assigned' if created else 'kept'}\t{args.path}")
    elif args.cmd == "find":
        want = args.id.lower()
        rows = [r for r in b.read_registry() if r["id"].lower() == want or (b.BRIEF_ID_RE.match(args.id) and b.BRIEF_ID_RE.match(r["id"]) and b.number(r["id"]) == b.number(want))]
        if not rows:
            print(f"not found: {args.id}", file=sys.stderr)
            return 1
        print(os.path.join(b.REPO, rows[0]["path"]) if not os.path.isabs(rows[0]["path"]) else rows[0]["path"])
    elif args.cmd == "list":
        for r in b.read_registry():
            print(f"{r['id']}\t{r['project']}\t{r['path']}\t{r['title']}")
    else:
        probs = b.check()
        for p in probs:
            print(p)
        print(f"{len(probs)} problem(s)")
        return 1 if probs else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())

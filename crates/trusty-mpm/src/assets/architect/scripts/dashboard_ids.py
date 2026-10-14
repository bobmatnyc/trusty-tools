#!/usr/bin/env python3
"""dashboard_ids.py: dashboard ids recycle (the owner 2026-10-09).

Why: ids are a prefix letter plus 2 hex digits (00-ff) per prefix, so 256 per prefix. The next id must
come from the store itself, wrap past ff, and the Architect must retire done items before ids run out.
What: next_id(prefix) is the first free id after the highest used, wrapping past ff to the lowest free (None
when full); free_count(prefix) is 256 minus the used ids; retire_candidates(prefix, keep_free=64) lists the
oldest status "done" items (by done_at) to retire so at least keep_free ids stay free. Used ids come from the
items, goals, priorities and sessions collections. This module never deletes: retirement (archive to memory,
then delete) stays with the Architect. The dashboard page computes the same values in index.html (nextIds).
Test: scripts/test_dashboard_ids.py
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_store as ds  # noqa: E402

COLLECTIONS = ("items", "goals", "priorities", "sessions")
ID_RE = re.compile(r"^([A-Z])([0-9a-f]{2})$")
SPACE = 256


def _docs(root, prefix):
    for coll in COLLECTIONS:
        d = os.path.join(root or ds.DEFAULT_ROOT, coll)
        try:
            names = sorted(os.listdir(d))
        except OSError:
            continue
        for n in names:
            m = ID_RE.match(n[:-5]) if n.endswith(".json") else None
            if not m or m.group(1) != prefix:
                continue
            try:
                with open(os.path.join(d, n), encoding="utf-8") as f:
                    raw = json.load(f)
            except (OSError, ValueError):
                raw = {}
            data = raw.get("data", raw) if isinstance(raw, dict) else {}
            yield int(m.group(2), 16), data if isinstance(data, dict) else {}


def used(prefix, root=None):
    return {n for n, _ in _docs(root, prefix)}


def free_count(prefix, root=None):
    return SPACE - len(used(prefix, root))


def next_id(prefix, root=None):
    u = used(prefix, root)
    top = max(u) if u else -1
    for k in range(1, SPACE + 1):
        c = (top + k) % SPACE
        if c not in u:
            return f"{prefix}{c:02x}"
    return None


def retire_candidates(prefix, root=None, keep_free=64):
    need = keep_free - free_count(prefix, root)
    if need <= 0:
        return []
    done = [(str(d.get("done_at") or d.get("updated") or d.get("created") or ""), n)
            for n, d in _docs(root, prefix) if d.get("status") == "done"]
    return [f"{prefix}{n:02x}" for _, n in sorted(done)[:need]]


if __name__ == "__main__":
    for p in "GPADTS":
        print(f"{p}\tnext={next_id(p)}\tfree={free_count(p)}\tretire={','.join(retire_candidates(p))}")

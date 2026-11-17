#!/usr/bin/env python3
"""dashboard_ids.py: dashboard ids recycle (the owner 2026-10-09).

Why: ids are a prefix letter plus 2 hex digits (00-ff) per prefix, so 256 per prefix. The next id must
come from the store itself, wrap past ff, and the Architect must retire done items before ids run out.
What: next_id(prefix) is the first free id after the highest used, wrapping past ff to the lowest free (None
when full); free_count(prefix) is 256 minus the used ids; retire_candidates(prefix, keep_free=64) lists the
oldest status "done" items (by done_at) to retire so at least keep_free ids stay free. Used ids come from the
items, goals, priorities, sessions, questions and media (prefix B, dashboard_media.sync_ids) collections.
Prefix B is the exception (the owner 2026-10-28): brief ids are B + 4 hex digits, never reused, so brief_next()/brief_used() read
local/briefs/registry.jsonl (scripts/brief_ids.py) and the CLI prints B from there, not from the 2-digit rules. The rules are in dashboard_derive; this module is the root-based wrapper. This module never deletes: retirement (archive to memory,
then delete) stays with the Architect.
Test: tests/test_dashboard_ids.py
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import brief_ids  # noqa: E402
import dashboard_derive as derive  # noqa: E402
import dashboard_store as ds  # noqa: E402

COLLECTIONS = derive.ID_COLLECTIONS
SPACE = derive.SPACE


def _snap(root):
    return derive.snapshot(ds.Store(root or ds.DEFAULT_ROOT), COLLECTIONS)


def used(prefix, root=None):
    return derive.used_ids(_snap(root), prefix)


def free_count(prefix, root=None):
    return derive.free_count(_snap(root), prefix)


def next_id(prefix, root=None):
    return derive.next_id(_snap(root), prefix)


def brief_used():
    """Count of brief ids taken (registry plus media-store ids; they share the B space)."""
    return len(brief_ids.used())


def brief_next():
    """Next brief id (B + 4 hex digits); ids never wrap or recycle."""
    return brief_ids.allocate()


def retire_candidates(prefix, root=None, keep_free=64):
    return derive.retire_candidates(_snap(root), prefix, keep_free)


if __name__ == "__main__":
    snap = _snap(None)
    for p in "GPADTSQ":
        print(f"{p}\tnext={derive.next_id(snap, p)}\tfree={derive.free_count(snap, p)}\tretire={','.join(derive.retire_candidates(snap, p))}")
    print(f"B\tnext={brief_next()}\tused={brief_used()}\tnever recycled (4 hex digits)")

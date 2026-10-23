#!/usr/bin/env python3
"""stale-tasks.py: list open, unparked dashboard tasks (kind task) with no recent activity.

Why: the owner (2026-10-11) wants tasks shown as active to be real; idle ones get parked by the Architect.
What: read-only. A task is stale when status is "open", parked is empty, and the latest of its
`updates[].at`, `progress.estimated_at` and `updated` (created if none) is older than --hours (default 24).
Prints one tab-separated line per task, oldest first: id, session, last activity (ISO), title.
Exit 0 always on a readable store, 2 on a store error. Root: --root or DASHBOARD_ROOT, default local/owner-queue.
Test: scripts/test_stale_tasks.py
"""
import argparse
import os
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import dashboard_derive as derive  # noqa: E402
import dashboard_store as ds  # noqa: E402


last_activity = derive.last_activity


def stale(root=None, hours=24.0, now=None):
    """Return [(last_activity, id, session, title)] oldest first; last_activity None sorts first."""
    now = now or datetime.now(timezone.utc)
    root = root or ds.DEFAULT_ROOT
    if not os.path.isdir(root):
        raise FileNotFoundError(f"no dashboard store at {root}")
    return derive.stale_tasks(ds.Store(root).list_docs("items"), now, hours)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT"))
    ap.add_argument("--hours", type=float, default=24.0)
    args = ap.parse_args(argv)
    try:
        rows = stale(args.root, args.hours)
    except (OSError, ds.StoreError) as e:
        print(f"stale-tasks: cannot read store: {e}", file=sys.stderr)
        return 2
    for last, item_id, session, title in rows:
        print(f"{item_id}\t{session}\t{last.strftime('%Y-%m-%dT%H:%M:%SZ') if last else 'never'}\t{title}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

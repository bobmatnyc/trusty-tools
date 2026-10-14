#!/usr/bin/env python3
"""pending-answers.py: list dashboard items the owner answered, cancelled or marked Won't do (kind "wont_do",
text = the reason) that are not yet relayed or acknowledged,
carry new feedback (kind "feedback": reclassify and/or note), or sit on a session with a new
owner command (kind "owner_command": Activate, Prioritize, Park or Restart from the session row),
an open task with a Status Update request (kind "status_request", text = title),
or a new goal effort tune (kind "effort": More/Less Effort, docs in effort_tunes).

Why: the Architect runs it before each reply so an owner answer is never missed.
What: prints id, time and answer per item; exit 1 if any exist, 0 if none, 2 on a store error.
Root: --root or DASHBOARD_ROOT, default local/owner-queue.
Test: scripts/test_fleet_poll_owner_answer.py
"""
import argparse
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import owner_answers  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT"))
    args = ap.parse_args(argv)
    try:
        items = owner_answers.pending(args.root)
    except OSError as e:
        print(f"pending-answers: cannot read store: {e}", file=sys.stderr)
        return 2
    for it in items:
        print(f"{it['id']}\t{it['kind']}\t{it['at']}\t{it['text']}")
    return 1 if items else 0


if __name__ == "__main__":
    sys.exit(main())

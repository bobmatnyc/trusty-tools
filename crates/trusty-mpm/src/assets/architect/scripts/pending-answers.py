#!/usr/bin/env python3
"""pending-answers.py: list dashboard items the owner answered, cancelled or marked Won't do (kind "wont_do",
text = the reason) or closed with Respond interactively (kind "interactive", text = title, printed "INTERACTIVE: <id> <title>") that are not yet relayed or acknowledged,
carry new feedback (kind "feedback": reclassify and/or note), or sit on a session with a new
owner command (kind "owner_command": Activate, Prioritize, Park or Restart from the session row),
an open task with a Status Update request (kind "status_request", text = title),
a task steer entry without relayed_at (kind "steer", text = the instruction),
an open item with a Resume request (kind "resume_request", text = title: the Resume button on a parked item),
an open item with a brief request and no brief (kind "brief_request", text = title: Ask for a brief on a decision or ask),
a new goal effort tune (kind "effort": More/Less Effort, docs in effort_tunes),
a goal park or unpark awaiting the Architect (kind "goal_park": text "parked until <date>" or "until unparked: <title>";
kind "goal_unpark": text = title; cleared by the Architect setting park_ack on the goal doc),
a goal marked complete on the page (kind "goal_done": text = title and the optional note; cleared by done_ack on the goal doc),
or an open item parked for clarification (kind "clarify_request": a marker chip or Explain; text = "<dimension> (<option>)"
or "general"; cleared by a later `updates` entry or by the Architect setting parked.relayed_at).

Why: the Architect runs it before each reply so an owner answer is never missed.
What: prints the session-qualified id (S14:Ddb; bare when the item has no known session), time and answer per item; exit 1 if any exist, 0 if none, 2 on a store error.
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
        items = owner_answers.qualified(args.root, owner_answers.pending(args.root))
    except OSError as e:
        print(f"pending-answers: cannot read store: {e}", file=sys.stderr)
        return 2
    for it in items:
        text = f"INTERACTIVE: {it['id']} {it['text']}" if it["kind"] == "interactive" else it["text"]
        print(f"{it['qid']}\t{it['kind']}\t{it['at']}\t{text}")
    return 1 if items else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""questions.py — read and act on the owner-question event log.

Why: the collector publishes `question.opened`; the supervisor records what
happens next (answer, relay, retirement) as events on the same log, so every
channel and every restart sees one history (scripts/qevents.py).
What:
  list                                   open questions, urgent first
  answered <id> <channel> <answer...>    the user's answer and where it came from
  relayed  <id> <target> <text...>       what was sent to which PM session
  retire   <id> <disposition> <reason...>
  resolve  <id> <disposition> <answer...>  answered (channel chat) + retire;
                                           kept for the common one-step case
  history  <id>                          every event for one question
Usage: scripts/questions.py <command> ...
"""
import json
import sys

import qevents as qe


def main():
    a = sys.argv[1:]
    cmd = a[0] if a else "list"
    if cmd == "list":
        items = sorted(qe.open_questions(), key=lambda q: (not q.get("urgent"), q["found"]))
        for q in items:
            flag = "URGENT " if q.get("urgent") else ""
            dlg = " [dialog]" if q.get("dialog") else ""
            print(f'{q["id"]} {q["found"]} {flag}[{q["session"]}]{dlg} {q["question"]}')
            for o in q.get("options", []):
                print(f"    - {o}")
            if q.get("recommendation"):
                print(f'    rec: {q["recommendation"]}')
            for ans in q["answers"]:
                print(f'    answered ({ans["channel"]}): {ans["answer"]}')
        print(f"open: {len(items)}")
        return
    if cmd == "history" and len(a) == 2:
        for e in qe.read():
            if e.get("id") == a[1]:
                print(json.dumps(e))
        return
    if len(a) < 4:
        sys.exit(__doc__)
    qid, x, text = a[1], a[2], " ".join(a[3:])
    state = qe.fold()
    if qid not in state:
        sys.exit(f"unknown question {qid}")
    if state[qid]["retired"] and cmd != "answered":
        sys.exit(f"{qid} already retired")
    if cmd == "answered":
        qe.append("question.answered", id=qid, channel=x, answer=text, by="user")
    elif cmd == "relayed":
        qe.append("question.relayed", id=qid, target=x, text=text)
    elif cmd == "retire":
        qe.append("question.retired", id=qid, disposition=x, reason=text)
    elif cmd == "resolve":
        if x == "asked-user":
            qe.append("question.answered", id=qid, channel="chat", answer=text, by="user")
        qe.append("question.retired", id=qid, disposition=x, reason=text)
    else:
        sys.exit(__doc__)
    print(f"{cmd} {qid}; open: {len(qe.open_questions())}")


if __name__ == "__main__":
    main()

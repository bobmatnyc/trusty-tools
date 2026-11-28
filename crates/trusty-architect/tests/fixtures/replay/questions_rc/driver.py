"""Replay driver for questions_rc.py and start-architect-questions.sh (#12029 P2f).

Why: both scripts find their checkout from their own location, run tmux from PATH on the fleet's default
server, and the launcher reads the wall clock through `date`. This file is copied beside the frozen
questions_rc.py, start-architect-questions.sh, dashboard_store.py, dashboard_jsonl.py and dashboard_schema.py at
record time as qrc_driver.py, and points both scripts at the sandbox root (the working directory) instead.
What: three modes, each run on the recorder's frozen clock.
  rc ARGS...      questions_rc.main(ARGS) with ROOT = the sandbox root, the store at local/owner-queue, schemas at
                  schemas/, HERE = "" (the action scripts run by bare name, so the recording's fakes answer), every
                  tmux call on the private socket -L trusty-replay-p2f, and settle's 2 s sleeps as no-ops.
  start ARGS...   the launcher under bash with $0 = <root>/scripts/start-architect-questions.sh (so ROOT is the
                  sandbox root) and two shell functions defined first: tmux (adds -L trusty-replay-p2f) and date
                  (the frozen instant for the two formats the script uses). The script's text is unchanged.
  cases FILE      the pure functions, one {"op": ...} per case: `case <i> <op>` then the answer, indented.
examples/replay_port.rs mirrors it as `replay_port questions_rc` (trusty_architect::questions::cases).
Usage: driver.py rc ARGS... | start ARGS... | cases CASES.json
"""
import json
import os
import subprocess
import sys
import time
import types

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
CWD = os.getcwd()
SOCK = "trusty-replay-p2f"

import dashboard_schema as dsch  # noqa: E402
import dashboard_store as ds  # noqa: E402
import questions_rc as q  # noqa: E402

dsch.SCHEMA_DIR = os.path.join(CWD, "schemas")
ds.DEFAULT_ROOT = os.path.join(CWD, "local", "owner-queue")
ds.Store.__init__.__defaults__ = (ds.DEFAULT_ROOT,) + ds.Store.__init__.__defaults__[1:]
q.ROOT = CWD
q.HERE = ""
q.read_queue.__defaults__ = (os.environ.get("ARCH_Q_QUEUE", os.path.join(CWD, "inbox", "interactive-queue.txt")),)
q.settle.__defaults__ = (None, lambda n: None, 4)
_run = subprocess.run


def _private(cmd, *a, **k):
    if cmd and cmd[0] == "tmux":
        cmd = ["tmux", "-L", SOCK] + list(cmd[1:])
    return _run(cmd, *a, **k)


q.subprocess = types.SimpleNamespace(run=_private)


def dumps(v):
    return json.dumps(v, ensure_ascii=False)


def start(args):
    now = time.gmtime()
    shim = (
        'tmux() { command tmux -L ' + SOCK + ' "$@"; }\n'
        'date() { case "$*" in '
        "'-u +%H:%MZ') echo " + time.strftime("%H:%MZ", now) + " ;; "
        "'-u +%H:%M:%SZ') echo " + time.strftime("%H:%M:%SZ", now) + " ;; "
        '*) command date "$@" ;; esac; }\n'
        '__src=$1; shift; . "$__src"\n'
    )
    os.makedirs(os.path.join(CWD, "scripts"), exist_ok=True)
    name = os.path.join(CWD, "scripts", "start-architect-questions.sh")
    sys.stdout.flush()
    p = subprocess.run(["bash", "-c", shim, name, os.path.join(HERE, "start-architect-questions.sh"), *args])
    return p.returncode


def answer(c):
    op = c["op"]
    if op == "write":
        os.makedirs(os.path.dirname(c["path"]) or ".", exist_ok=True)
        with open(c["path"], "wb") as f:
            f.write(bytes.fromhex(c["hex"]) if "hex" in c else c["text"].encode("utf-8"))
        return ["ok"]
    if op == "open_ids":
        return [dumps(q.open_ids(c["statuses"], c["queued"]))]
    if op == "decide":
        return [q.decide(c["count"], c["up"], c["rc"])]
    if op == "lines":
        return [dumps([q.rc_from_capture(c["lines"]), q.url_from_capture(c["lines"])])]
    if op == "pane":
        lines = c["text"].splitlines()
        return [dumps([q.rc_from_capture(lines), q.url_from_capture(lines)])]
    if op == "settle":
        seq, sleeps, reads = iter(c["seq"]), [], [0]

        def observe():
            reads[0] += 1
            return tuple(next(seq))

        kw = {"tries": c["tries"]} if "tries" in c else {}
        up, rc = q.settle(c["action"], True, "s", observe=observe, sleep=sleeps.append, **kw)
        return [dumps([up, rc, len(sleeps), reads[0]])]
    if op == "step":
        calls = []
        action, count = q.step(c["statuses"], c["queued"], c["up"], c["rc"],
                               runner=lambda a, ids: calls.append([a, ids]), dry_run=c.get("dry", False))
        return [dumps([action, count, calls])]
    if op == "write_state":
        doc = q.write_state(c["rc"], c["url"], c["count"], path=c["path"])
        with open(c["path"], encoding="utf-8") as f:
            text = f.read()
        assert json.loads(text) == doc
        return [text]
    if op == "queue":
        return [dumps(q.read_queue(c["path"]))]
    raise KeyError(op)


def cases(path):
    with open(path, encoding="utf-8") as f:
        items = json.load(f)
    for i, case in enumerate(items):
        print(f"case {i} {case['op']}", flush=True)
        try:
            lines = answer(case)
        except UnicodeDecodeError:
            lines = ["UnicodeDecodeError"]
        except OSError:
            lines = ["OSError"]
        for line in lines:
            print(f"  {line}", flush=True)
    return 0


def main(argv):
    if argv[:1] == ["rc"]:
        return q.main(argv[1:])
    if argv[:1] == ["start"]:
        return start(argv[1:])
    if len(argv) == 2 and argv[0] == "cases":
        return cases(argv[1])
    sys.stderr.write("usage: driver.py rc ARGS... | start ARGS... | cases CASES.json\n")
    return 2


sys.exit(main(sys.argv[1:]))

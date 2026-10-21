"""Regenerate the #9758 writer parity corpus from the prototype `datg.py`.

Why: the Rust writer must leave the same bytes on disk and print the same
stdout as the prototype for every write verb (LDG-1.5, LDG-3.3). The goldens
must come from running the Python, never from the Rust.
What: loads `datg.py` read-only by path from the prototype directory given as
the one argument (-B -I, sys.dont_write_bytecode). For each scrubbed store in
`../<store>/`, copies it into a system tempdir as `datg/<store>/` and runs one
chain of verbs through `datg.main` with a pinned clock (one minute per step):
add, update, close, batch (add, update, session), a batch that crosses the
8 KB bar, prune --dry-run and prune two days on, and show. `DATG_NOTIFY_CMD` is `true`, so no escalation reaches pm-notify.
Writes `corpus.jsonl` (one line per step: the batch ops the CLI built, exit
code, stdout, the bytes appended to `log.jsonl` and `archive.jsonl`, and the
`state.json` length and `v`) and `<store>.state.json` (the final snapshot).
Test: `ledger_writer_matches_prototype_bytes` in tests/ledger_writer_parity.rs.

Regenerate (from the repository root):
    python3 -B -I crates/trusty-architect/tests/fixtures/ledger/writer/regen_writer.py <prototype-dir>
where <prototype-dir> is the Architect prototype's scripts/ directory.
"""
import contextlib
import importlib.util
import io
import json
import os
import shutil
import sys
import tempfile
from datetime import datetime, timezone

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
STORES = ("pm-small", "pm-medium", "pm-over-warn", "pm-over-refuse")
START = datetime(2026, 10, 21, 10, 0, tzinfo=timezone.utc)
LABEL = ("lorem ipsum dolor sit amet elit sed tempor magna " * 4)[:160]
if len(sys.argv) != 2:
    sys.exit("usage: regen_writer.py <prototype-dir>")
os.environ["DATG_NOTIFY_CMD"] = "true"
spec = importlib.util.spec_from_file_location("datg", os.path.join(sys.argv[1], "datg.py"))
datg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(datg)
NOW = [START]


class FixedDatetime(datetime):
    @classmethod
    def now(cls, tz=None):
        return NOW[0]


class FixedTime:
    @staticmethod
    def time():
        return NOW[0].timestamp()


datg.datetime = FixedDatetime
datg.time = FixedTime
datg.now_iso = lambda: NOW[0].strftime("%Y-%m-%dT%H:%MZ")


def chain(code, goal):
    """(verb, cli argv, batch ops as `tm dbatg` builds them, stdin) per step."""
    t = f"{code}-T"
    return [
        ("add", ["add", "T", "--title", "lorem ipsum dolor", "--goal", goal],
         [{"op": "add", "kind": "T", "title": "lorem ipsum dolor", "goal": goal}]),
        ("add", ["add", "D", "--title", "drift: sit amet", "--goal", goal, "--opts", "lorem*,ipsum"],
         [{"op": "add", "kind": "D", "title": "drift: sit amet", "goal": goal,
           "opts": [{"l": "lorem", "rec": True}, {"l": "ipsum", "rec": False}]}]),
        ("add", ["add", "T", "--title", "magna", "--goal", f"{code}-G99"],
         [{"op": "add", "kind": "T", "title": "magna", "goal": f"{code}-G99"}]),
        ("update", ["update", "{D}", "--esc", "architect"], [{"op": "update", "id": "{D}", "esc": "architect"}]),
        ("update", ["update", "{D}", "--answer", "sit amet elit"],
         [{"op": "update", "id": "{D}", "answer": "sit amet elit"}]),
        ("close", ["close", "{D}", "--outcome", "lorem ipsum"], [{"op": "close", "id": "{D}", "outcome": "lorem ipsum"}]),
        ("batch", ["batch"], [
            {"op": "add", "kind": "G", "title": "magna elit", "rank": 9, "finish": "sed tempor"},
            {"op": "update", "id": "{T}", "to": "agent", "status": "running", "note": "lorem"},
            {"op": "session", "task": "lorem ipsum", "next": "dolor sit",
             "waiting_on": [{"who": "ci", "what": "amet", "since": "2026-10-21T09:00Z"}],
             "owed": [{"check": "sed", "expect": "abc1234", "ref": "tempor"}]}]),
        ("update", ["update", "{G}", "--rank", "2"], [{"op": "update", "id": "{G}", "rank": 2}]),
        ("close", ["close", "{T}", "--outcome", "magna elit"], [{"op": "close", "id": "{T}", "outcome": "magna elit"}]),
        ("batch", ["batch"], [{"op": "add", "kind": "D", "title": f"sit amet {i}", "goal": goal,
                               "opts": [{"l": LABEL[:150], "rec": j == 0} for j in range(4)]} for i in range(7)]),
        ("add", ["add", "D", "--title", "tempor", "--goal", goal, "--opts", ",".join([LABEL[:160]] * 4)],
         [{"op": "add", "kind": "D", "title": "tempor", "goal": goal,
           "opts": [{"l": LABEL[:160], "rec": False} for _ in range(4)]}]),
        ("add", ["add", "A", "--title", "elit", "--goal", goal], [{"op": "add", "kind": "A", "title": "elit", "goal": goal}]),
        ("update", ["update", "{A}", "--answer", "magna"], [{"op": "update", "id": "{A}", "answer": "magna"}]),
        ("prune", ["prune", "--dry-run"], [{"op": "prune", "dry_run": True}]),
        ("prune", ["prune"], [{"op": "prune"}]),
        ("show", ["show", "--brief"], []),
    ]


def fill(value, ids):
    if isinstance(value, str):
        for k, v in ids.items():
            value = value.replace("{" + k + "}", v)
        return value
    if isinstance(value, list):
        return [fill(x, ids) for x in value]
    if isinstance(value, dict):
        return {k: fill(x, ids) for k, x in value.items()}
    return value


def read(path):
    try:
        with open(path, "rb") as f:
            return f.read()
    except FileNotFoundError:
        return b""


def run(store, corpus):
    tmp = tempfile.mkdtemp(prefix="regen-writer-")
    try:
        d = os.path.join(tmp, ".trusty-mpm", "datg", store)
        shutil.copytree(os.path.join(HERE, "..", store), d)
        os.environ["DATG_SESSION"] = store
        st = json.load(open(os.path.join(d, "state.json")))
        goal = next(g["id"] for g in sorted(st["goals"], key=lambda g: g["rank"]) if g["status"] == "active")
        ids = {}
        steps = chain(st["code"], goal)
        if store == "pm-small":
            steps.append(("show", ["show"], []))
        for n, (verb, argv, ops) in enumerate(steps):
            # Prune and show run two days on, so the step-12 answer is old.
            NOW[0] = START.replace(minute=n, day=START.day + (2 if verb in ("prune", "show") else 0))
            argv, ops = fill(argv, ids), fill(ops, ids)
            before = {f: read(os.path.join(d, f)) for f in ("log.jsonl", "archive.jsonl")}
            out = io.StringIO()
            stdin = sys.stdin
            sys.stdin = io.StringIO(json.dumps(ops))
            try:
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                    code = datg.main(["--project", tmp, "--session", store] + argv)
            finally:
                sys.stdin = stdin
            state = read(os.path.join(d, "state.json"))
            for e in out.getvalue().strip().split(", ") if verb in ("add", "batch") and code == 0 else []:
                ids.setdefault(e[len(st["code"]) + 1], e)
            after = {f: read(os.path.join(d, f)) for f in before}
            corpus.write(json.dumps({
                "store": store, "step": n, "verb": verb, "minute": n, "ops": ops, "exit": code,
                "stdout": out.getvalue(),
                "log": after["log.jsonl"][len(before["log.jsonl"]):].decode(),
                "archive": after["archive.jsonl"][len(before["archive.jsonl"]):].decode(),
                "state_len": len(state), "v": json.loads(state)["v"],
            }, ensure_ascii=False) + "\n")
        with open(os.path.join(HERE, f"{store}.state.json"), "wb") as f:
            f.write(state)
    finally:
        shutil.rmtree(tmp)


with open(os.path.join(HERE, "corpus.jsonl"), "w", encoding="utf-8") as corpus:
    for s in STORES:
        run(s, corpus)

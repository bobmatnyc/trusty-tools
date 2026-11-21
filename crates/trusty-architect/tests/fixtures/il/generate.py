#!/usr/bin/env python3
"""Regenerates the Intent Ledger golden fixtures from the Python scripts (never run by CI).

Why: the Rust port must write the same bytes as `datg.py` and `ilio.py`. The goldens are
what the Python produced for a fixed ledger, a fixed list of steps and a frozen clock, so
`tests/il_golden.rs` can prove parity without python3 or the live ledgers.
What: builds `base_datg/` (a small synthetic ledger) with `datg.py`, replays `steps.jsonl`
on a copy of it with `datg.py`/`ilio.py`, and records each step's stdout, stderr and exit
code (`golden/steps.jsonl`) plus the final files (`golden/datg/`). Paths are written as
`@P@` (project) and `@I@` (ILIO_DIR).
Usage: generate.py <architect scripts dir> [--steps-only]
Test: `tests/il_golden.rs` replays the same steps with the Rust binary.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
FROZEN = os.path.join(os.path.dirname(HERE), "..", "support", "frozen_py.py")
T0 = 1795000000   # 2026-11-18T11:06:40Z
DAY = 86400
PROJECT_LEN = 150   # characters in the project path (`<tmp>/x.../p`); tests/il_golden.rs uses the same


def run(scripts, work, epoch, prog, args, stdin=None):
    env = dict(os.environ, FROZEN_EPOCH=str(epoch), DATG_NOTIFY_CMD="/usr/bin/true",
               DASHBOARD_ROOT=os.path.join(work, "dash"), ARCHITECT_INBOX_DIR=os.path.join(work, "inbox"),
               ILIO_PROJECT=os.path.join(work, "p"), ILIO_DIR=os.path.join(work, "ilio"), ILIO_WHO="tm-architect")
    for k in ("DATG_SESSION", "DATG_STATE_DIR", "TMUX_PANE"):
        env.pop(k, None)
    cmd = ["python3", "-I", FROZEN, scripts, prog] + (["--project", os.path.join(work, "p")] if prog == "datg" else []) + args
    r = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=env)
    return r


def norm(s, work):
    return s.replace(os.path.join(work, "p"), "@P@").replace(os.path.join(work, "ilio"), "@I@")


def base(scripts, work):
    os.makedirs(os.path.join(work, "dash"))
    os.makedirs(os.path.join(work, "inbox"))
    os.makedirs(os.path.join(work, "ilio"))
    steps = [
        (T0, ["init", "--code", "fx"], None),
        (T0 + 60, ["add", "G", "--title", "Ship the thing", "--finish", "v1 is out", "--maps", "G9"], None),
        (T0 + 120, ["add", "G", "--title", "Handle ad-hoc tasks (standing)"], None),
        (T0 + 180, ["add", "G", "--title", "Later: tidy the docs", "--rank", "8"], None),
        (T0 + 240, ["add", "D", "--title", "Which path? café ✓", "--goal", "fx-G01", "--opts", "Fast*,Slow"], None),
        (T0 + 300, ["add", "A", "--title", "Bob: sign the form", "--goal", "fx-G01", "--opts", "done*"], None),
        (T0 + 360, ["add", "D", "--title", "drift: scope creeping", "--goal", "fx-G01"], None),
        (T0 + 420, ["add", "T", "--title", "Write the spec", "--goal", "fx-G01", "--done-when",
                    "spec merged; see local/briefs/2026-11-01-spec.md"], None),
        (T0 + 480, ["add", "T", "--title", "Port the parser", "--goal", "fx-G02", "--done-when", "tests green"], None),
        (T0 + 540, ["add", "T", "--title", "日本語 task with ünicode", "--goal", "fx-G03", "--done-when", "done"], None),
        # More open decisions: `show --brief` caps its OPEN list at five ids.
        (T0 + 361, ["add", "D", "--title", "Extra decision one", "--goal", "fx-G01"], None),
        (T0 + 362, ["add", "D", "--title", "Extra decision two", "--goal", "fx-G01"], None),
        (T0 + 363, ["add", "D", "--title", "Extra decision three", "--goal", "fx-G02"], None),
        (T0 + 364, ["add", "D", "--title", "Extra decision four", "--goal", "fx-G02"], None),
        (T0 + 365, ["add", "D", "--title", "Extra decision five", "--goal", "fx-G01"], None),
        # More live tasks, so a small digest budget trims some of them and keeps others.
        (T0 + 550, ["add", "T", "--title", "Review the parser output against the spec examples", "--goal", "fx-G01",
                    "--done-when", "examples reviewed; notes filed in the brief"], None),
        (T0 + 560, ["add", "T", "--title", "Draft the migration plan for the old ledger", "--goal", "fx-G02",
                    "--done-when", "plan reviewed by the owner"], None),
        (T0 + 570, ["add", "T", "--title", "Benchmark the digest on the large ledger", "--goal", "fx-G01",
                    "--done-when", "numbers in the report"], None),
        (T0 + 580, ["add", "T", "--title", "Write the runbook for lock timeouts", "--goal", "fx-G02",
                    "--done-when", "runbook merged"], None),
        (T0 + 590, ["add", "T", "--title", "Audit the log rotation edge cases", "--goal", "fx-G03",
                    "--done-when", "edge cases listed with tests"], None),
        (T0 + 600, ["update", "fx-D01", "--answer", "Fast"], None),
        (T0 + 660, ["update", "fx-D02", "--esc", "architect"], None),
        (T0 + 720, ["batch"], json.dumps([
            {"op": "pulse", "id": "fx-T01", "note": "drafting", "pct": "40", "est": "2h", "remaining": "1h",
             "progress_note": "outline done", "progress_at": "2026-11-18T11:18:40Z", "est_in": 20000, "est_out": 5000,
             "act_in": 12500, "act_out": 3200, "act_models": "opus"},
            {"op": "remind", "id": "fx-T01", "at": "2026-11-19", "note": "check CI"},
            {"op": "remind", "id": "fx-T02", "at": "2026-11-18T12:00Z", "every": "14d", "note": "nudge the reviewer"},
            {"op": "act", "who": "tm-architect", "what": "recorded the spec decision", "owner_words": "go ahead",
             "evidence": "PR #1", "goal": "fx-G01", "refs": "PR #1"},
            {"op": "act", "who": "tm-architect", "what": "second action"},
        ])),
        (T0 + 780, ["session", "set", "--task", "Ship the thing", "--next", "write the spec", "--wait", "owner:review",
                    "--owe", "ci=green@run 12", "--agent", "agt:fx-G01:parser", "--cron", "c1=*/5 * * * *=poll",
                    "--ref", "spec=local/briefs/spec.md", "--ctx-use", "40:20:150000:keep this"], None),
        (T0 + 840, ["close", "fx-T03", "--outcome", "verified on host, PR #7"], None),
    ]
    for epoch, args, stdin in steps:
        r = run(scripts, work, epoch, "datg", args, stdin)
        if r.returncode != 0:
            sys.exit(f"base step {args} failed: {r.stderr}")
    d = os.path.join(work, "p", ".trusty-mpm", "datg")
    p = os.path.join(d, "state.json")
    st = json.load(open(p))
    st["config"]["wip_cap"] = 3   # a low cap so the steps below reach the overflow queue
    open(p, "w").write(json.dumps(st, separators=(",", ":"), ensure_ascii=False) + "\n")
    # A rotated log from an earlier week, for `acts` across files and `rotate`.
    open(os.path.join(d, "log-20261101.jsonl"), "w").write(
        '{"ts":"2026-11-01T09:00Z","session":"","kind":"act","who":"tm-architect","what":"old action","owner_words":"",'
        '"evidence":"","goal":"fx-G01","refs":"issue #2"}\n')
    shutil.copy(os.path.join(HERE, "ilio", "r-library.json"), os.path.join(work, "ilio", "r-library.json"))


def steps_list():
    S = T0 + 2 * DAY
    out = []

    def d(args, epoch=S, stdin=None):
        out.append({"epoch": epoch, "prog": "datg", "args": args, "stdin": stdin})

    def i(args, epoch=S, stdin=None):
        out.append({"epoch": epoch, "prog": "ilio", "args": args, "stdin": stdin})

    for n in (3000, 1000, 900, 800, 700, 650, 600, 550, 500, 450, 400, 350, 300, 200):   # partial task trims at the middle budgets
        d(["digest", "--no-log", "--max-tokens", str(n)])
    d(["show", "--brief"])
    d(["show"])
    d(["fresh"])
    d(["fresh", "--max-age", "5"])
    d(["fresh", "--since", "2026-11-18T11:06Z"])
    d(["acts"])
    d(["acts", "--since", "48h", "--json"])
    d(["acts", "--goal", "fx-G01", "--limit", "1"])
    d(["reminders"])
    d(["reminders", "--all", "--json"])
    d(["add", "T", "--title", "Fourth task", "--goal", "fx-G02", "--done-when", "x"])
    d(["add", "T", "--title", "Fifth task", "--goal", "fx-G01", "--done-when", "y"], S + 60)
    d(["update", "fx-T01", "--priority", "1"], S + 120)
    d(["update", "fx-T02", "--status", "done", "--outcome", "merged PR #12"], S + 180)
    d(["update", "fx-D01", "--status", "done", "--outcome", "verified"], S + 240)
    d(["add", "A", "--title", "Check the build", "--goal", "fx-G01", "--opts", "yes*,no"], S + 300)
    d(["update", "fx-A02", "--answer", "yes"], S + 360)
    d(["update", "fx-D02", "--esc", ""], S + 420)
    d(["remind", "fx-T01", "--at", "2026-11-21", "--every", "7d", "--note", "ping the owner"], S + 480)
    d(["remind", "fx-T01", "--at", "2026-11-21", "--every", "7d", "--note", "ping the owner"], S + 481)
    d(["remind-done", "fx-T01", "1"], S + 540)
    d(["remind-fire", "--dry-run"], S + 3 * DAY)
    d(["remind-fire"], S + 3 * DAY)
    d(["remind-done", "fx-T01", "3", "--end"], S + 3 * DAY + 60)
    d(["act", "--who", "tm-architect", "--what", "reviewed the digest", "--refs", "local/briefs/x.md"], S + 600)
    d(["session", "set", "--next", "port the parser", "--owe", "none", "--wait", "none", "--ref", "spec=none", "--boundary"], S + 660)
    d(["batch"], S + 720, json.dumps([
        {"op": "add", "kind": "D", "title": "Batch decision", "goal": "fx-G01", "opts": [{"l": "A", "rec": True}, {"l": "B", "rec": False}]},
        {"op": "update", "id": "fx-T01", "title": "Write the spec (v2)", "note": "retitled"},
        {"op": "act", "who": "w", "what": "batched act"},
    ]))
    d(["rebalance"], S + 780)
    d(["update", "fx-T01", "--priority", "9"], S + 800)
    d(["update", "fx-T99", "--title", "x"], S + 810)
    d(["add", "T", "--title", "No done when", "--goal", "fx-G01"], S + 820)
    d(["add", "T", "--title", "Relay the answer", "--goal", "fx-G01", "--done-when", "z"], S + 830)
    d(["add", "D", "--title", "x", "--goal", "nope"], S + 840)
    d(["close", "fx-T04", "--outcome", ""], S + 850)
    d(["session", "set", "--wait", "bad:thing"], S + 860)
    d(["session", "set", "--owe", "x=latest@r"], S + 870)
    d(["archive-show", "fx-T03"], S + 880)
    d(["archive-show", "fx-T77"], S + 881)
    d(["prune", "--dry-run"], S + 4 * DAY)
    d(["prune"], S + 4 * DAY + 60)
    d(["rotate"], S + 10 * DAY)
    d(["digest", "--no-log", "--max-tokens", "3000"], S + 10 * DAY + 60)
    d(["fresh", "--since", "later"], S + 10 * DAY + 70)
    i(["table"], S + 10 * DAY + 80)
    i(["table", "--max-tokens", "40"], S + 10 * DAY + 81)
    i(["relay", "R01-a", "sha=abc123", "where=main"], S + 10 * DAY + 82)
    i(["relay", "R09-z"], S + 10 * DAY + 83)
    ok = "```il\ntask: fx-T01@1118\ndone: outline written\npct: 60\nest: 2h\nremaining: 30m\nnext: finish the spec\n```\n"
    i(["check"], S + 10 * DAY + 90, ok)
    for n, block in enumerate([
        "no block here",
        "```il\ntask: fx-T01\ndone: x\npct: 5\n```",
        "```il\ntask: fx-T01\ndone: x\nnext: y\npct: 5\n```",
        "```il\ntask: fx-T01\ndone: x\npct: 5\nnext: y\nweird: 1\n```",
        "```il\ntask: fx-T01\ndone: " + "w" * 300 + "\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T01\ndone: key ghp_" + "a" * 24 + "\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T01\ndone: x\npct: 105\nnext: y\n```",
        "```il\ntask: fx-T0o\ndone: x\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T77\ndone: x\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T01@0101\ndone: x\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T01\ndone: see R01-z\npct: 5\nnext: y\n```",
        "```il\ntask: fx-T01\ndone: x\npct: 5\nremind: fx-T01 at=nope \"n\"\nnext: y\n```",
        "```il\ntask: fx-T01\ndone: x\npct: 5\nremind: fx-T99 at=2026-12-01 \"n\"\nnext: y\n```",
    ]):
        i(["check"], S + 10 * DAY + 100 + n, block)
    i(["parse"], S + 10 * DAY + 130, ok)
    i(["expand"], S + 10 * DAY + 131, ok.replace("done: outline written", "done: R01-a sha=1 where=x for fx-T01"))
    i(["apply"], S + 10 * DAY + 140, ok)
    i(["apply"], S + 10 * DAY + 141, ok.replace("remaining: 30m\n", ""))   # pct unchanged, done: claims progress, no remaining
    i(["apply"], S + 10 * DAY + 150,
      "```il\ntask: fx-T01\ndone: moved\npct: 70\nremaining: 20m\n"
      "new: T \"Review the PR\" done_when=\"approved; merged\"\nnew: T \"Review the PR\" done_when=\"approved; merged\"\n"
      "new: T \"Relay the notes\" multistep done_when=\"sent\"\n"
      "remind: fx-T01 at=2026-12-01T09:30Z every=7d \"check in\"\nnext: wait for review\ndelta: scope trimmed\ndecided: ship it\n"
      "act: pushed the branch | owner: go | ev: PR #3 | refs: PR #3\n```\n")
    i(["apply"], S + 10 * DAY + 160,
      "```il\ntask: fx-T01\ndone: closing\npct: 100\nclose: fx-T05 fx-T04\nnext: done\nevidence: verified by hand\n```\n")
    i(["apply"], S + 10 * DAY + 170, "```il\ntask: fx-T01\ndone: x\npct: 80\nnew: T \"No done when here\"\nnext: y\n```\n")
    i(["apply"], S + 10 * DAY + 171, "```il\ntask: fx-T01\ndone: x\npct: 81\nnew: D \"A decision\" detail=\"short\"\nnext: y\n```\n")
    i(["collapse", "--dry-run"], S + 10 * DAY + 200)
    i(["collapse"], S + 10 * DAY + 201)
    d(["digest", "--no-log", "--max-tokens", "800"], S + 10 * DAY + 300)
    return out


def main():
    scripts = sys.argv[1]
    # The digest cuts by length and its POINTERS line carries the project path, so the cut depends on
    # the path length; the test builds its project at exactly this many characters too.
    tmp = tempfile.mkdtemp(prefix="il-gen-")
    work = os.path.join(tmp, "x" * (PROJECT_LEN - len(tmp) - 3))
    os.makedirs(work)
    base(scripts, work)
    datg = os.path.join(work, "p", ".trusty-mpm", "datg")
    # Not under a `.trusty-mpm/` directory: the repo ignore rules skip those.
    dest = os.path.join(HERE, "base_datg")
    shutil.rmtree(dest, ignore_errors=True)
    os.makedirs(dest)
    for f in os.listdir(datg):
        if f != ".lock":
            shutil.copy(os.path.join(datg, f), dest)
    steps = steps_list()
    with open(os.path.join(HERE, "steps.jsonl"), "w") as f:
        for s in steps:
            f.write(json.dumps(s, ensure_ascii=False) + "\n")
    shutil.rmtree(os.path.join(HERE, "golden"), ignore_errors=True)
    os.makedirs(os.path.join(HERE, "golden", "datg"))
    with open(os.path.join(HERE, "golden", "steps.jsonl"), "w") as f:
        for s in steps:
            r = run(scripts, work, s["epoch"], s["prog"], s["args"], s["stdin"])
            f.write(json.dumps({"stdout": norm(r.stdout, work), "stderr": norm(r.stderr, work), "code": r.returncode},
                               ensure_ascii=False) + "\n")
    for name in os.listdir(datg):
        if name != ".lock":
            shutil.copy(os.path.join(datg, name), os.path.join(HERE, "golden", "datg", name))
    ep = os.path.join(work, "ilio", "epoch.json")
    if os.path.exists(ep):
        shutil.copy(ep, os.path.join(HERE, "golden", "epoch.json"))
    shutil.rmtree(tmp)


if __name__ == "__main__":
    main()

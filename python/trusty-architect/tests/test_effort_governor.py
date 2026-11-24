"""Effort governor: thresholds, restore, dedupe, stale readings, gated send (fixtures only, no panes)."""
import importlib.util
import json
import os
import stat
import subprocess
from datetime import datetime, timedelta, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S1: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")


def _load(name, f):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, f))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


eg = _load("effort_governor", "effort-governor.py")
# #10571 S1: the poller-hook check (fleet-poll.py effort_alerts and dedup) ports with the poller in S3.
NOW = datetime(2026, 10, 11, 5, 0, tzinfo=timezone.utc)
R5 = "2026-10-11T07:20:00Z"
RW = "2026-10-17T15:00:00Z"


def row(p5=10, pw=10, acct="a@x", age_min=2, r5=R5, rw=RW):
    ts = (NOW - timedelta(minutes=age_min)).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {"ts": ts, "account": acct, "pct5h": p5, "reset5h": r5, "pctwk": pw, "resetwk": rw}


def run(rows, state=None, sessions=("tm-a", "tm-b")):
    return eg.decide(sessions, eg.reading(rows, NOW), state or {})


@pytest.mark.parametrize("pct,want", [(89, None), (90, "medium"), (94, "medium"), (95, "low"), (100, "low")])
def test_thresholds_use_weekly_only(pct, want):
    got = {c["level"] for c in run([row(5, pct)])}
    assert got == ({want} if want else set())
    assert run([row(pct, 5)]) == []  # a 5-hour figure never drives a decision (owner 2026-10-31: "no 5 hour limits")
    assert run([row(None, pct, r5=None)]) == run([row(5, pct)])  # a missing 5h value is normal


def test_restore_on_reset_only_for_sessions_we_lowered():
    state = {"sessions": {"tm-a": {"level": "low", "account": "a@x", "reset": RW}}}
    out = run([row(10, 97)], state)
    assert [(c["session"], c["level"]) for c in out] == [("tm-b", "low")]  # tm-a already low
    after = run([row(10, 97, rw="2026-10-11T04:00:00Z")], state)  # weekly reset passed: 0%
    assert [(c["session"], c["level"]) for c in after] == [("tm-a", "auto")]  # tm-b never lowered


def test_restore_on_account_change_and_lower_level_change():
    state = {"sessions": {"tm-a": {"level": "low", "account": "old@x", "reset": R5}}}
    out = run([row(20, 20, acct="new@x")], state)
    assert [(c["session"], c["level"]) for c in out] == [("tm-a", "auto")]
    assert "account changed" in out[0]["reason"]
    state = {"sessions": {"tm-a": {"level": "low", "account": "a@x", "reset": R5}}}
    assert [c["level"] for c in run([row(10, 91)], state, ["tm-a"])] == ["medium"]


def test_commit_makes_decision_quiet_and_auto_forgets():
    rd = eg.reading([row(10, 96)], NOW)
    state = eg.commit({}, "tm-a", "low", rd, NOW)
    assert [c["session"] for c in run([row(10, 96)], state)] == ["tm-b"]
    eg.commit(state, "tm-a", "auto", rd, NOW)
    assert "tm-a" not in state["sessions"]


def test_fingerprint_is_stable_and_changes_with_window():
    a = run([row(10, 96)])[0]["fingerprint"]
    assert a == run([row(10, 96, age_min=1)])[0]["fingerprint"] == f"tm-a|low|{RW}"
    assert a != run([row(10, 96, age_min=1, rw="2026-10-18T15:00:00Z")])[0]["fingerprint"]


def test_stale_reading_and_architect_sessions_change_nothing():
    assert run([row(99, 99, age_min=25 * 60, r5=None, rw=None)]) == []
    assert run([row(10, 99)], sessions=["tm-architect", "tm-architect-poll"]) == []
    assert run([]) == []


def test_skipped_sessions_follow_the_architect_session_env(monkeypatch):
    """#10571 S1: the skip set is ARCHITECT_SESSION and its poll session, never an instance list."""
    monkeypatch.setenv("ARCHITECT_SESSION", "tm-arch2")
    renamed = _load("effort_governor_env", "effort-governor.py")
    assert renamed.SKIP == {"tm-arch2", "tm-arch2-poll"}
    rd = renamed.reading([row(10, 99)], NOW)
    assert [c["session"] for c in renamed.decide(["tm-arch2", "tm-arch2-poll", "tm-architect"], rd, {})] == \
        ["tm-architect"]


# --- effort-send.sh against a fake tmux -----------------------------------------------

FAKE = r'''#!/usr/bin/env python3
import json, sys
SF = __SF__
st = json.load(open(SF))
a = sys.argv[1:]
if a[:1] == ["-S"]:
    a = a[2:]
if a[0] == "capture-pane":
    print("\n".join(st["lines"] + ["-" * 20, "❯ " + st["input"], "-" * 20]))
elif a[0] == "send-keys":
    k = a[3:]
    if k[0] == "-l":
        st["input"] += k[1]
    elif k[0] == "Enter":
        st["sent"].append(st["input"]); st["input"] = ""
json.dump(st, open(SF, "w"))
'''


def send(tmp_path, lines=(), box="", level="low"):
    b = tmp_path / "bin"
    b.mkdir()
    sf = tmp_path / "t.json"
    sf.write_text(json.dumps({"lines": list(lines), "input": box, "sent": []}))
    for n, body in (("tmux", FAKE.replace("__SF__", repr(str(sf)))), ("sleep", "#!/bin/sh\nexit 0\n")):
        (b / n).write_text(body)
        (b / n).chmod(0o755 | stat.S_IEXEC)
    env = dict(os.environ, PATH=f"{b}:{os.environ['PATH']}", EFFORT_STATE=str(tmp_path / "es.json"),
               USAGE_LEDGER=str(tmp_path / "l.jsonl"), EFFORT_ECHO_SLEEP="0")
    (tmp_path / "l.jsonl").write_text(json.dumps(row(10, 96, age_min=0)) + "\n")
    r = subprocess.run([os.path.join(SCRIPTS, "effort-send.sh"), "tm-a", level], env=env,
                       capture_output=True, text=True, timeout=60)
    return r, json.loads(sf.read_text()), tmp_path / "es.json"


def test_send_types_enters_and_commits_at_idle_prompt(tmp_path):
    r, st, es = send(tmp_path, lines=["done."])
    assert r.returncode == 0, r.stdout + r.stderr
    assert st["sent"] == ["/effort low"]
    assert "tm-a" in json.loads(es.read_text())["sessions"]


@pytest.mark.parametrize("lines,box,why", [
    (["✻ Working… (12s · ↓ 1.2k tokens · esc to interrupt)"], "", "turn running"),
    (["✻ Cogitating… (3m · 4k tokens)"], "", "turn running"),
    ([], "half-written draft", "input box is typed"),
])
def test_send_skips_when_busy_or_draft(tmp_path, lines, box, why):
    r, st, es = send(tmp_path, lines=lines, box=box)
    assert r.returncode == 1 and f"tm-a SKIP {why}" in r.stdout
    assert st["sent"] == [] and not es.exists()

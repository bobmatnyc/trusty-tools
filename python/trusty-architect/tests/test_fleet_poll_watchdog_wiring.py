"""The poller's watchdog, il-cron and ctx-watch steps (#10571 Phase 2), ported from the prototype's watchdog tests.

Covers: run_architect_relaunch aborts on a live pane, runs the bound `tm architect up` and proves the launch
record (#11367), fails closed, and touches nothing while tm is not installed; architect_busy reads the spinner
and treats a finished turn as idle; the frozen alert re-emits once per 30 min; il_cron_alerts and ctx_watch_alerts map each injected
finding to one alert and survive a failing step; a cycle runs all three steps and appends their alerts.
Test: tests/test_fleet_poll_watchdog_wiring.py
"""
import importlib.util
import json
import os
import subprocess
import types

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
_spec = importlib.util.spec_from_file_location("fleet_poll_wd", os.path.join(SCRIPTS, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

S = "tm-architect"
LIVE = [{"session": S, "pane": "%1", "cmd": "claude"}]
SHELL = [{"session": S, "pane": "%1", "cmd": "zsh"}]
OTHER = [{"session": "tm-api", "pane": "%2", "cmd": "claude"}]
NOW = 1_000_000.0
TOP = "─" * 40
SPIN = "✻ Running PreToolUse hook… (2m 20s)"


@pytest.fixture
def tm_bin(tmp_path, monkeypatch):
    tm = tmp_path / "tm"
    tm.write_text("#!/bin/sh\n")
    monkeypatch.setattr(fp, "TM_BIN", str(tm), raising=False)  # raising=False: the pre-#11367 poller has none
    monkeypatch.setattr(fp, "tmux", lambda *a: pytest.fail(f"raw tmux call {a}"))
    return str(tm)


def _report(action="launch", state="shell", session=S):
    return json.dumps({"session": session, "action": action, "observed": {"state": state}})


UP_OK = _report()                                      # `up` launched bound into the shell
BOUND = _report("ensure", "live_bound")                # the `up --dry-run` that follows


def _relaunch(listings, outs=(UP_OK, BOUND), rc=0, raises=None):
    seq, ran, outs = list(listings), [], list(outs)

    def lister():
        return seq.pop(0) if len(seq) > 1 else seq[0]

    def runner(cmd, **kw):
        ran.append(cmd)
        if raises:
            raise raises
        return types.SimpleNamespace(returncode=rc, stdout=outs.pop(0) if outs else "")
    return fp.run_architect_relaunch(lister, runner), ran


def test_relaunch_aborts_when_live(tm_bin):
    result, ran = _relaunch([LIVE])
    assert result[0] == "aborted" and not ran


def test_relaunch_runs_the_bound_launch_path(tm_bin):
    # #11367: the watchdog calls `tm architect up`, which records the launch, then proves it bound.
    for panes in (OTHER, SHELL):                       # no session, or a pane at a shell
        result, ran = _relaunch([panes])
        assert result == ("ok", "tm architect up: live and bound")
        up = [tm_bin, "architect", "up", "--json", "--dir", fp.ROOT]
        assert ran == [up, up + ["--dry-run"]]


def test_relaunch_fails_closed(tm_bin):
    assert _relaunch([SHELL], rc=1)[0][0] == "failed"
    assert _relaunch([SHELL], outs=("not json",))[0] == ("failed", "tm architect up exit 0")
    assert _relaunch([SHELL], outs=(_report(session="other"),))[0][0] == "failed"
    # a live unbound claude is reported by up, never counted as a relaunch
    assert _relaunch([SHELL], outs=(_report("report", "live_unbound"),))[0][0] == "failed"
    # up launched, but no launch record binds what runs now
    bad = _report("report", "live_unbound")
    assert _relaunch([SHELL], outs=(UP_OK, bad))[0] == (
        "failed", "no launch record binds the relaunched Architect (live_unbound)")
    assert _relaunch([SHELL], raises=subprocess.TimeoutExpired("x", 1))[0][0] == "failed"
    assert _relaunch([SHELL], raises=OSError("exec"))[0][0] == "failed"


def test_relaunch_without_tm_never_touches_tmux(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "TM_BIN", str(tmp_path / "absent"))
    result, ran = _relaunch([OTHER])
    assert result == ("failed", "tm is not installed") and not ran
    monkeypatch.setattr(fp, "TM_BIN", "")
    assert _relaunch([OTHER])[0] == ("failed", "tm is not installed")


def test_debounce_and_failure_cap_are_unchanged():
    assert fp.watchdog.RELAUNCH_DEBOUNCE == 1800 and fp.watchdog.MAX_FAILURES == 2


def test_architect_busy_reads_spinner():
    pane = f"{SPIN}\n\n{TOP}\n❯ \n{TOP}\n  footer\n"
    busy, spinner = fp.architect_busy(LIVE, capturer=lambda pid: pane)
    assert busy and spinner == SPIN
    assert fp.architect_busy(SHELL, capturer=lambda pid: pane) == (False, "")


def test_architect_busy_completed_turn_is_idle():
    done = "✻ Brewed for 9s · done 7:14 AM · 1 monitor still running"
    monitor = "◯ inbox-watch  tail events  1m 5s · ↓ 2 tokens"
    pane = f"{done}\n\n{TOP}\n❯ \n{TOP}\n  footer\n{monitor}\n"
    assert fp.architect_busy(LIVE, capturer=lambda pid: pane) == (False, "")
    live = "✻ Brewing… (12s · ↓ 300 tokens)"
    pane = f"{live}\n\n{TOP}\n❯ \n{TOP}\n  footer\n"
    assert fp.architect_busy(LIVE, capturer=lambda pid: pane) == (True, live)


def test_frozen_alert_debounced_to_one_per_30_min():
    state = {}
    cand = lambda t: [fp.alert("architect_frozen", "frozen", "d", S, now=t)]  # noqa: E731
    assert len(fp.dedup(state, cand(NOW), NOW)[0]) == 1
    assert len(fp.dedup(state, cand(NOW + 600), NOW + 600)[0]) == 0
    assert len(fp.dedup(state, cand(NOW + 1801), NOW + 1801)[0]) == 1


def test_il_cron_alerts_map_each_finding():
    seen = []

    def step(now, dry_run):
        seen.append((now, dry_run))
        return [("il-cron|prune|failed", "prune failed 3 times")]
    out = fp.il_cron_alerts(NOW, True, step=step)
    assert seen == [(NOW, True)]
    assert [(a["kind"], a["fingerprint"], a["detail"]) for a in out] == [
        ("il_cron", "il-cron|prune|failed", "prune failed 3 times")]


def test_ctx_watch_alerts_map_each_candidate():
    seen = []

    def step(now, write):
        seen.append(write)
        return [{"session": "tm-api", "fingerprint": "S2|tm-api", "signal": "S2", "action": "clear",
                 "detail": "ctx 260000 at a boundary"}]
    out = fp.ctx_watch_alerts(NOW, dry_run=True, step=step)
    assert seen == [False]                                   # a dry run writes no ctx-watch state
    assert [(a["kind"], a["session"], a["detail"]) for a in out] == [
        ("ctxwatch", "tm-api", "S2 clear: ctx 260000 at a boundary")]


def test_a_failing_il_step_logs_and_yields_nothing(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))

    def boom(**kw):
        raise RuntimeError("bad schedule")
    assert fp.il_cron_alerts(NOW, step=boom) == []
    assert fp.ctx_watch_alerts(NOW, step=boom) == []
    log = (tmp_path / "poll.log").read_text()
    assert "il-cron ERROR RuntimeError: bad schedule" in log and "ctx-watch ERROR RuntimeError" in log


def test_cycle_runs_the_three_steps_and_appends_their_alerts(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "list_panes", lambda: [])
    monkeypatch.setattr(fp, "host_alerts", lambda now: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "EVENTS", str(tmp_path / "events.jsonl"))
    monkeypatch.setattr(fp, "ALERTS", str(tmp_path / "alerts.jsonl"))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    monkeypatch.setattr(fp, "QUIET_SESSIONS_FILE", str(tmp_path / "quiet.txt"))
    monkeypatch.setattr(fp, "dispatch_wake", lambda *a: "skipped: no tm-architect pane")
    monkeypatch.setattr(fp, "TM_BIN", str(tmp_path / "absent"))
    monkeypatch.setattr(fp, "architect_transcript_mtime", lambda: None)
    monkeypatch.setattr(fp.il_cron, "step", lambda now, dry_run: [("il-fp", "il detail")])
    monkeypatch.setattr(fp.ctx_watch, "step", lambda now, write: [
        {"session": "tm-api", "fingerprint": "cw", "signal": "S5", "action": "ask", "detail": "guard"}])
    state = {}
    fp.cycle(state, dry_run=False, now=NOW)
    with open(tmp_path / "alerts.jsonl") as f:
        kinds = sorted(json.loads(line)["kind"] for line in f)
    assert kinds == ["architect_dead", "ctxwatch", "il_cron"]   # no Architect pane: dead, relaunch fails
    assert "architect=dead" in (tmp_path / "poll.log").read_text()
    assert state["watchdog"]["failures"] == 1

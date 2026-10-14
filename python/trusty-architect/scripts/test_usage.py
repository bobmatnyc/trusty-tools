"""usage.py: /usage parsing, projection, reset-passed rule, cutover plan, alerts, sources, poller hook."""
import importlib.util
import json
import os
import stat
import subprocess
from datetime import datetime, timedelta, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("usage", os.path.join(HERE, "usage.py"))
u = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(u)
_fp = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fpoll = importlib.util.module_from_spec(_fp)
_fp.loader.exec_module(fpoll)

UTC = timezone.utc
SAMPLE = ("  Current session\n  ████▌   9% used\n  Resets 4:50pm (America/New_York)\n"
          "  Current week (all models)\n  ████   84% used\n  Resets Oct 9 at 3am (America/New_York)")
T = lambda s: datetime.fromisoformat(s.replace("Z", "+00:00"))
CAP = T("2026-10-07T16:12:00Z")


def row(ts, acct, p5, r5, pw, rw, src="manual", models=None):
    return {"ts": ts, "account": acct, "pct5h": p5, "reset5h": r5, "pctwk": pw, "resetwk": rw,
            "weekly_models": models or {}, "source": src}


def fleet(now_extra=()):
    """The 16:1xZ readings of 2026-10-07 plus the owner's 13:09Z reading (burn-rate pair)."""
    return sorted([
        row("2026-10-07T13:09:00Z", "owner@example.org", 3, "2026-10-07T17:59:00Z", 1, "2026-10-13T10:59:00Z"),
        row("2026-10-07T16:15:00Z", "owner@example.org", 100, "2026-10-07T18:00:00Z", 27, "2026-10-13T11:00:00Z", models={"fable": 0}),
        row("2026-10-07T16:12:00Z", "owner@example.com", 9, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
        row("2026-10-07T16:17:00Z", "r@consult.example", 0, None, 89, "2026-10-12T15:00:00Z"),
        row("2026-10-07T16:17:00Z", "owner@example.net", 0, None, 93, "2026-10-10T15:00:00Z"),
        row("2026-10-07T16:16:00Z", "support@consult.example", 0, None, 96, "2026-10-11T09:00:00Z"),
        row("2026-10-07T16:18:00Z", "owner@employer.example", 0, None, 50, "2026-10-12T11:00:00Z"),
        *now_extra], key=lambda r: r["ts"])


def test_parse_usage_capture_sample():
    d = u.parse_usage_capture(SAMPLE, CAP)
    assert (d["pct5h"], d["reset5h"]) == (9, "2026-10-07T20:50:00Z")
    assert (d["pctwk"], d["resetwk"]) == (84, "2026-10-09T07:00:00Z")


def test_parse_usage_capture_model_block_and_garbage():
    d = u.parse_usage_capture(SAMPLE + "\n  Current week (Fable)\n  0% used\n  Resets Oct 9 at 3am (America/New_York)", CAP)
    assert d["weekly_models"] == {"fable": 0} and d["pctwk"] == 84
    assert u.parse_usage_capture("no figures here", CAP) is None


def test_time_only_reset_rolls_to_next_day():
    assert u.iso(u.parse_reset("Resets 4:50pm (America/New_York)", T("2026-10-07T21:00:00Z"))) == "2026-10-08T20:50:00Z"


def test_projection_math_masa_pair():
    rows = [r for r in fleet() if r["account"] == "owner@example.org"]
    p = u.project(rows, "owner@example.org", T("2026-10-07T16:20:00Z"), "wk")
    # 1% at 13:09 -> 27% at 16:15 = 26 points in 3.1 h; 73 left at that rate
    assert p["rate"] == pytest.approx(26 / (186 / 60), rel=1e-6)
    assert p["at"] == T("2026-10-07T16:15:00Z") + timedelta(hours=73 / p["rate"])
    assert p["before_reset"] and p["pct"] == 27


def test_no_rate_from_one_sample_or_falling_or_gap():
    now = T("2026-10-08T00:00:00Z")
    one = [row("2026-10-07T16:12:00Z", "a", 9, None, 84, "2026-10-09T07:00:00Z")]
    assert u.burn_rate(one, "a", now, "wk") is None
    far = one + [row("2026-10-07T23:50:00Z", "a", 9, None, 90, "2026-10-09T07:00:00Z")]  # 7.6 h gap
    assert u.burn_rate(far, "a", now, "wk") is None
    other_window = one + [row("2026-10-07T16:40:00Z", "a", 9, None, 90, "2026-10-16T07:00:00Z")]
    assert u.burn_rate(other_window, "a", now, "wk") is None


def test_reset_passed_counts_as_zero_and_is_flagged_inferred():
    rec = row("2026-10-06T18:58:00Z", "d", 91, "2026-10-06T21:00:00Z", 25, "2026-10-12T11:00:00Z")
    e = u.effective(rec, T("2026-10-07T16:00:00Z"))
    assert (e["pct5h"], e["inferred5h"], e["pctwk"], e["inferredwk"]) == (0, True, 25, False)
    e2 = u.effective(rec, T("2026-10-12T12:00:00Z"))
    assert (e2["pctwk"], e2["inferredwk"]) == (0, True)
    started = u.effective(row("2026-10-07T16:17:00Z", "r", 0, None, 89, "2026-10-12T15:00:00Z"), T("2026-10-07T16:30:00Z"))
    assert started["pct5h"] == 0 and not started["inferred5h"]  # window not started: 0, not inferred


def test_plan_names_best_next_account_and_skips_exhausted_5h():
    now = T("2026-10-07T16:20:00Z")
    rows = fleet([row("2026-10-07T14:30:00Z", "owner@example.com", 1, "2026-10-07T20:50:00Z", 60, "2026-10-09T07:00:00Z")])
    p = u.plan(rows, now, "owner@example.com")  # 60% -> 84% in 1.7 h: weekly gone in ~1.1 h
    assert p["need"] and p["next"][0] == "owner@employer.example"  # lowest weekly (50%)
    assert ("owner@example.org", "5h window at 100%") in p["skipped"]
    assert [a for a, _ in p["alternatives"]] == ["r@consult.example", "support@consult.example", "owner@example.net"]  # 93/96 tie within 5 points: later reset first
    text = u.format_plan(p)
    assert "Next account: owner@employer.example" in text and "Switch by" in text


def test_plan_no_cutover_when_healthy_and_limit_reached_means_now():
    now = T("2026-10-07T16:20:00Z")
    healthy = [r for r in fleet() if r["account"] != "owner@example.com"]
    assert not u.plan(healthy, now, "owner@employer.example")["need"]
    p = u.plan(fleet(), now, "owner@example.org")
    assert p["need"] and p["switch_at"] == now and "5-hour limit reached" in p["triggers"][0][1]


def test_runway_capacity_hours_and_gap():
    now = T("2026-10-07T16:20:00Z")
    rw = u.runway(fleet(), now, "owner@example.com")
    assert rw["left"] == pytest.approx(73 + 16 + 11 + 4 + 7 + 50)  # one per account in fleet()
    assert rw["rate_account"] == "owner@example.org"
    assert rw["runway_h"] == pytest.approx(rw["left"] / rw["rate"])
    assert rw["first_reset"][1] == "owner@example.com"
    assert rw["gap_h"] == pytest.approx((T("2026-10-09T07:00:00Z") - now).total_seconds() / 3600)
    assert rw["covers_gap"] is False


def test_alerts_week_thresholds_and_5h_exhaustion_name_next_account():
    now = T("2026-10-07T16:20:00Z")
    rows = fleet([row("2026-10-07T14:30:00Z", "owner@example.com", 1, "2026-10-07T20:50:00Z", 60, "2026-10-09T07:00:00Z")])
    out = dict(u.usage_alerts(rows, now, "owner@example.com"))
    wk = [d for k, d in out.items() if k.startswith("wk>=80")]
    assert wk and "owner@employer.example" in wk[0] and not any(k.startswith("wk>=90") for k in out)
    assert any(k.startswith("cutover:") for k in out)
    rows = fleet([row("2026-10-07T15:40:00Z", "owner@example.com", 30, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
                  row("2026-10-07T16:20:00Z", "owner@example.com", 70, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z")])
    out = dict(u.usage_alerts(rows, T("2026-10-07T16:21:00Z"), "owner@example.com"))
    exh = [d for k, d in out.items() if k.startswith("5h-exhaust")]
    assert exh and "projected to hit 100% at 2026-10-07T16:" in exh[0]  # 60 %/h from 70%: 30 min
    far = fleet([row("2026-10-07T14:00:00Z", "owner@example.com", 1, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z"),
                 row("2026-10-07T16:20:00Z", "owner@example.com", 10, "2026-10-07T20:50:00Z", 84, "2026-10-09T07:00:00Z")])
    assert not [k for k, _ in u.usage_alerts(far, T("2026-10-07T16:21:00Z"), "owner@example.com") if k.startswith("5h-")]


def test_alert_detail_fits_poller_cap():
    rows = fleet([row("2026-10-07T14:30:00Z", "owner@example.com", 1, "2026-10-07T20:50:00Z", 60, "2026-10-09T07:00:00Z")])
    for k, d in u.usage_alerts(rows, T("2026-10-07T16:20:00Z"), "owner@example.com"):
        assert "owner@employer.example" in d[:fpoll.DETAIL_MAX], k


def _env(tmp_path, monkeypatch, account="owner@example.com"):
    (tmp_path / "inbox").mkdir()
    cj = tmp_path / "claude.json"
    cj.write_text(json.dumps({"oauthAccount": {"emailAddress": account}}))
    monkeypatch.setattr(u, "LEDGER", str(tmp_path / "usage" / "ledger.jsonl"))
    monkeypatch.setattr(u, "CACHE", str(tmp_path / "inbox" / "statusline-usage.json"))
    monkeypatch.setattr(u, "CLAUDE_JSON", str(cj))
    monkeypatch.setattr(u, "INBOX", str(tmp_path / "inbox"))
    return tmp_path


def _cache(tmp_path, account, five=12.5, week=84, r5=1791400000, rw=1791500000):
    payload = {"rate_limits": {"five_hour": {"used_percentage": five, "resets_at": r5},
                               "seven_day": {"used_percentage": week, "resets_at": rw}}}
    (tmp_path / "inbox" / "statusline-usage.json").write_text(account + "\n" + json.dumps(payload) + "\n")


def test_sample_cache_records_matching_account_only(tmp_path, monkeypatch):
    _env(tmp_path, monkeypatch)
    now = datetime.now(UTC)
    _cache(tmp_path, "owner@example.net")
    assert u.sample_cache(now) is None                      # cache written under another account
    _cache(tmp_path, "owner@example.com")
    rec = u.sample_cache(now)
    assert rec["source"] == "statusline-cache" and rec["pctwk"] == 84 and rec["resetwk"] == u.iso(datetime.fromtimestamp(1791500000, UTC))
    assert u.sample_cache(now) is None                      # unchanged reading is not re-recorded


def test_cache_rejected_when_windows_equal_previous_accounts(tmp_path, monkeypatch):
    _env(tmp_path, monkeypatch)
    now = datetime.now(UTC)
    old = datetime.fromtimestamp(1791500000, UTC)
    u.append_ledger(row(u.iso(now - timedelta(hours=1)), "owner@example.org", 5, u.iso(datetime.fromtimestamp(1791400000, UTC)), 27, u.iso(old)))
    _cache(tmp_path, "owner@example.com")                 # stale carry-over right after /login
    assert u.sample_cache(now) is None


def test_poll_step_probes_on_account_change_and_failure_cannot_break_cycle(tmp_path, monkeypatch):
    _env(tmp_path, monkeypatch)
    launched = []
    state = {"usage": {"account": "owner@example.org", "since": 0}}
    now = 1_791_380_000.0
    u.poll_step(state, now, [("tm-x", "%1")], launcher=lambda *a: launched.append(a))
    assert launched == [("tm-x", "%1", "owner@example.com")] and state["usage"]["pending"]
    u.poll_step(state, now + 600, [("tm-x", "%1")], launcher=lambda *a: launched.append(a))
    assert len(launched) == 2                                # still pending: retry (max 3)
    u.poll_step(state, now + 700, [], launcher=lambda *a: launched.append(a))
    assert len(launched) == 2                                # no idle pane: never types

    def boom(*a, **k):
        raise RuntimeError("sampler broke")
    assert fpoll.usage_alerts({}, 0, [], step=boom) == []


def test_poll_step_probes_hourly_not_more(tmp_path, monkeypatch):
    _env(tmp_path, monkeypatch)
    launched = []
    state = {}
    now = 1_791_380_000.0
    for dt in (0, 600, 1200, 3700):
        u.poll_step(state, now + dt, [("tm-x", "%1")], launcher=lambda *a: launched.append(a))
    assert len(launched) == 2                                # at t=0 and t>=3600


def _exe(path, body):
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)


def test_statusline_tee_caches_and_passes_through(tmp_path):
    inbox = tmp_path / "inbox"; inbox.mkdir()
    cj = tmp_path / "c.json"; cj.write_text('{"oauthAccount": {"emailAddress": "a@b.c"}}')
    tm = tmp_path / "tm"; _exe(tm, "#!/bin/sh\necho RENDER:$(cat)\n")
    env = dict(os.environ, SUPERVISOR_INBOX_DIR=str(inbox), USAGE_CLAUDE_JSON=str(cj), TM_BIN=str(tm))
    run = lambda payload: subprocess.run([os.path.join(HERE, "statusline-tee.sh")], input=payload, env=env,
                                         capture_output=True, text=True)
    r = run('{"x":1}')
    assert r.stdout.strip() == 'RENDER:{"x":1}' and not (inbox / "statusline-usage.json").exists()
    r = run('{"rate_limits":{"five_hour":{"used_percentage":5}}}')
    assert r.returncode == 0 and "RENDER" in r.stdout
    first, _, rest = (inbox / "statusline-usage.json").read_text().partition("\n")
    assert first == "a@b.c" and json.loads(rest)["rate_limits"]["five_hour"]["used_percentage"] == 5
    env["SUPERVISOR_INBOX_DIR"] = str(tmp_path / "missing")  # cache failure must not break the render
    assert "RENDER" in run('{"rate_limits":{}}').stdout


FAKE_TMUX = r'''#!/usr/bin/env python3
import json, sys
SF = __STATE__
st = json.load(open(SF)); a = sys.argv[1:]
if a[:1] == ["-S"]: a = a[2:]
R = "-" * 20
if a[0] == "capture-pane":
    body = ["  Current session", "  9% used", "  Resets 4:50pm (America/New_York)", "  Current week (all models)",
            "  84% used", "  Resets Oct 9 at 3am (America/New_York)"] if st["shown"] else []
    print("\n".join(body + [R, "❯ " + st["input"], R] + (["esc to interrupt"] if st["busy"] else [])))
elif a[0] == "send-keys":
    k = a[3:]
    if k[0] == "-l": st["input"] += k[1]; st["typed"].append(k[1])
    elif k[0] == "Enter" and st["input"] == "/usage": st["input"] = ""; st["shown"] = True
    elif k[0] == "Escape": st["esc"] = True
json.dump(st, open(SF, "w"))
'''


def _probe(tmp_path, **init):
    bindir = tmp_path / "bin"; bindir.mkdir()
    state = tmp_path / "st.json"
    state.write_text(json.dumps({**dict(input="", shown=False, typed=[], busy=False, esc=False), **init}))
    _exe(bindir / "tmux", FAKE_TMUX.replace("__STATE__", repr(str(state))))
    _exe(bindir / "sleep", "#!/bin/sh\nexit 0\n")
    ledger = tmp_path / "ledger.jsonl"
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}", USAGE_LEDGER=str(ledger),
               RC_ECHO_TRIES="2", USAGE_WAIT_TRIES="2")
    r = subprocess.run([os.path.join(HERE, "usage-probe.sh"), "tm-x", "%1", "owner@example.com"],
                       env=env, capture_output=True, text=True, timeout=60)
    return r, json.loads(state.read_text()), ledger


def test_usage_probe_types_reads_escapes_and_ingests(tmp_path):
    r, st, ledger = _probe(tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    rec = json.loads(ledger.read_text())
    assert (rec["source"], rec["account"], rec["pct5h"], rec["pctwk"]) == ("usage-cmd", "owner@example.com", 9, 84)
    assert st["typed"] == ["/usage"] and st["esc"]


def test_usage_probe_never_types_over_a_draft_or_a_running_turn(tmp_path):
    r, st, ledger = _probe(tmp_path, input="half a thought")
    assert r.returncode == 2 and st["typed"] == [] and not ledger.exists()
    r, st, ledger = _probe(tmp_path / "b" if (tmp_path / "b").mkdir() is None else tmp_path, busy=True)
    assert r.returncode == 2 and st["typed"] == []

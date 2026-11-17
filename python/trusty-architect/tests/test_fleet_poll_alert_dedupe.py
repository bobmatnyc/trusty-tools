"""ar-T8a: host-alert dedupe (kind|severity|bucket) and hold-aware alerting in fleet-poll.py.

Why: on 2026-11-08 an unchanged swap alert woke a paused Architect 16 times, 14:21:38Z to 21:52:58Z,
because its key was the constant "swap" and swap re-emitted every 30 min. load and disk had the same key.
Covers the plan's eight cases (tests 1-8 below), the load and disk equivalents, the fail-open rules and the
two hold writers (the Architect's ledger HOLD via datg.py, and the owner's away/sleeping state).
Test: tests/test_fleet_poll_alert_dedupe.py
"""
import collections
import importlib.util
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll_dedupe_t", "fleet-poll.py")

T0 = datetime(2026, 11, 8, 14, 21, 38, tzinfo=timezone.utc).timestamp()   # first repeat alert
END = datetime(2026, 11, 8, 21, 52, 58, tzinfo=timezone.utc).timestamp()  # the sixteenth
DU = collections.namedtuple("DU", "total used free")


def host(kind, sev, bucket, now=T0):
    """A host alert as the new host_alerts builds it: fingerprint sev|bBUCKET plus the two fields."""
    a = fp.alert(kind, f"{sev}|b{bucket}", f"{kind} {sev} b{bucket}", now=now)
    a["severity"], a["bucket"] = sev, bucket
    return a


@pytest.fixture
def quiet_host(monkeypatch):
    """host_alerts on a host with low load and disk; the test sets the swap reading via `set_swap`."""
    monkeypatch.setattr(fp.os, "getloadavg", lambda: (0.1, 0.1, 0.1))
    monkeypatch.setattr(fp._io.shutil, "disk_usage", lambda p: DU(100, 10, 90))
    monkeypatch.setattr(fp._io, "_SWAP_PREV", [], raising=False)
    box = {"mb": 3079.0, "counters": (100, 100), "pressure": "normal"}

    def probe():
        return {"used_mb": box["mb"], "counters": box["counters"], "pressure": box["pressure"], "errors": []}
    monkeypatch.setattr(fp, "swap_probe", probe, raising=False)
    monkeypatch.setattr(fp, "swap_used_mb", lambda: box["mb"])   # the pre-ar-T8a host_alerts reads this
    return box


def _replay_wakes(box, active):
    """Cycles every 60 s across 14:21:38Z-21:52:58Z with 3,079 MB of swap; returns the cycles that woke."""
    st, woke, t, n = {}, [], T0, 0
    box["counters"] = (100, 100)
    getattr(fp._io, "_SWAP_PREV", []).clear()   # a fresh poller process
    while t <= END:
        n += 1
        if active:
            box["counters"] = (100 + n, 100 + n)   # swap-in/out moving every cycle
        new, _ = fp.dedup(st, fp.host_alerts(t), t)
        if [a for a in new if a["kind"] == "swap"]:
            woke.append(t)
        t += 60
    return woke


# --- 1 and 8: the 14:21-21:52Z replay -----------------------------------------------------------------

def test_leftover_swap_never_wakes_and_active_swap_wakes_once(quiet_host):
    """Case 1: 3,079 MB unchanged, no swap-in/out (Matt's reading): 0 wakes. Active swapping: exactly 1."""
    assert _replay_wakes(quiet_host, active=False) == []
    assert len(_replay_wakes(quiet_host, active=True)) == 1


def test_replay_of_the_16_alert_window_is_one_wake(quiet_host, monkeypatch):
    """Case 8: the same alert stream that produced 16 wakes (one per 30 min) now produces 1."""
    woke = _replay_wakes(quiet_host, active=True)
    assert len(woke) == 1, f"{len(woke)} wakes at {[fp.iso(t) for t in woke]}"


# --- 2: severity and bucket ---------------------------------------------------------------------------

def test_severity_and_bucket_rises_wake_and_a_fall_does_not():
    st, t = {}, T0
    assert len(fp.dedup(st, [host("swap", "warn", 2)], t)[0]) == 1
    assert fp.dedup(st, [host("swap", "warn", 2)], t + 60)[0] == []
    assert len(fp.dedup(st, [host("swap", "critical", 2)], t + 120)[0]) == 1   # severity rise
    assert len(fp.dedup(st, [host("swap", "critical", 3)], t + 180)[0]) == 1   # bucket rise
    assert fp.dedup(st, [host("swap", "critical", 2)], t + 240)[0] == []       # bucket fall
    assert fp.dedup(st, [host("swap", "warn", 2)], t + 300)[0] == []           # severity fall
    assert fp.dedup(st, [host("swap", "warn", 3)], t + 360)[0] == []           # back to the high-water mark


# --- 3 and 4: holds -----------------------------------------------------------------------------------

@pytest.fixture
def fleet(tmp_path, monkeypatch):
    """cycle() with no panes; the test sets the host alerts in box["host"]; wake() is recorded, not typed."""
    box = {"host": [], "wakes": []}
    monkeypatch.setattr(fp, "list_panes", lambda: [])
    monkeypatch.setattr(fp, "host_alerts", lambda now: list(box["host"]))
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "watchdog_alerts", lambda *a: ([], ""))
    monkeypatch.setattr(fp, "il_cron_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "ctx_watch_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "owner_answer_alerts", lambda now, dry_run, ever_read=False: ([], None))
    monkeypatch.setattr(fp, "self_refresh_step", lambda *a: "")
    (tmp_path / "events.jsonl").write_text("")
    for name, file in (("EVENTS", "events.jsonl"), ("ALERTS", "alerts.jsonl"), ("LOG", "poll.log"),
                       ("QUIET_SESSIONS_FILE", "quiet.txt"), ("STATE", "poll-state.json")):
        monkeypatch.setattr(fp, name, str(tmp_path / file))
    monkeypatch.setattr(fp, "HOLD", str(tmp_path / "hold.json"), raising=False)

    def wake(n, panes, dry_run, defer_turn=False):
        box["wakes"].append(n)
        return "sent to %0"
    monkeypatch.setattr(fp, "wake", wake)
    box["hold"] = tmp_path / "hold.json"
    box["alerts"] = tmp_path / "alerts.jsonl"
    return box


def _written(path):
    return [json.loads(l) for l in path.read_text().splitlines()] if path.exists() else []


def _hold(path, **entry):
    path.write_text(json.dumps({"holds": {"ledger": {"reason": "stopped until Monday reset",
                                                     "until": None, "until_event": "cleared", **entry}}}))


def test_a_hold_logs_non_urgent_and_wakes_urgent(fleet):
    """Case 3: held, swap warn and load are logged with `held` and wake nothing; owner_answer,
    architect_dead and swap critical still wake."""
    _hold(fleet["hold"])
    fleet["host"] = [host("swap", "warn", 3), host("load", "warn", 1),
                     fp.alert("owner_answer", "D01@x", "D01 answered", now=T0),
                     fp.alert("architect_dead", "dead", "no claude", now=T0)]
    state = {}
    fp.cycle(state, now=T0)
    assert fleet["wakes"] == [2]
    held = [a for a in _written(fleet["alerts"]) if a.get("held")]
    assert sorted(a["kind"] for a in held) == ["load", "swap"]
    fleet["host"].append(host("swap", "critical", 3, T0 + 60))
    fleet["host"] = [a for a in fleet["host"] if a.get("severity") != "warn" or a["kind"] != "swap"]
    fp.cycle(state, now=T0 + 60)
    assert fleet["wakes"] == [2, 1]   # the critical swap: urgent, held or not


def test_an_expired_or_unparseable_hold_is_not_held(fleet, tmp_path):
    """Case 4: an expired `until`, a non-string `until` or a broken file is not held, so the alert wakes."""
    at = fp.alert_triage
    p = tmp_path / "h.json"
    _hold(p, until="2026-11-08T14:00:00Z")
    assert at.read_hold(str(p), T0)["held"] is False
    _hold(p, until=12345)
    r = at.read_hold(str(p), T0)
    assert r["held"] is False and r["error"]
    p.write_text("{not json")
    r = at.read_hold(str(p), T0)
    assert r["held"] is False and r["error"]
    fleet["hold"].write_text("{not json")
    fleet["host"] = [host("swap", "warn", 3)]
    fp.cycle({}, now=T0)
    assert fleet["wakes"] == [1]


def test_release_emits_one_digest_and_no_replay(fleet):
    """Case 4: on release ONE held_digest wakes; the suppressed swap and load keys stay deduped."""
    _hold(fleet["hold"])
    fleet["host"] = [host("swap", "warn", 3), host("load", "warn", 1)]
    state = {}
    for k in range(5):
        fp.cycle(state, now=T0 + 60 * k)
    assert fleet["wakes"] == []
    fleet["hold"].unlink()
    fp.cycle(state, now=T0 + 300)
    assert fleet["wakes"] == [1]
    digest = _written(fleet["alerts"])[-1]
    assert digest["kind"] == "held_digest" and "2 alerts held" in digest["detail"]
    fp.cycle(state, now=T0 + 360)
    assert fleet["wakes"] == [1]


def test_agent_needs_input_wakes_while_held(fleet):
    """#10857: a PM agent waiting on input has permission_prompt's shape: it wakes through a hold."""
    _hold(fleet["hold"])
    fleet["host"] = [fp.alert("agent_needs_input", "ev1", "agent waits for input", "tm-x", "%1", now=T0),
                     host("swap", "warn", 3)]
    fp.cycle({}, now=T0)
    assert fleet["wakes"] == [1]
    assert [a["kind"] for a in _written(fleet["alerts"]) if a.get("held")] == ["swap"]


def test_urgent_classes_wake_while_held():
    at = fp.alert_triage
    for a in (fp.alert("owner_answer_error", "x", "d"), fp.alert("architect_frozen", "x", "d"),
              fp.alert("permission_prompt", "x", "d"), fp.alert("dialog", "x", "d"),
              fp.alert("pm_message", "x", "d"), host("swap", "critical", 3), host("disk", "critical", 96)):
        assert at.is_urgent(a), a["kind"]
    for a in (fp.alert("idle", "x", "d"), fp.alert("ctx", "x", "d"), host("swap", "warn", 3),
              host("disk", "warn", 92), host("load", "warn", 2), fp.alert("token", "x", "d")):
        assert not at.is_urgent(a), a["kind"]


# --- 5: restart and old-shape state -------------------------------------------------------------------

def test_restart_and_old_shape_state_do_not_re_wake(fleet, monkeypatch, tmp_path):
    """Case 5: the dedupe state survives a restart (poll-state.json, a fresh module), and a pre-ar-T8a
    entry ({first, last} under ||swap|swap) migrates without a wake; a critical still wakes."""
    fleet["host"] = [host("swap", "warn", 3)]
    state = {}
    fp.cycle(state, now=T0)
    fp.save_state(state)
    fresh = _load("fleet_poll_restarted", "fleet-poll.py")
    monkeypatch.setattr(fresh, "STATE", fp.STATE)
    reloaded = fresh.load_state()
    assert fresh.dedup(reloaded, [host("swap", "warn", 3, T0 + 1900)], T0 + 1900)[0] == []
    old = {"alerts": {"||swap|swap": {"first": T0 - 7200, "last": T0 - 60}}}
    assert fp.dedup(old, [host("swap", "warn", 3)], T0)[0] == []
    old = {"alerts": {"||swap|swap": {"first": T0 - 7200, "last": T0 - 60}}}
    assert len(fp.dedup(old, [host("swap", "critical", 3)], T0)[0]) == 1


def test_a_corrupt_state_entry_fails_open():
    """A dedupe entry that cannot be read never suppresses: the alert fires."""
    st = {"alerts": {"||swap": {"sev": 7, "bucket": "x"}, "||dialog|f": "garbage"}}
    assert len(fp.dedup(st, [host("swap", "critical", 3)], T0)[0]) == 1
    st = {"alerts": {"||swap": {"sev": 7, "bucket": "x"}, "||dialog|f": "garbage"}}
    assert len(fp.dedup(st, [fp.alert("dialog", "f", "d")], T0)[0]) == 1


# --- 6 and 7: clear debounce and the reminder ---------------------------------------------------------

def test_clear_for_two_cycles_then_recur_wakes():
    """Case 6: a one-cycle flap does not re-wake; two clear cycles then a recurrence does."""
    st, a = {}, host("swap", "warn", 3)
    assert len(fp.dedup(st, [a], T0)[0]) == 1
    fp.dedup(st, [], T0 + 60)                       # clear 1 cycle
    assert fp.dedup(st, [a], T0 + 120)[0] == []     # flap: no wake
    fp.dedup(st, [], T0 + 180)
    fp.dedup(st, [], T0 + 240)                      # clear 2 cycles
    assert len(fp.dedup(st, [a], T0 + 300)[0]) == 1


def test_the_24h_reminder_fires_once_and_never_while_held():
    """Case 7: hourly for 47 h: one reminder at 24 h when not held; none while held."""
    for held, expect in ((False, 2), (True, 1)):
        st, wakes = {}, 0
        for h in range(48):
            wakes += len(fp.dedup(st, [host("swap", "warn", 3)], T0 + 3600 * h, held=held)[0])
        assert wakes == expect, (held, wakes)


# --- load and disk -------------------------------------------------------------------------------------

def test_load_and_disk_carry_severity_and_bucket(quiet_host, monkeypatch):
    """Q5: load and disk had the constant key too; a steady load wakes once in the 16-alert window,
    disk wakes on a whole-percent rise and is critical (urgent) over 95%."""
    monkeypatch.setattr(fp.os, "getloadavg", lambda: (30.0, 25.0, 20.0))
    monkeypatch.setattr(fp.os, "cpu_count", lambda: 16)
    monkeypatch.delenv("LOAD_FACTOR", raising=False)
    quiet_host["mb"] = 0
    st, wakes, t = {}, 0, T0
    while t <= END:
        wakes += len([a for a in fp.dedup(st, fp.host_alerts(t), t)[0] if a["kind"] == "load"])
        t += 60
    assert wakes == 1
    monkeypatch.setattr(fp.os, "getloadavg", lambda: (0.1, 0.1, 0.1))
    st = {}
    for pct, expect in ((91.2, 1), (91.8, 0), (92.1, 1), (91.0, 0), (96.0, 1)):
        monkeypatch.setattr(fp._io.shutil, "disk_usage", lambda p, pct=pct: DU(1000, pct * 10, 0))
        disk = [a for a in fp.host_alerts(T0) if a["kind"] == "disk"]
        assert len(fp.dedup(st, disk, T0)[0]) == expect, pct
    assert disk[0]["severity"] == "critical" and fp.alert_triage.is_urgent(disk[0])


def test_a_pressure_probe_error_neither_invents_critical_nor_drops_warn(monkeypatch):
    """A failed pressure probe is an unknown reading: active swap stays warn, never critical."""
    def boom():
        raise OSError("sysctl missing")
    monkeypatch.setattr(fp, "swap_used_mb", lambda: 3079.0)
    monkeypatch.setattr(fp, "_swap_counters", lambda: (200, 200), raising=False)
    monkeypatch.setattr(fp, "_memory_pressure", boom, raising=False)
    r = fp.swap_probe()
    assert r["pressure"] is None and r["errors"] == ["memory pressure unknown: OSError"]
    a = fp.alert_dedupe.swap_alert(fp.alert, r, (100, 100), T0)
    assert a["severity"] == "warn" and "memory pressure unknown" in a["detail"]
    r["pressure"] = "critical"
    assert fp.alert_dedupe.swap_alert(fp.alert, r, (100, 100), T0)["severity"] == "critical"
    r["counters"] = None   # activity unknown: logged only, even under critical pressure
    assert fp.alert_dedupe.swap_alert(fp.alert, r, (100, 100), T0)["severity"] == "info"


# --- hold writers ---------------------------------------------------------------------------------------

def _datg(project, *args):
    return subprocess.run([sys.executable, os.path.join(SCRIPTS, "datg.py"), "--project", str(project), *args],
                          capture_output=True, text=True, check=True)


def test_ledger_hold_writes_and_clears_the_hold_file(tmp_path):
    """The Architect's existing stop/pause record (`datg.py session set --hold`) writes inbox/hold.json;
    `--hold none` removes it."""
    arch = tmp_path / "arch"
    (arch / "scripts").mkdir(parents=True)
    (arch / "scripts" / "fleet-poll.py").write_text("")
    il = arch / "local" / "il"
    il.mkdir(parents=True)
    _datg(il, "init", "--code", "ar")
    _datg(il, "session", "set", "--hold", "stopped until Monday reset")
    hold = arch / "inbox" / "hold.json"
    r = fp.alert_triage.read_hold(str(hold), T0)
    assert r["held"] and "stopped until Monday reset" in r["reason"]
    _datg(il, "session", "set", "--hold", "none")
    assert not hold.exists()
    pm = tmp_path / "pm"   # a PM's ledger hold is not the Architect's hold
    pm.mkdir()
    _datg(pm, "init", "--code", "pm")
    _datg(pm, "session", "set", "--hold", "no merges")
    assert not (pm / "inbox").exists() and not hold.exists()


def test_user_state_sleeping_holds_and_active_releases(fleet, monkeypatch):
    """The owner's Away/Sleeping state (meta/settings.user_state) holds; any other state releases."""
    ds = fp.owner_answers.ds
    store = ds.Store(os.environ["DASHBOARD_ROOT"])
    store.set("meta", "settings", {"user_state": "sleeping"})
    assert fp.user_state_hold() == "sleeping"
    assert fp.alert_triage.read_hold(str(fleet["hold"]), T0)["held"]
    fleet["host"] = [host("swap", "warn", 3)]
    fp.cycle({}, now=T0)
    assert fleet["wakes"] == []
    store.set("meta", "settings", {"user_state": "active"})
    assert fp.user_state_hold() == "active"
    assert not fleet["hold"].exists()

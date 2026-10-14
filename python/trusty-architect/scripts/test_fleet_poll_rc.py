"""`/rc failed` detection and the rc-restore debounce / skip logic in fleet-poll.py."""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

R = "─" * 40
BASE = f"⏺ Done.\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)"


def _info(footer, busy=False, box="empty", rc_failed=None):
    info = fp.parse_pane(f"{BASE} · {footer}\n")
    info["busy"], info["box"] = busy, box
    if rc_failed is not None:
        info["rc_failed"] = rc_failed
    return info


class Runner:
    def __init__(self, *results):
        self.results, self.calls = list(results), []

    def __call__(self, session):
        self.calls.append(session)
        return self.results.pop(0)


def test_rc_marker_detected():
    assert _info("/rc failed")["rc_failed"] is True
    assert _info("/rc")["rc_failed"] is False


def test_rc_debounce_and_skip_alert():
    state, run = {}, Runner(("restored", "https://claude.ai/code/session_x"), ("failed", "no URL"))
    failed = _info("/rc failed")
    a = fp.rc_step(state, "tm-a", "%1", failed, 1000, run)
    assert [x["kind"] for x in a] == ["rc_restored"] and "session_x" in a[0]["detail"]
    # within 10 min: no second run, no alert
    assert fp.rc_step(state, "tm-a", "%1", failed, 1000 + fp.RC_DEBOUNCE - 1, run) == []
    assert run.calls == ["tm-a"]
    # after 10 min: runs again; a failure alerts
    a = fp.rc_step(state, "tm-a", "%1", failed, 1000 + fp.RC_DEBOUNCE, run)
    assert [x["kind"] for x in a] == ["rc_failed"] and run.calls == ["tm-a", "tm-a"]
    # marker gone: state cleared, next failure runs at once
    assert fp.rc_step(state, "tm-a", "%1", _info("/rc"), 1000 + fp.RC_DEBOUNCE + 1, run) == []
    assert "tm-a" not in state["rc"]
    # busy pane: skip, no subprocess, alert only on the 3rd consecutive skip
    busy, run2, st2 = _info("/rc failed", busy=True), Runner(), {}
    kinds = [[x["kind"] for x in fp.rc_step(st2, "tm-b", "%2", busy, 5000 + i, run2)]
             for i in range(3)]
    assert kinds == [[], [], ["rc_failed"]] and run2.calls == []
    # typed input is a skip too; a later successful run resets the skip count
    typed = _info("/rc failed", box="typed")
    assert fp.rc_step({}, "tm-c", "%3", typed, 1, run2) == [] and run2.calls == []
    ok = Runner(("restored", "u"))
    fp.rc_step(st2, "tm-b", "%2", _info("/rc failed"), 6000, ok)
    assert st2["rc"]["tm-b"]["skips"] == 0


# --- Architect self-restore (architect_rc_step) ---------------------------------

def _acct(account, busy=False, box="empty", rc_failed=False):
    info = fp.parse_pane(f"{BASE} · TM 1.7.12 ● | architect | @x | ✻{account} | Opus 5.5 | ctx 3%\n")
    info["busy"], info["box"], info["rc_failed"] = busy, box, rc_failed
    return info


class Cap:
    """Fake capturer: returns the queued screens in order (last one repeats)."""
    def __init__(self, *screens):
        self.screens, self.calls = list(screens), 0

    def __call__(self, pane):
        self.calls += 1
        return self.screens.pop(0) if len(self.screens) > 1 else self.screens[0]


IDLE = f"{BASE}\n"
BUSY = f"✻ Working… (esc to interrupt)\n\n{R}\n❯ \n{R}\n"
TYPED = f"{R}\n❯ half a draft\n{R}\n"


def test_account_field_parsed_and_truncation_ignored():
    assert _acct("a@example.com")["account"] == "a@example.com"
    cut = fp.parse_pane(f"{BASE} · TM 1 ● | p | ✻a@example.co…\n")
    assert cut["account"] is None


def test_architect_account_change_detected():
    state, run = {}, Runner(("restored", "https://claude.ai/code/session_n"))
    cap = Cap(IDLE)
    # first sight records the account, no run
    assert fp.architect_rc_step(state, "%9", _acct("a@example.com"), 1000, run, cap) == ([], False, False)
    assert state["architect_account"] == "a@example.com" and run.calls == []
    # a different account triggers rc-restore.sh tm-architect and moves the record
    alerts, ran, fanout = fp.architect_rc_step(state, "%9", _acct("b@example.com"), 2000, run, cap)
    assert run.calls == ["tm-architect"] and ran and fanout
    assert [a["kind"] for a in alerts] == ["rc_restored"]
    assert state["architect_account"] == "b@example.com"
    # `/rc failed` alone (same account) restores too, without fan-out
    run2 = Runner(("restored", "u"))
    alerts, ran, fanout = fp.architect_rc_step(state, "%9", _acct("b@example.com", rc_failed=True),
                                               2000 + fp.RC_DEBOUNCE + 1, run2, cap)
    assert run2.calls == ["tm-architect"] and ran and not fanout


def test_architect_idle_gating():
    for info in (_acct("b@example.com", busy=True), _acct("b@example.com", box="typed"),
                 _acct("b@example.com", box="suggestion")):
        state, run, cap = {"architect_account": "a@example.com"}, Runner(), Cap(IDLE)
        assert fp.architect_rc_step(state, "%9", info, 1000, run, cap) == ([], False, False)
        assert run.calls == [] and cap.calls == 0
        assert state["architect_account"] == "a@example.com"  # retried next cycle


def test_architect_recheck_before_invoke():
    for screen in (BUSY, TYPED):
        state, run = {"architect_account": "a@example.com"}, Runner()
        alerts, ran, fanout = fp.architect_rc_step(state, "%9", _acct("b@example.com"), 1000, run, Cap(screen))
        assert run.calls == [] and not ran and not fanout
        assert state["architect_account"] == "a@example.com"
    # a failed restore keeps the old account so the next cycle retries
    state, run = {"architect_account": "a@example.com"}, Runner(("failed", "no URL"))
    alerts, ran, fanout = fp.architect_rc_step(state, "%9", _acct("b@example.com"), 1000, run, Cap(IDLE))
    assert [a["kind"] for a in alerts] == ["rc_failed"] and ran and not fanout
    assert state["architect_account"] == "a@example.com"


def test_architect_fanout_after_account_restore(monkeypatch):
    panes = [
        {"session": "tm-architect", "pane": "%9", "cmd": "claude", "active": "1"},
        {"session": "tm-a", "pane": "%1", "cmd": "claude", "active": "1"},
        {"session": "tm-architect-poll", "pane": "%5", "cmd": "python3", "active": "1"},
    ]
    screens = {"%9": f"{BASE} · TM 1 ● | p | ✻b@example.com | Opus\n", "%1": f"{BASE} · TM 1 ● | p | ✻b@example.com | Opus\n"}
    calls = []
    monkeypatch.setattr(fp, "list_panes", lambda: panes)
    monkeypatch.setattr(fp, "capture", lambda pid: screens[pid])
    monkeypatch.setattr(fp, "rename_windows", lambda dry=False: 0)
    monkeypatch.setattr(fp, "watchdog_alerts", lambda *a, **k: ([], "ok"))
    monkeypatch.setattr(fp, "usage_alerts", lambda *a, **k: [])
    monkeypatch.setattr(fp, "token_alerts", lambda *a, **k: [])
    monkeypatch.setattr(fp, "read_events", lambda s: [])
    monkeypatch.setattr(fp, "host_alerts", lambda *a, **k: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda *a, **k: [])
    # #8436: no live dashboard store; every alert wakes, so the deferral is what is tested.
    monkeypatch.setattr(fp, "owner_status_alerts", lambda *a, **k: [])
    monkeypatch.setattr(fp, "user_state_alerts", lambda *a, **k: [])
    monkeypatch.setattr(fp, "triage_step", lambda new, *a, **k: (list(new), 0))
    monkeypatch.setattr(fp, "INBOX", "/nonexistent")
    monkeypatch.setattr(fp, "ALERTS", os.devnull)
    monkeypatch.setattr(fp, "log", lambda *a, **k: None)
    monkeypatch.setattr(fp, "run_rc_restore", lambda s: (calls.append(s), ("restored", "u"))[1])
    # rc_step / architect_rc_step bind run_rc_restore as a default at def time: patch via wrapper
    monkeypatch.setattr(fp.rc_step, "__defaults__", (lambda s: (calls.append(s), ("restored", "u"))[1], ("empty", "suggestion")))
    monkeypatch.setattr(fp.architect_rc_step, "__defaults__",
                        (lambda s: (calls.append(s), ("restored", "u"))[1], None))
    woke = []
    monkeypatch.setattr(fp, "dispatch_wake", lambda *a: woke.append(a) or "x")
    state = {"architect_account": "a@example.com"}
    fp.cycle(state, now=5000)
    assert calls == ["tm-architect", "tm-a"]       # Architect first, then the PM (no /rc failed marker)
    assert woke == [] and state["architect_account"] == "b@example.com"  # no wake typed this cycle
    assert state["wake_pending"] >= 1
    assert "tm-architect" in state["rc"]           # not pruned with the PM-only session map

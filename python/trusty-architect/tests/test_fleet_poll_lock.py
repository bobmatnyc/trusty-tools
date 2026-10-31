"""One live poller per inbox (#10571 Phase 2): inbox/poll.lock, and the dashboard's poller name.

Covers: a second acquire of a held lock is busy; a second real `fleet-poll.py --once` exits 3 before it writes
poll.log or poll-state.json; an unknown lock state only logs and the poll runs; dashboard_fleet.POLLER follows
ARCHITECT_POLL_SESSION, then ARCHITECT_SESSION, as the poller's own name does.
Test: tests/test_fleet_poll_lock.py
"""
import importlib.util
import os
import subprocess
import sys

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll_lock", "fleet-poll.py")


def test_a_second_acquire_of_a_held_lock_is_busy(tmp_path):
    path = str(tmp_path / "poll.lock")
    held, fd = fp.acquire_poll_lock(path)
    try:
        assert held == "held"
        assert fp.acquire_poll_lock(path) == ("busy", f"another fleet poller holds {path}")
    finally:
        os.close(fd)
    held, fd = fp.acquire_poll_lock(path)   # released with its holder
    assert held == "held"
    os.close(fd)


def test_a_second_live_poller_exits_before_any_write(tmp_path):
    held, fd = fp.acquire_poll_lock(str(tmp_path / "poll.lock"))
    assert held == "held"
    try:
        env = {k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_SOCKET")}
        env.update(ARCHITECT_INBOX_DIR=str(tmp_path), ARCHITECT_SESSION="tm-lock-test-arch",
                   TMUX_SOCKET=str(tmp_path / "no-such-tmux.sock"))
        r = subprocess.run([sys.executable, os.path.join(SCRIPTS, "fleet-poll.py"), "--once"],
                           env=env, capture_output=True, text=True, timeout=60)
    finally:
        os.close(fd)
    assert r.returncode == 3, r.stderr
    assert "another fleet poller holds" in r.stderr
    assert sorted(os.listdir(tmp_path)) == ["poll.lock"]       # no poll.log, no poll-state.json


def test_an_unknown_lock_state_only_logs(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "INBOX", str(tmp_path))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    monkeypatch.setattr(fp, "STATE", str(tmp_path / "poll-state.json"))
    monkeypatch.setattr(fp, "acquire_poll_lock", lambda path: ("unknown", "cannot lock x: ENOLCK"))
    ran = []
    monkeypatch.setattr(fp, "cycle", lambda state, dry_run: ran.append(dry_run))
    assert fp.main(["--once"]) == 0
    assert ran == [False]
    assert "WARNING poll lock: cannot lock x: ENOLCK; polling without it" in (tmp_path / "poll.log").read_text()


def test_dashboard_poller_name_follows_the_architect_session(monkeypatch):
    monkeypatch.delenv("ARCHITECT_POLL_SESSION", raising=False)
    monkeypatch.setenv("ARCHITECT_SESSION", "tm-arch-7")
    assert _load("dashboard_fleet_a", "dashboard_fleet.py").POLLER == "tm-arch-7-poll"
    monkeypatch.setenv("ARCHITECT_POLL_SESSION", "tm-own-poller")
    assert _load("dashboard_fleet_b", "dashboard_fleet.py").POLLER == "tm-own-poller"
    monkeypatch.delenv("ARCHITECT_SESSION")
    monkeypatch.delenv("ARCHITECT_POLL_SESSION")
    assert _load("dashboard_fleet_c", "dashboard_fleet.py").POLLER == "tm-architect-poll"

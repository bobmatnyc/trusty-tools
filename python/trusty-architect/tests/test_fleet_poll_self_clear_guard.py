"""The poller types nothing into the Architect while self-clear.sh runs (#10571, #10599).

Covers: a pid file held under flock, or naming a live self-clear.sh, is running and wake() sends no key;
a free lock with a stale pid lets wake() proceed; a pid file that cannot be read counts as held; the one
send path refuses too, so no key path skips the check.
Test: tests/test_fleet_poll_self_clear_guard.py
"""
import fcntl
import importlib.util
import os
import subprocess
import sys

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
_spec = importlib.util.spec_from_file_location("fleet_poll_guard", os.path.join(SCRIPTS, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

R = "─" * 40
EMPTY_BOX = f"⏺ Done.\n\n✻ Cooked for 4s · done 1:00 AM\n\n{R}\n❯ \n{R}\n  ⏵⏵ bypass\n"
PANES = [{"session": fp.ARCHITECT, "pane": "%1", "cmd": "claude", "active": "11"}]


def _dead_pid():
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.wait()
    return p.pid


@pytest.fixture
def arch(tmp_path, monkeypatch):
    (tmp_path / "inbox").mkdir()
    monkeypatch.setattr(fp, "ROOT", str(tmp_path))
    return tmp_path


def _wake(monkeypatch):
    sent = []
    monkeypatch.setattr(fp, "capture", lambda pane: EMPTY_BOX)
    monkeypatch.setattr(fp, "tmux", lambda *a, **k: sent.append(a) or f"{fp.ARCHITECT}\n")
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    return fp.wake(1, PANES, dry_run=False), [a for a in sent if a[0] == "send-keys"]


def test_wake_skips_while_self_clear_holds_its_lock(arch, monkeypatch):
    pidfile = arch / "inbox" / "self-clear.pid"
    pidfile.write_text(f"{_dead_pid()}\n")
    fd = os.open(pidfile, os.O_RDWR)
    fcntl.flock(fd, fcntl.LOCK_EX)
    try:
        assert fp.self_clear_running(str(arch))
        result, keys = _wake(monkeypatch)
        with pytest.raises(fp.TmuxError, match="self-clear running"):
            fp.send_to_architect("%1", "Enter")
    finally:
        os.close(fd)
    assert result == "skipped: self-clear running" and keys == []


def test_wake_proceeds_when_the_lock_is_free(arch, monkeypatch):
    (arch / "inbox" / "self-clear.pid").write_text(f"{_dead_pid()}\n")   # stale: its process is gone
    assert not fp.self_clear_running(str(arch))
    result, keys = _wake(monkeypatch)
    assert keys and keys[0][3] == "-l"          # the pointer was typed
    assert not result.startswith("skipped")


def test_an_unreadable_self_clear_lock_counts_as_held(arch):
    pidfile = arch / "inbox" / "self-clear.pid"
    pidfile.write_text("not a pid\n")
    assert fp.self_clear_running(str(arch))
    pidfile.unlink()
    os.symlink(arch / "missing-target", pidfile)  # present, but cannot be opened
    assert fp.self_clear_running(str(arch))

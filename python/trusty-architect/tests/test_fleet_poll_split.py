"""fleet-poll.py split (#10571 Phase 2): moved names are re-exported, late-bound, and the self-refresh slot is wired.

Covers: every name moved to fleet_poll_{cfg,io,alerts,wake,selfrefresh}.py is still a fleet-poll attribute; a
moved function reads fleet-poll's names when it runs, so rebinding one (as a test or caller does) steers it; the
self-refresh stub returns "" and the cycle calls it once a cycle unless the wake was sent, leaving the log line
as it was.
Test: tests/test_fleet_poll_split.py
"""
import importlib.util
import os

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
_spec = importlib.util.spec_from_file_location("fleet_poll_split", os.path.join(SCRIPTS, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

MOVED = {
    "_cfg": ["ROOT", "INBOX", "EVENTS", "ALERTS", "STATE", "LOG", "SOCKET", "WORKSPACE_PREFIX", "ARCHITECT",
             "POLL_SESSION", "SKIP_SESSIONS", "SHELLS", "EVENT_TYPES", "DISK_HIGH", "LOAD_FACTOR_DEFAULT",
             "QUIET_SESSIONS_FILE"],
    "_io": ["TmuxError", "log", "tmux", "socket_dir", "workspace_servers", "list_panes", "_rows", "capture",
            "capture_pane", "send_to_architect", "read_events", "swap_used_mb", "host_alerts"],
    "_alerts": ["resolve_context_bar", "self_ctx_alerts"],
    "_wake": ["_norm", "_box_full_text", "_own_pointer", "wake", "dispatch_wake"],
    "_selfrefresh": ["self_refresh_step"],
}


def test_every_moved_name_is_re_exported_from_its_module():
    for mod, names in MOVED.items():
        for name in names:
            assert getattr(fp, name) is getattr(getattr(fp, mod), name), f"{mod}.{name}"


def test_a_moved_function_reads_fleet_polls_names_when_it_runs(monkeypatch):
    sent = []
    monkeypatch.setattr(fp, "tmux", lambda *a, **k: sent.append(a) or "tm-other\n")
    with pytest.raises(fp.TmuxError, match="refusing to send to %9"):
        fp.send_to_architect("%9", "x")
    assert sent == [("display-message", "-p", "-t", "%9", "#{session_name}")]   # no send-keys after the refusal


def test_self_refresh_stub_returns_nothing():
    assert fp.self_refresh_step({}, [], 0.0, False) == ""


def _cycle(monkeypatch, tmp_path, wake_result):
    calls = []
    monkeypatch.setattr(fp, "list_panes", lambda: [])
    monkeypatch.setattr(fp, "host_alerts", lambda now: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "EVENTS", str(tmp_path / "events.jsonl"))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    monkeypatch.setattr(fp, "QUIET_SESSIONS_FILE", str(tmp_path / "quiet.txt"))
    monkeypatch.setattr(fp, "dispatch_wake", lambda *a: wake_result)
    monkeypatch.setattr(fp, "self_refresh_step", lambda *a: calls.append(a) or "")
    fp.cycle({}, dry_run=False, now=100.0)
    return calls, (tmp_path / "poll.log").read_text().strip()


def test_cycle_calls_the_self_refresh_slot_unless_the_wake_was_sent(monkeypatch, tmp_path):
    calls, line = _cycle(monkeypatch, tmp_path, "none")
    assert len(calls) == 1 and calls[0][2] == 100.0
    assert line.endswith("panes=0 new=0 suppressed=0 wake=none")       # "" adds nothing to the log line
    calls, _ = _cycle(monkeypatch, tmp_path, "sent to %1")
    assert calls == []

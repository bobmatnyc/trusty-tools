"""Exited detection: a recorded session whose tmux session is gone, whose pane is dead or whose
pane runs a plain shell is marked exited on its record and alerts once per exit; Claude and tm are up."""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)


def put(root, sid, **data):
    d = root / "sessions"
    d.mkdir(exist_ok=True)
    (d / f"{sid}.json").write_text(json.dumps(dict(data, version=1)))


def rec(root, sid):
    return json.loads((root / "sessions" / f"{sid}.json").read_text())


def pane(session, cmd, active="11", dead="0"):
    return {"session": session, "pane": "%1", "cmd": cmd, "active": active, "path": "", "dead": dead}


T0 = 1_791_000_000  # 2026-10-09 ~02:00Z


def test_exit_reason_gone_dead_shell_versus_claude_and_tm():
    assert fp.exit_reason([]) == "tmux session gone"
    assert fp.exit_reason([pane("tm-a", "2.1.292", dead="1")]) == "pane dead"
    assert fp.exit_reason([pane("tm-a", "zsh")]) == "pane runs zsh, not Claude"
    assert fp.exit_reason([pane("tm-a", "-bash")]) == "pane runs -bash, not Claude"
    assert fp.exit_reason([pane("tm-a", "2.1.292")]) is None
    assert fp.exit_reason([pane("tm-a", "tm")]) is None
    assert fp.exit_reason([pane("tm-a", "zsh", active="00"), pane("tm-a", "2.1.292")]) is None   # the pane in front decides


def test_marks_record_exited_and_alerts_once_per_exit(tmp_path):
    put(tmp_path, "S03", name="tm-memory-01", state="idle", status="Idle")
    put(tmp_path, "S04", name="tm-search-01", state="idle")
    put(tmp_path, "S05", name="tm-old-01", state="parked", status="Parked")
    put(tmp_path, "S06", name="tm-architect", state="working")
    panes = [pane("tm-memory-01", "zsh"), pane("tm-search-01", "2.1.292"), pane("tm-architect", "zsh")]
    st = {}
    out = fp.exited_step(st, panes, T0, root=str(tmp_path))
    assert [a["kind"] for a in out] == ["session_exited"] and "tm-memory-01" in out[0]["display"]
    r = rec(tmp_path, "S03")
    hhmm = fp.iso(T0)[11:16]
    assert (r["state"], r["status"]) == ("exited", f"Exited: pane runs zsh, not Claude (detected {hhmm}Z)")
    assert r["version"] == 2
    assert rec(tmp_path, "S04")["state"] == "idle" and rec(tmp_path, "S05")["state"] == "parked" and rec(tmp_path, "S06")["state"] == "working"
    new, _ = fp.dedup(st, out, T0)
    assert len(new) == 1
    out2 = fp.exited_step(st, panes, T0 + 120, root=str(tmp_path))      # next poll: same detected time, no rewrite, no new alert
    assert rec(tmp_path, "S03")["version"] == 2 and fp.dedup(st, out2, T0 + 120)[0] == []
    fp.exited_step(st, [pane("tm-memory-01", "2.1.292"), pane("tm-search-01", "2.1.292")], T0 + 240, root=str(tmp_path))   # back up: forgotten
    assert st["exited"] == {}
    out3 = fp.exited_step(st, [], T0 + 360, root=str(tmp_path))          # exits again: a new alert
    assert any("tmux session gone" in a["display"] for a in out3)
    assert len(fp.dedup(st, out3, T0 + 360)[0]) == 2   # S03 and S04, both gone now


def test_dry_run_writes_nothing(tmp_path):
    put(tmp_path, "S03", name="tm-memory-01", state="idle")
    assert len(fp.exited_step({}, [], T0, dry_run=True, root=str(tmp_path))) == 1
    assert rec(tmp_path, "S03")["state"] == "idle"

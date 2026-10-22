"""pm-digest.py: grouping, the 20-line cap and last_wake advance."""
import importlib.util
import json
import os
import subprocess
import sys
import time
import calendar

def _scripts_dir():
    """SCRIPTS env, else this directory if pm_intake.py is here, else ../scripts."""
    env = os.environ.get("SCRIPTS")
    if env:
        return env
    here = os.path.dirname(os.path.abspath(__file__))
    if os.path.exists(os.path.join(here, "pm_intake.py")):
        return here
    return os.path.join(os.path.dirname(here), "scripts")


HERE = _scripts_dir()
spec = importlib.util.spec_from_file_location("pm_digest", os.path.join(HERE, "pm-digest.py"))
pd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pd)


def M(kind, session="tm-a", ts="2026-10-12T03:00:00Z", **kw):
    return {"ts": ts, "session": session, "kind": kind, "text": "t", **kw}


def test_groups_held_and_lists_wake_first():
    lines = pd.build([M("progress"), M("progress", text="CI pending"), M("ack", "tm-b"),
                      M("blocker", "tm-c", text="cap")], "02:41Z")
    assert lines[0] == "DIGEST since 02:41Z (3 held, 1 woke)"
    assert "WAKE blocker" in lines[1]
    assert 'held 2 progress (last: "CI pending")' in lines[2]
    assert len(lines) == 4


def test_cap_is_20_lines_and_all_lifts_it():
    msgs = [M("done", f"tm-{i}") for i in range(30)]
    lines = pd.build(msgs, "x")
    assert len(lines) == 20 and lines[-1] == "+12 more, run with --all"
    assert len(pd.build(msgs, "x", show_all=True)) == 31
    assert max(len(ln) for ln in pd.build([M("done", text="x" * 300)], "x")) <= 110


def _run(tmp_path, *args):
    env = {**os.environ, "SUPERVISOR_INBOX_DIR": str(tmp_path), "ARCHITECT_INBOX_DIR": str(tmp_path)}
    return subprocess.run([sys.executable, os.path.join(HERE, "pm-digest.py"), *args],
                          capture_output=True, text=True, env=env)


def test_advance_and_no_advance(tmp_path):
    (tmp_path / "pm-messages.jsonl").write_text(json.dumps(M("ack", ts="2020-01-01T00:00:00Z")) + "\n")
    r = _run(tmp_path, "--no-advance", "--since", "2019-01-01T00:00:00Z")
    assert "1 held" in r.stdout and not (tmp_path / "digest-state.json").exists()
    _run(tmp_path, "--since", "2019-01-01T00:00:00Z")
    assert "last_wake" in json.loads((tmp_path / "digest-state.json").read_text())
    assert "0 held" in _run(tmp_path).stdout  # now newer than every message


def _pd():
    spec = importlib.util.spec_from_file_location("pm_digest", os.path.join(HERE, "pm-digest.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_wake_digest_single_line_truncates_and_caps():
    pd = _pd()
    alerts = [{"session": "tm-a", "kind": "error", "detail": "x\ny" * 200}]
    msgs = [{"session": f"tm-{i}", "kind": "progress", "text": "t"} for i in range(9)]
    out = pd.wake_digest(alerts, msgs, "/p/alerts.jsonl")
    assert "\n" not in out and out.startswith("tm-a error: x y")
    parts = out.split(" | ")
    assert len(parts[0]) <= len("tm-a error: ") + 140
    assert len(parts) == 9 and parts[-1] == "+2 more -> /p/alerts.jsonl"
    assert pd.wake_digest([], [], "/p") == ""


def test_held_since_skips_wake_and_old(tmp_path):
    pd = _pd()
    f = tmp_path / "m.jsonl"
    rows = [M("progress", ts="2026-10-12T03:00:00Z"), M("progress", ts="2026-10-12T05:00:00Z"),
            M("decision", ts="2026-10-12T05:00:00Z")]
    f.write_text("\n".join(json.dumps(r) for r in rows))
    got = pd.held_since(calendar.timegm((2026, 10, 12, 4, 0, 0)), str(f))
    assert [m["kind"] for m in got] == ["progress"] and got[0]["ts"].endswith("05:00:00Z")


def test_parse_ts_is_utc_epoch_regardless_of_dst():
    assert pd.parse_ts("2026-10-12T04:00:00Z") == calendar.timegm((2026, 10, 12, 4, 0, 0))


def test_held_since_counts_recent_message_against_real_epoch_cutoff(tmp_path):
    f = tmp_path / "m.jsonl"
    ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 60))
    f.write_text(json.dumps(M("progress", ts=ts)) + "\n")
    assert len(pd.held_since(time.time() - 600, str(f))) == 1

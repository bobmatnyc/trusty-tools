"""pm_intake: classification, dedup, offset reading, the poll step and pm-notify.sh."""
import importlib.util
import json
import os
import subprocess

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


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


pi = _load("pm_intake", "pm_intake.py")
NOW = 1_791_600_000.0


def M(kind, **kw):
    return {"ts": "2026-10-12T03:00:00Z", "session": "tm-x-01", "kind": kind, "text": "t", **kw}


def test_kinds_wake_or_hold():
    cases = [
        (M("decision"), "wake"), (M("blocker"), "wake"), (M("done"), "wake"),
        (M("head", head="3f9a1c2d44"), "wake"),
        (M("ack"), "hold"), (M("progress"), "hold"), (M("fyi"), "hold"),
        (M("weird"), "wake"), ({"session": "s", "text": "t"}, "wake"),
    ]
    for msg, want in cases:
        assert pi.classify(msg)[0] == want, msg


def test_head_without_valid_sha_is_unknown_and_wakes():
    for head in (None, "", "abc", "zzzzzzzz"):
        decision, reason = pi.classify(M("head", head=head))
        assert decision == "wake" and "unknown" in reason


def test_urgent_wakes_a_held_kind():
    assert pi.classify(M("progress", urgent="prod"))[0] == "wake"


def test_held_text_asking_for_a_ruling_wakes():
    assert pi.classify(M("progress", text="Which option do you want?"))[0] == "wake"
    assert pi.classify(M("ack", text="please approve"))[0] == "wake"


def test_held_text_with_snapshot_path_wakes():
    snap = "Paused for refresh. Snapshot: /srv/example/.trusty-mpm/sessions/abc/session-20261014-005834.md done"
    assert pi.classify(M("progress", text=snap)) == ("wake", "snapshot ready for refresh")
    assert pi.classify(M("progress", text="Paused for refresh, no file yet"))[0] == "hold"
    assert pi.classify(M("fyi", text="see .trusty-mpm/sessions/abc/notes.md"))[0] == "hold"


def _write(path, *msgs):
    with open(path, "a") as f:
        for m in msgs:
            f.write(json.dumps(m) + "\n")


def test_read_new_offset_dedup_and_partial_line(tmp_path):
    p = tmp_path / "pm.jsonl"
    _write(p, M("done", pr=1), M("done", pr=1), M("done", pr=2))
    st = {}
    got = pi.read_new(st, str(p))
    assert [bool(m.get("dup")) for m, _ in got] == [False, True, False]
    assert pi.read_new(st, str(p)) == []
    with open(p, "a") as f:
        f.write('{"kind": "ack"')  # half-written line is left for the next read
    assert pi.read_new(st, str(p)) == []
    with open(p, "a") as f:
        f.write(', "session": "s"}\n')
    assert [m["kind"] for m, _ in pi.read_new(st, str(p))] == ["ack"]


def test_poll_step_wakes_and_logs_held(tmp_path, monkeypatch):
    monkeypatch.setenv("SUPERVISOR_INBOX_DIR", str(tmp_path))
    _write(pi.messages_path(), M("blocker"), M("progress"), M("done", pr=3), M("done", pr=3))
    mk = lambda kind, fp, detail, session, pane, now: {"kind": kind, "fingerprint": fp, "detail": detail}
    out = tmp_path / "triage.jsonl"
    alerts = pi.poll_step({}, mk, NOW, str(out))
    assert [a["kind"] for a in alerts] == ["pm_message", "pm_message"]
    held = [json.loads(x) for x in out.read_text().splitlines()]
    assert [h["reason"] for h in held] == ["progress", "duplicate within 30 min"]
    assert all(h["decision"] == "log" for h in held)


def test_poll_step_dry_run_writes_nothing(tmp_path, monkeypatch):
    monkeypatch.setenv("SUPERVISOR_INBOX_DIR", str(tmp_path))
    _write(pi.messages_path(), M("ack"))
    out = tmp_path / "triage.jsonl"
    pi.poll_step({}, None, NOW, str(out), dry_run=True)
    assert not out.exists()


def test_pm_notify_appends_one_valid_line(tmp_path):
    env = {**os.environ, "SUPERVISOR_INBOX_DIR": str(tmp_path)}
    r = subprocess.run([os.path.join(HERE, "pm-notify.sh"), "--kind", "head", "--session", "tm-a",
                        "--pr", "7", "--head", "abcdef1", "ready", "for", "review"],
                       capture_output=True, text=True, env=env)
    assert (r.returncode, r.stdout.strip()) == (0, "queued")
    (line,) = (tmp_path / "pm-messages.jsonl").read_text().splitlines()
    m = json.loads(line)
    assert (m["kind"], m["pr"], m["head"], m["text"]) == ("head", 7, "abcdef1", "ready for review")


def test_pm_notify_requires_kind(tmp_path):
    env = {**os.environ, "SUPERVISOR_INBOX_DIR": str(tmp_path)}
    r = subprocess.run([os.path.join(HERE, "pm-notify.sh"), "--session", "s", "x"],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 2 and not (tmp_path / "pm-messages.jsonl").exists()


def test_different_decision_texts_from_one_session_both_wake(tmp_path):
    p = tmp_path / "pm.jsonl"
    _write(p, M("decision", text="pick A or B"), M("decision", text="rotate the key now"))
    got = pi.read_new({}, str(p))
    assert [bool(m.get("dup")) for m, _ in got] == [False, False]


def test_poll_step_returns_alerts_when_triage_write_fails(tmp_path, monkeypatch):
    monkeypatch.setenv("SUPERVISOR_INBOX_DIR", str(tmp_path))
    _write(pi.messages_path(), M("blocker"), M("progress"))
    mk = lambda kind, fp, detail, session, pane, now: {"kind": kind, "fingerprint": fp, "detail": detail}
    bad = tmp_path / "no-such-dir" / "triage.jsonl"
    alerts = pi.poll_step({}, mk, NOW, str(bad))
    assert [a["kind"] for a in alerts] == ["pm_message"]

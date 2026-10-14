"""Pane ctx fallback: each pane resolves its own transcript, never a cwd-shared one."""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)

CWD_DIR = "-srv-x-trusty-tools"  # one project dir shared by every pane below


def _transcript(base, sid, tokens, mtime):
    d = base / CWD_DIR
    d.mkdir(parents=True, exist_ok=True)
    p = d / f"{sid}.jsonl"
    p.write_text(json.dumps({"type": "assistant",
                             "message": {"usage": {"input_tokens": tokens}}}) + "\n")
    os.utime(p, (mtime, mtime))


def _session(sdir, pid, sid, pane):
    sdir.mkdir(exist_ok=True)
    (sdir / f"{pid}.json").write_text(json.dumps(
        {"pid": pid, "sessionId": sid, "tmux": f"tm-x:@1.{pane}", "updatedAt": 1}))


def test_same_cwd_panes_get_their_own_ctx(tmp_path, monkeypatch):
    base, sdir = tmp_path / "projects", tmp_path / "sessions"
    mypid = os.getpid()  # a live pid, so the registry entry counts as alive
    _transcript(base, "sid-a", 450_000, 2_000)  # newer: the old code picked this for both
    _transcript(base, "sid-b", 180_000, 1_000)
    _session(sdir, mypid, "sid-a", "%1")
    _session(sdir, mypid + 1_000_000, "sid-b", "%2")  # dead pid: ignored
    monkeypatch.setattr(fp, "TRANSCRIPT_BASE", str(base))
    monkeypatch.setattr(fp, "SESSIONS_DIR", str(sdir))
    monkeypatch.setattr(fp, "SELF_CTX_WINDOW_DEFAULT", 1_000_000)
    fp._transcript_cache.clear()
    assert fp.transcript_ctx_pct("%1") == 45
    assert fp.transcript_ctx_pct("%2") is None  # unresolved: no borrowed value
    _session(sdir, 1, "sid-b", "%2")  # pid 1 is always alive
    assert fp.transcript_ctx_pct("%2") == 18
    assert fp.transcript_ctx_pct("%9") is None


def test_ctx_thresholds_follow_ruling_b7():
    assert (fp.CTX_WARN, fp.CTX_HIGH) == (22, 25)
    assert fp.SELF_CTX_THRESHOLD_DEFAULT == 25

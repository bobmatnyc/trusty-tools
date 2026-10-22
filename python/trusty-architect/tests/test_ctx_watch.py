"""ctx-watch.py: signals S1-S5 fire at their thresholds and not below (SPEC-IL-01 section 7)."""
import importlib.util
import json
import os

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
_spec = importlib.util.spec_from_file_location("ctx_watch", os.path.join(SCRIPTS, "ctx-watch.py"))
cw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cw)

NOW = cw.parse_iso("2026-10-17T06:00:00Z")
NAME = "tm-x"


def build(tmp, ctx, age, il=None, text=None, task_state=True):
    """Fake registry + transcript (last assistant turn `age` s ago) + IL state.json; paths dict."""
    sessions, base, proj = tmp / "sessions", tmp / "projects", tmp / "proj"
    sessions.mkdir(exist_ok=True)
    (base / "-p").mkdir(parents=True, exist_ok=True)
    (sessions / "1.json").write_text(json.dumps(
        {"pid": os.getpid(), "sessionId": "sid1", "tmux": f"{NAME}:@1.%1", "updatedAt": 1}))
    lines = [{"type": "assistant", "timestamp": cw.iso(NOW - age - 3000),
              "message": {"usage": {"input_tokens": 83_000}}}]
    if text:
        lines.append({"type": "assistant", "timestamp": cw.iso(NOW - age - 5),
                      "message": {"content": [{"type": "text", "text": text}],
                                  "usage": {"input_tokens": 1}}})
    lines.append({"type": "assistant", "timestamp": cw.iso(NOW - age),
                  "message": {"usage": {"input_tokens": ctx - 20_000, "cache_read_input_tokens": 15_000,
                                        "cache_creation_input_tokens": 5_000}}})
    (base / "-p" / "sid1.jsonl").write_text("\n".join(json.dumps(x) for x in lines) + "\n")
    d = proj / ".trusty-mpm" / "datg"
    d.mkdir(parents=True, exist_ok=True)
    (d / "state.json").write_text(json.dumps({"session": il or {}}))
    return {"sessions_dir": str(sessions), "transcripts": str(base),
            "cycle_jsonl": str(tmp / "cycle.jsonl"), "proxy": str(tmp / "proxy.jsonl")}, str(proj)


def fire(tmp, ctx, age, il=None, text=None, state=None, signals=None):
    p, proj = build(tmp, ctx, age, il, text)
    entry = {"name": NAME, "project": proj}
    if signals:
        entry["signals"] = signals
    sched = {"sessions": [entry]}
    st = {} if state is None else state
    cands, rows = cw.evaluate(sched, NOW, st, p=p)
    return {c["signal"]: c for c in cands}, rows, st


def il_at_boundary(age, stale=80, cu_age_after=5, **extra):
    """IL whose boundary is just before the last turn (turn `age` s ago) with a valid self-report."""
    b = NOW - age - 20
    d = {"boundary_at": cw.iso(b), "ctx_use": {"at": cw.iso(b + cu_age_after), "active": 100 - stale,
                                                "stale": stale, "ctx": 160_000, "keep": "x"}}
    d.update(extra)
    return d


@pytest.mark.parametrize("ctx,stale,fires", [
    (150_000, 70, True), (149_999, 70, False), (150_000, 69, False), (260_000, 95, True)])
def test_s1_thresholds(tmp_path, ctx, stale, fires):
    got, _, _ = fire(tmp_path, ctx, 30, il_at_boundary(30, stale=stale), signals=["S1"])
    assert ("S1" in got) is fires


def test_s1_self_report_older_than_boundary_is_ignored(tmp_path):
    il = il_at_boundary(30)
    il["ctx_use"]["at"] = cw.iso(NOW - 30 - 20 - 600)  # report predates boundary_at
    got, _, _ = fire(tmp_path, 200_000, 30, il, signals=["S1"])
    assert "S1" not in got


def test_s1_needs_a_boundary_not_mid_task(tmp_path):
    il = il_at_boundary(30)
    il["boundary_at"] = cw.iso(NOW - 30 - 900)  # the PM kept working 15 min after the boundary
    il["ctx_use"]["at"] = il["boundary_at"]
    got, _, _ = fire(tmp_path, 200_000, 30, il, signals=["S1"])
    assert "S1" not in got


def test_s1_uses_transcript_ctx_use_line_when_il_has_none(tmp_path):
    text = "done. CTX-USE: active ~20% · stale ~80% · keep: PR 12 review"
    il = {"boundary_at": cw.iso(NOW - 60)}
    got, _, _ = fire(tmp_path, 200_000, 30, il, text=text, signals=["S1"])
    assert "S1" in got


def test_s1_not_repeated_after_a_successful_clear(tmp_path):
    p, proj = build(tmp_path, 200_000, 30, il_at_boundary(30))
    (tmp_path / "cycle.jsonl").write_text(json.dumps(
        {"session": NAME, "exit": 0, "live": True, "ts": cw.iso(NOW - 10)}) + "\n")
    cands, _ = cw.evaluate({"sessions": [{"name": NAME, "project": proj, "signals": ["S1"]}]}, NOW, {}, p=p)
    assert cands == []


@pytest.mark.parametrize("ctx,action", [(250_000, "clear"), (220_000, "ask-boundary"), (219_999, None)])
def test_s2_bar_and_approach(tmp_path, ctx, action):
    got, _, _ = fire(tmp_path, ctx, 30, il_at_boundary(30, stale=10), signals=["S2"])
    assert (got["S2"]["action"] if "S2" in got else None) == action


def test_s2_debounce_once_per_15_min(tmp_path):
    st = {}
    got, _, st = fire(tmp_path, 260_000, 30, il_at_boundary(30), state=st, signals=["S2"])
    assert "S2" in got
    p, proj = build(tmp_path, 260_000, 30, il_at_boundary(30))
    sched = {"sessions": [{"name": NAME, "project": proj, "signals": ["S2"]}]}
    assert cw.evaluate(sched, NOW + 300, st, p=p)[0] == []
    assert cw.evaluate(sched, NOW + 901, st, p=p)[0] != []


@pytest.mark.parametrize("age,fires", [(599, False), (600, True), (3000, True), (3001, False)])
def test_s3_idle_window(tmp_path, age, fires):
    il = {"boundary_at": cw.iso(NOW - age - 20)}
    got, _, _ = fire(tmp_path, 160_000, age, il, signals=["S3"])
    assert ("S3" in got) is fires


def test_s3_not_with_running_agents_or_below_floor(tmp_path):
    il = {"boundary_at": cw.iso(NOW - 1000 - 20), "agents": [{"id": "a", "goal": "g", "what": "w"}]}
    assert "S3" not in fire(tmp_path, 160_000, 1000, il, signals=["S3"])[0]
    il = {"boundary_at": cw.iso(NOW - 1000 - 20)}
    assert "S3" not in fire(tmp_path, 149_000, 1000, il, signals=["S3"])[0]


def test_s4_once_per_session_id_and_not_with_open_task(tmp_path):
    st = {}
    got, _, st = fire(tmp_path, 160_000, 3601, {}, state=st, signals=["S4"])
    assert "S4" in got
    assert "S4" not in fire(tmp_path, 160_000, 3601, {}, state=st, signals=["S4"])[0]
    (tmp_path / "b").mkdir()
    (tmp_path / "c").mkdir()
    assert "S4" not in fire(tmp_path / "b", 160_000, 3601, {"task": "df-T04 busy"}, signals=["S4"])[0]
    assert "S4" not in fire(tmp_path / "c", 160_000, 3600, {}, signals=["S4"])[0]


@pytest.mark.parametrize("ctx,fires", [(400_000, True), (399_999, False)])
def test_s5_guard(tmp_path, ctx, fires):
    assert ("S5" in fire(tmp_path, ctx, 30, {}, signals=["S5"])[0]) is fires


def test_s5_reports_instead_of_clearing_with_agents(tmp_path):
    got, _, _ = fire(tmp_path, 420_000, 30, {"agents": [{"id": "a", "goal": "g", "what": "w"}]},
                     signals=["S5"])
    assert got["S5"]["action"] == "report"


def test_proxy_row_is_logged_and_never_a_trigger(tmp_path, monkeypatch):
    p, proj = build(tmp_path, 120_000, 30, il_at_boundary(30, stale=95))
    monkeypatch.setenv("IL_SCHEDULE", str(tmp_path / "s.json"))
    (tmp_path / "s.json").write_text(json.dumps({"sessions": [{"name": NAME, "project": proj}]}))
    monkeypatch.setenv("SESSIONS_DIR", p["sessions_dir"])
    monkeypatch.setenv("TRANSCRIPT_BASE", p["transcripts"])
    monkeypatch.setenv("CTX_PROXY_LOG", p["proxy"])
    monkeypatch.setenv("SUPERVISOR_INBOX_DIR", str(tmp_path / "inbox"))
    assert cw.step(now=NOW) == []  # ctx below the floor: stale 95 alone triggers nothing
    row = json.loads(open(p["proxy"]).read().splitlines()[0])
    assert row["session"] == NAME and row["reported_stale"] == 95 and "proxy_stale" in row

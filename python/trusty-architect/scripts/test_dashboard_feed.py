"""dashboard_feed.py on fixture transcripts (synthetic). Covers the 120 s active/parked
edge, finished agents, the session-id mapping with two sessions on one path, and the
1h and 24h token windows (which must equal token_watch.report)."""
import importlib.util
import json
import os
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("dashboard_feed", os.path.join(HERE, "dashboard_feed.py"))
df = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(df)
tw = df.tw

NOW = datetime(2026, 10, 8, 15, 30, 0, tzinfo=timezone.utc).timestamp()
PROJ = "-srv-x-trusty-tools"
SID_A, SID_B = "aaaaaaaa-0000", "bbbbbbbb-0000"


def asst(stop):
    return json.dumps({"type": "assistant", "message": {"stop_reason": stop}}) + "\n"


def agent(tmp, sid, aid, age, stop="tool_use", tail_attachment=True):
    p = tmp / "projects" / PROJ / sid / "subagents" / f"agent-{aid}.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(asst(stop) + ('{"type": "attachment"}\n' if tail_attachment else ""))
    os.utime(p, (NOW - age, NOW - age))


def hour_row(hours_ago, sid, out, units):
    h = tw.hour_of(NOW - hours_ago * 3600)
    return {"hour": h, "project": PROJ, "session": sid, "agent_type": "main", "model": "claude-opus-5",
            "out_tokens": out, "units": units}


def feed(tmp, rows=(), names=None, state=None):
    hp, sp = tmp / "hourly.jsonl", tmp / "state.json"
    hp.write_text("".join(json.dumps(r) + "\n" for r in rows))
    sp.write_text(json.dumps(state or tw.new_state()))
    return df.build_feed(NOW, base=str(tmp / "projects"),
                         names={SID_A: "tm-a", SID_B: "tm-b"} if names is None else names,
                         state_path=str(sp), hourly_path=str(hp))


def test_active_parked_edge_and_finished(tmp_path):
    agent(tmp_path, SID_A, "e120", 120)                      # exactly 120 s: active
    agent(tmp_path, SID_A, "e121", 121)                      # 121 s: parked
    agent(tmp_path, SID_A, "fresh", 5)                       # active
    agent(tmp_path, SID_A, "done", 5, stop="end_turn")       # finished: neither
    agent(tmp_path, SID_A, "old", 7 * 3600)                  # abandoned: neither
    u = feed(tmp_path)["sessions"]["tm-a"]
    assert (u["agents_active"], u["agents_parked"]) == (2, 1)


def test_two_sessions_one_path_map_by_session_id(tmp_path):
    agent(tmp_path, SID_A, "a1", 10)
    agent(tmp_path, SID_B, "b1", 600)
    agent(tmp_path, SID_B, "b2", 700)
    rows = [hour_row(0, SID_A, 100, 1.5), hour_row(0, SID_B, 7000, 2.0)]
    st = tw.new_state()
    st["files"] = {"p1": {"kind": "main", "session": SID_A, "last_ts": 1, "recent": [[100000, 20000, 0]]},
                   "p2": {"kind": "main", "session": SID_B, "last_ts": 1, "recent": [[1, 2, 3], [40000, 0, 0]]}}
    out = feed(tmp_path, rows, state=st)
    a, b = out["sessions"]["tm-a"], out["sessions"]["tm-b"]
    assert (a["agents_active"], a["agents_parked"], a["out_tokens_1h"], a["ctx_k"]) == (1, 0, 100, 120.0)
    assert (b["agents_active"], b["agents_parked"], b["out_tokens_1h"], b["ctx_k"]) == (0, 2, 7000, 40.0)
    assert out["fleet"]["out_tokens_1h"] == 7100 and out["fleet"]["agents_parked"] == 2


def test_windows_match_report_and_unmapped_counts_in_fleet(tmp_path):
    rows = [hour_row(0, SID_A, 10, 1.0), hour_row(1, SID_A, 20, 2.0),
            hour_row(5, SID_A, 400, 4.0), hour_row(30, SID_A, 8000, 8.0),
            hour_row(0, "cccccccc-0000", 5, 0.5)]
    out = feed(tmp_path, rows)
    a = out["sessions"]["tm-a"]
    assert a["out_tokens_1h"] == 30            # current and previous hour bucket, as report --since 1h
    assert a["out_tokens_24h"] == 430 and a["units_24h"] == 7.0   # 30 h ago excluded
    assert "cccccccc-0000" not in out["sessions"]
    assert out["fleet"]["out_tokens_1h"] == 35
    want = sum(r["out_tokens"] for r in tw.report(3600, str(tmp_path / "hourly.jsonl"),
                                                  str(tmp_path / "state.json"), NOW))
    assert want == out["fleet"]["out_tokens_1h"]


def test_unreadable_inputs_give_zero_feed(tmp_path):
    out = df.build_feed(NOW, base=str(tmp_path / "none"), names={SID_A: "tm-a"},
                        state_path=str(tmp_path / "none.json"), hourly_path=str(tmp_path / "none.jsonl"))
    assert out["sessions"]["tm-a"]["agents_active"] == 0 and out["fleet"]["out_tokens_24h"] == 0
    assert out["fleet"]["measured_at"] == "2026-10-08T15:30:00Z"


def test_live_state_is_added_next_to_usage(tmp_path):
    hp, sp = tmp_path / "h.jsonl", tmp_path / "s.json"
    hp.write_text("")
    sp.write_text(json.dumps(tw.new_state()))
    out = df.build_feed(NOW, base=str(tmp_path / "p"), names={SID_A: "tm-a"}, state_path=str(sp),
                        hourly_path=str(hp), live={"tm-a": {"state": "waiting", "idle_min": 7}})
    assert (out["sessions"]["tm-a"]["live_state"], out["sessions"]["tm-a"]["idle_min"]) == ("waiting", 7)


def test_accounts_latest_line_per_account_written_once(tmp_path):
    ledger = tmp_path / "ledger.jsonl"
    lines = [
        {"ts": "2026-10-09T01:00:00Z", "account": "a@example.com", "pct5h": 10, "reset5h": "r", "pctwk": 50, "resetwk": "w", "source": "s", "weekly_models": {}},
        {"ts": "2026-10-09T02:00:00Z", "account": "a@example.com", "pct5h": 20, "reset5h": "r", "pctwk": 51, "resetwk": "w", "source": "s"},
        {"ts": "2026-10-09T01:30:00Z", "account": "B.c@example.io", "pct5h": 0, "reset5h": None, "pctwk": 90, "resetwk": "w2", "source": "manual"},
    ]
    ledger.write_text("\n".join(json.dumps(r) for r in lines) + "\nnot json\n" + json.dumps({"ts": "t"}) + "\n")
    acc = df.latest_accounts(str(ledger))
    assert sorted(acc) == ["a-example-com", "b-c-example-io"]                 # one doc per account, count follows the data
    assert acc["a-example-com"]["pct5h"] == 20 and acc["b-c-example-io"]["reset5h"] is None
    root = tmp_path / "store"
    assert sorted(df.write_accounts(str(root), acc)) == ["a-example-com", "b-c-example-io"]
    assert df.write_accounts(str(root), acc) == []                # unchanged: no rewrite, no version bump
    assert json.loads((root / "accounts" / "a-example-com.json").read_text())["pct5h"] == 20
    assert df.latest_accounts(str(tmp_path / "missing.jsonl")) == {}

"""fleet-poll.py dashboard feed apply (usage field only, version-pinned, conflict skip,
alert on failure) and the pane_live classification."""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("fleet_poll", os.path.join(HERE, "fleet-poll.py"))
fp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fp)
df = fp.dashboard_feed
ds = df.ds


def feed(parked=0, state="idle", idle_min=0, out=1000, stamp="2026-10-08T15:00:00Z"):
    u = {"agents_active": 0, "agents_parked": parked, "out_tokens_1h": out, "out_tokens_24h": out,
         "units_24h": 1.0, "ctx_k": 100.0, "measured_at": stamp, "live_state": state, "idle_min": idle_min}
    return {"sessions": {"tm-a": u}, "fleet": {"agents_active": 0, "agents_parked": parked,
            "out_tokens_1h": out, "out_tokens_24h": out, "units_24h": 1.0, "measured_at": stamp}}


def make_store(tmp_path, names=("tm-a", "tm-b"), usage=None):
    st = ds.Store(str(tmp_path))
    for i, n in enumerate(names):
        st.set("sessions", f"S{i}", {"name": n, "goal": "G01", "usage": usage or {"old": 1}})
    st.set("meta", "state", {"owner_name": "Owner", "fleet_usage": {"old": 1}, "updated": "t0"})
    return st


def two_row_feed(**kw):
    f = feed(**kw)
    f["sessions"]["tm-b"] = dict(f["sessions"]["tm-a"], agents_active=2)
    return f


def test_apply_writes_changed_rows_usage_only(tmp_path):
    st = make_store(tmp_path)
    f = two_row_feed()
    res = df.apply_feed(f, str(tmp_path))
    assert sorted(res["wrote"]) == ["S0", "S1", "meta/state"] and res["errors"] == []
    a = st.get("sessions", "S0")
    assert a["data"]["usage"] == f["sessions"]["tm-a"] and a["version"] == 2
    assert {k: v for k, v in a["data"].items() if k != "usage"} == {"name": "tm-a", "goal": "G01"}
    m = st.get("meta", "state")["data"]
    assert m["fleet_usage"] == f["fleet"] and m["owner_name"] == "Owner" and m["updated"] == "t0"


def test_apply_unchanged_rows_write_nothing(tmp_path):
    st = make_store(tmp_path)
    f = two_row_feed()
    df.apply_feed(f, str(tmp_path))
    # measured_at and idle_min move every cycle; they alone are not a change
    g = two_row_feed(stamp="2026-10-08T16:00:00Z", idle_min=42)
    g["sessions"]["tm-b"]["idle_min"] = 7
    res = df.apply_feed(g, str(tmp_path))
    assert res == {"wrote": [], "conflicts": [], "errors": []}
    assert st.get("sessions", "S0")["version"] == 2
    h = two_row_feed(out=2000)                      # a token count moved: written
    assert "S0" in df.apply_feed(h, str(tmp_path))["wrote"]


def test_apply_version_conflict_skips_row_and_retries_next_poll(tmp_path, monkeypatch):
    st = make_store(tmp_path, names=("tm-a",))
    real = ds.Store.update

    def racing(self, coll, doc_id, data, if_version=None):
        if coll == "sessions":       # someone else writes between our read and our update
            real(self, coll, doc_id, {"summary": "theirs"})
        return real(self, coll, doc_id, data, if_version=if_version)
    monkeypatch.setattr(ds.Store, "update", racing)
    res = df.apply_feed(feed(), str(tmp_path))
    assert res["conflicts"] == ["S0"] and "S0" not in res["wrote"] and res["errors"] == []
    doc = st.get("sessions", "S0")["data"]
    assert doc["usage"] == {"old": 1} and doc["summary"] == "theirs"
    monkeypatch.setattr(ds.Store, "update", real)
    assert "S0" in df.apply_feed(feed(), str(tmp_path))["wrote"]


def test_apply_skips_rows_without_a_doc_and_docs_without_a_row(tmp_path):
    make_store(tmp_path, names=("tm-zzz",))
    res = df.apply_feed(feed(), str(tmp_path))      # feed has tm-a only; doc is tm-zzz
    assert res["wrote"] == ["meta/state"] and res["errors"] == []


def test_apply_error_is_reported_not_raised(tmp_path):
    make_store(tmp_path, names=("tm-a",))
    (tmp_path / "sessions" / "S9.json").write_text("{not json")
    res = df.apply_feed(feed(), str(tmp_path))
    assert res["errors"] and "S9" in res["errors"][0]


def test_apply_step_quiet_on_success_alerts_on_failure(tmp_path, monkeypatch):
    make_store(tmp_path, names=("tm-a",))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    assert fp.dashboard_apply(feed(), 1000, root=str(tmp_path)) is None      # applied: no alert
    (tmp_path / "sessions" / "S9.json").write_text("{not json")
    a = fp.dashboard_apply(feed(parked=3), 1100, root=str(tmp_path))
    assert a["kind"] == "dashboard" and "S9" in a["detail"] and a["fingerprint"]
    b = fp.dashboard_apply(feed(parked=4), 1200, root=str(tmp_path))
    assert b["fingerprint"] == a["fingerprint"]                              # one alert per failure
    boom = fp.dashboard_apply(feed(), 1300, root=str(tmp_path / "missing" / "x" / "\0"))
    assert boom and boom["kind"] == "dashboard"                              # even a crash alerts


def test_pane_live_classification():
    base = {"dialog": None, "question": None, "busy": False}
    assert fp.pane_live(dict(base, busy=True), {}, 0)["state"] == "working"
    assert fp.pane_live(dict(base, question="q?"), {}, 0)["state"] == "waiting"
    assert fp.pane_live(dict(base, dialog=("n", "d")), {}, 0)["state"] == "waiting"
    assert fp.pane_live(base, {"idle_since": 0}, 780) == {"state": "idle", "idle_min": 13}


def test_feeder_failure_returns_none(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "INBOX", str(tmp_path))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))

    def boom(*a, **k):
        raise TimeoutError("slow")
    assert fp.run_dashboard_feeder({}, runner=boom) is None


PANES = [{"session": "tm-a", "pane": "%1", "cmd": "claude", "active": "1"}]
SCREEN = "⏺ Done.\n\n──\n❯ \n──\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n"


def patch_cycle(monkeypatch, alerts_path, feeder, apply):
    for name, val in (("list_panes", lambda: PANES), ("capture", lambda pid: SCREEN),
                      ("rename_windows", lambda dry=False: 0),
                      ("watchdog_alerts", lambda *a, **k: ([], "ok")),
                      ("usage_alerts", lambda *a, **k: []), ("token_alerts", lambda *a, **k: []),
                      ("read_events", lambda s: []), ("host_alerts", lambda *a, **k: []),
                      ("self_ctx_alerts", lambda *a, **k: []), ("log", lambda *a, **k: None),
                      ("dispatch_wake", lambda *a: "x"),
                      ("run_dashboard_feeder", feeder), ("dashboard_apply", apply)):
        monkeypatch.setattr(fp, name, val)
    monkeypatch.setattr(fp, "ALERTS", str(alerts_path))


def alert_kinds(path):
    return [json.loads(ln)["kind"] for ln in open(path) if ln.strip()] if os.path.exists(path) else []


def test_cycle_hands_feeder_the_live_dict_not_a_set(tmp_path, monkeypatch):
    # cycle() once rebound `live` to a set of pane keys before the feeder call, so
    # json.dump raised "Object of type set is not JSON serializable" every cycle.
    seen = []
    patch_cycle(monkeypatch, os.devnull, lambda live: seen.append(live), lambda *a, **k: None)
    fp.cycle({}, now=5000)
    assert isinstance(seen[0], dict) and seen[0]["tm-a"]["state"] == "idle"
    json.dumps(seen[0])


def test_cycle_applies_feed_and_raises_dashboard_alert_only_on_failure(tmp_path, monkeypatch):
    applied, out = [], tmp_path / "alerts.jsonl"
    patch_cycle(monkeypatch, out, lambda live: feed(), lambda f, now, root=None: applied.append(f) or None)
    fp.cycle({}, now=5000)
    assert applied and "dashboard" not in alert_kinds(out)       # applied: no Architect turn
    patch_cycle(monkeypatch, out, lambda live: feed(),
                lambda f, now, root=None: fp.alert("dashboard", "fp1", "feed apply failed: boom", now=now))
    fp.cycle({}, now=6000)
    assert "dashboard" in alert_kinds(out)


def test_feeder_writes_live_dict_json(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "INBOX", str(tmp_path))
    monkeypatch.setattr(fp, "DASHBOARD_FEED", str(tmp_path / "feed.json"))
    (tmp_path / "feed.json").write_text("{}")
    live = {"tm-a": {"state": "idle", "idle_min": 3}}
    assert fp.run_dashboard_feeder(live, runner=lambda *a, **k: None) == {}
    assert json.load(open(tmp_path / "dashboard-live.json")) == live

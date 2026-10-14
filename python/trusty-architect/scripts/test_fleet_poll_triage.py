"""Alert triage: deterministic rules, hold expiry, the batched Haiku gate and its fail-open."""
import importlib.util
import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


at = _load("alert_triage", "alert_triage.py")
fp = _load("fleet_poll", "fleet-poll.py")

NOW = 1_791_600_000.0  # 2026-10-10T02:40:00Z
FUTURE = "2099-01-01T00:00:00Z"
PAST = "2020-01-01T00:00:00Z"


def A(kind, session="tm-x-01", fingerprint="f", detail="d"):
    return {"ts": "2026-10-09T20:00:00Z", "session": session, "pane": "%1", "kind": kind,
            "fingerprint": fingerprint, "detail": detail, "display": f"[{session}]: {kind}: {detail}"}


def rules(*holds):
    return {"holds": [dict({"session": "x-01", "reason": "waits A1", "until": FUTURE,
                            "set_by": "architect", "set_at": "2026-10-09T20:00:00Z"}, **h)
                      for h in holds]}


def decide(alert, r=None, rc=None):
    return at.decide(alert, r or rules(), NOW, rc if rc is not None else {})


# --- deterministic rules ---------------------------------------------------------

def test_always_wake_kinds():
    for kind in ("owner_answer", "owner_feedback", "owner_unpark_due", "status_request",
                 "permission_prompt", "dialog", "question", "exited", "session_exited",
                 "self_ctx", "band", "architect_dead", "architect_frozen"):
        d = decide(A(kind))
        assert d["decision"] == "wake" and d["path"].startswith("rule:"), kind


def test_ctx_at_bar_wakes_below_logs():
    assert decide(A("ctx", detail="ctx 25%"))["decision"] == "wake"
    assert decide(A("ctx", detail="ctx 88%"))["decision"] == "wake"
    d = decide(A("ctx", detail="ctx 23%"))
    assert d["decision"] == "log" and d["path"].startswith("rule:")


def test_token_ctx_main_bar_is_250k():
    hi = A("token", fingerprint="ctx_main:300k", detail="ctx_main: ctx 300062 >= 300000 main opus")
    lo = A("token", fingerprint="ctx_main:200k", detail="ctx_main: ctx 206908 >= 200000 main opus")
    assert decide(hi)["decision"] == "wake"
    assert decide(lo)["decision"] == "log"


def test_token_info_kinds_log_others_ask():
    for k in ("hit_low", "miss_idle", "miss_unexpected", "ctx_sub"):
        assert decide(A("token", fingerprint=f"{k}:x", detail=f"{k}: whatever"))["decision"] == "log", k
    assert decide(A("token", fingerprint="burn:1", detail="burn: fleet"))["decision"] == "ask_haiku"


def test_rc_restored_logs_and_resets_failures():
    rc = {"tm-x-01": 2}
    assert decide(A("rc_restored"), rc=rc)["decision"] == "log"
    assert rc == {}


def test_rc_failed_three_in_a_row_wakes():
    rc = {}
    assert decide(A("rc_failed"), rc=rc)["decision"] == "ask_haiku"
    assert decide(A("rc_failed"), rc=rc)["decision"] == "ask_haiku"
    assert decide(A("rc_failed"), rc=rc)["decision"] == "wake"
    assert decide(A("rc_failed", session="tm-other"), rc=rc)["decision"] == "ask_haiku"


def test_idle_with_active_hold_logs_with_hold_reason():
    d = decide(A("idle"), rules({}))
    assert d["decision"] == "log" and "waits A1" in d["reason"]


def test_idle_hold_matches_session_with_or_without_tm_prefix():
    assert decide(A("idle", session="tm-x-01"), rules({"session": "tm-x-01"}))["decision"] == "log"


def test_idle_without_hold_asks_haiku():
    assert decide(A("idle", session="tm-y-01"), rules({}))["decision"] == "ask_haiku"


def test_idle_with_expired_hold_asks_haiku():
    assert decide(A("idle"), rules({"until": PAST}))["decision"] == "ask_haiku"


def test_hold_until_event_does_not_expire():
    h = {"until": None, "until_event": "A34 answered"}
    assert decide(A("idle"), rules(h))["decision"] == "log"


def test_hold_expiry_boundary():
    assert at.hold_active({"until": "2026-10-10T02:40:00Z"}, NOW) is False
    assert at.hold_active({"until": "2026-10-10T02:40:01Z"}, NOW) is True


def test_ctx_alert_with_ctx_hold_logs_with_hold_reason():
    d = decide(A("ctx", detail="ctx 88%"), rules({"kinds": ["idle", "ctx"]}))
    assert d["decision"] == "log" and d["path"] == "rule:ctx_hold" and "waits A1" in d["reason"]


def test_ctx_alert_with_hold_lacking_kinds_still_wakes():
    assert decide(A("ctx", detail="ctx 88%"), rules({}))["decision"] == "wake"


def test_idle_alert_with_hold_lacking_kinds_still_logs():
    assert decide(A("idle"), rules({}))["decision"] == "log"


def test_expired_ctx_hold_wakes():
    d = decide(A("ctx", detail="ctx 88%"), rules({"kinds": ["idle", "ctx"], "until": PAST}))
    assert d["decision"] == "wake" and d["path"] == "rule:ctx_at_bar"


def test_unknown_kind_asks_haiku():
    assert decide(A("error"))["decision"] == "ask_haiku"


def test_load_rules_missing_or_bad_file_is_empty(tmp_path):
    assert at.load_rules(str(tmp_path / "nope.json"))["holds"] == []
    bad = tmp_path / "bad.json"
    bad.write_text("{not json")
    assert at.load_rules(str(bad))["holds"] == []


def test_seed_rules_file_loads_with_all_holds():
    # #8436: a neutral copy of an installation's local/triage-rules.json.
    r = at.load_rules(os.path.join(HERE, "fixtures", "triage-rules.json"))
    sessions = {h["session"] for h in r["holds"]}
    for s in ("alpha-01", "beta", "beta-companion", "gamma-01", "delta-01", "epsilon",
              "epsilon-ops-01", "utils-01"):
        assert s in sessions, s
    assert all(h["reason"] and h["set_by"] and h["set_at"] and (h.get("until") or h.get("until_event"))
               for h in r["holds"])


# --- Haiku gate ----------------------------------------------------------------------

def test_parse_haiku_valid_and_fenced():
    out = 'sure\n```json\n[{"id": "a1", "investigate": true, "reason": "stuck"}]\n```'
    assert at.parse_haiku(out) == {"a1": (True, "stuck")}


def test_parse_haiku_rejects_bad_shapes():
    for bad in ("", "no json", "[{\"id\": \"a1\"}]", "[{\"id\": \"a1\", \"investigate\": \"yes\", \"reason\": \"r\"}]"):
        try:
            at.parse_haiku(bad)
        except ValueError:
            continue
        raise AssertionError(f"accepted {bad!r}")


def _triage(alerts, runner, r=None, panes=None):
    calls = []

    def pane(session):
        calls.append(session)
        return f"pane of {session}"
    recs = at.triage(alerts, r or rules(), NOW, {}, runner=runner, pane_fn=pane)
    return recs, calls


def test_one_batched_call_for_all_ask_haiku_alerts():
    prompts = []

    def runner(prompt):
        prompts.append(prompt)
        return json.dumps([{"id": "a1", "investigate": False, "reason": "known"},
                           {"id": "a2", "investigate": True, "reason": "stuck"}])
    alerts = [A("idle", session="tm-a"), A("error", session="tm-b"), A("rc_restored")]
    recs, panes = _triage(alerts, runner)
    assert len(prompts) == 1
    assert "pane of tm-a" in prompts[0] and "pane of tm-b" in prompts[0]
    assert [r["decision"] for r in recs] == ["log", "wake", "log"]
    assert recs[0]["path"] == "haiku" and recs[1]["reason"] == "stuck"
    assert recs[2]["path"].startswith("rule:")


def test_no_ask_alerts_means_no_call():
    def runner(prompt):
        raise AssertionError("must not be called")
    recs, _ = _triage([A("rc_restored"), A("owner_answer")], runner)
    assert [r["decision"] for r in recs] == ["log", "wake"]


def test_prompt_carries_hold_entry_and_redacts_secrets():
    seen = []

    def runner(prompt):
        seen.append(prompt)
        return json.dumps([{"id": "a1", "investigate": False, "reason": "r"}])
    fake_gh = "ghp_" + "abcdefghijklmnopqrstuvwxyz0123456789"  # built here: no token shape in the source
    a = A("idle", detail="mail owner@example.net token " + fake_gh)
    r = rules({"until": PAST, "reason": "expired hold text"})
    at.triage([a], r, NOW, {}, runner=runner, pane_fn=lambda s: "key sk-" + "ant-abcdefghijklmnopqrstu")
    p = seen[0]
    assert "expired hold text" in p
    assert "owner@example.net" not in p and "ghp_abc" not in p and "sk-ant-abc" not in p


def test_fail_open_on_bad_json_timeout_and_nonzero_exit():
    def bad(prompt):
        return "not json"

    def timeout(prompt):
        raise subprocess.TimeoutExpired("claude", 90)

    def nonzero(prompt):
        raise RuntimeError("claude exited 1")
    for runner in (bad, timeout, nonzero):
        recs, _ = _triage([A("idle", session="tm-a"), A("error")], runner)
        assert [r["decision"] for r in recs] == ["wake", "wake"]
        assert all(r["path"] == "haiku" and "fail-open" in r["reason"] for r in recs)


def _fake_run(calls, returncode=0, stdout="OK", stderr=""):
    def run(cmd, **kw):
        calls.append(kw)
        return subprocess.CompletedProcess(cmd, returncode, stdout, stderr)
    return run


def test_run_haiku_sets_config_dir_when_unset(monkeypatch):
    monkeypatch.delenv("CLAUDE_CONFIG_DIR", raising=False)
    calls = []
    monkeypatch.setattr(at.subprocess, "run", _fake_run(calls))
    assert at.run_haiku("p") == "OK"
    assert calls[0]["env"]["CLAUDE_CONFIG_DIR"] == at.CLAUDE_CONFIG_DIR
    assert at.CLAUDE_CONFIG_DIR.endswith(".trusty-tools/trusty-mpm/claude-config")


def test_run_haiku_keeps_existing_config_dir(monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", "/somewhere/else")
    calls = []
    monkeypatch.setattr(at.subprocess, "run", _fake_run(calls))
    at.run_haiku("p")
    assert calls[0]["env"]["CLAUDE_CONFIG_DIR"] == "/somewhere/else"


def test_run_haiku_error_carries_stdout_when_stderr_empty(monkeypatch):
    out = "Failed to authenticate: OAuth session expired and could not be refreshed" + "x" * 200
    monkeypatch.setattr(at.subprocess, "run", _fake_run([], 1, out, ""))
    try:
        at.run_haiku("p")
    except RuntimeError as e:
        msg = str(e)
    else:
        raise AssertionError("expected RuntimeError")
    assert msg.startswith("claude exited 1: Failed to authenticate")
    assert len(msg) <= len("claude exited 1: ") + 120


def test_alert_missing_from_reply_fails_open():
    def runner(prompt):
        return json.dumps([{"id": "a1", "investigate": False, "reason": "known"}])
    recs, _ = _triage([A("idle", session="tm-a"), A("error")], runner)
    assert [r["decision"] for r in recs] == ["log", "wake"]
    assert "fail-open" in recs[1]["reason"]


def test_pane_capture_failure_does_not_block_the_gate():
    def pane(session):
        raise OSError("no tmux")

    def runner(prompt):
        return json.dumps([{"id": "a1", "investigate": False, "reason": "ok"}])
    recs = at.triage([A("idle", session="tm-a")], rules(), NOW, {}, runner=runner, pane_fn=pane)
    assert recs[0]["decision"] == "log"


def test_disabled_gate_wakes(monkeypatch):
    monkeypatch.setenv("TRIAGE_HAIKU", "off")
    recs = at.triage([A("error")], rules(), NOW, {}, pane_fn=lambda s: "")
    assert recs[0]["decision"] == "wake" and "disabled" in recs[0]["reason"]


# --- record + wake message ------------------------------------------------------------

def test_record_fields_and_jsonl(tmp_path):
    recs, _ = _triage([A("rc_restored")], lambda p: "")
    r = recs[0]
    assert {"ts", "id", "kind", "session", "decision", "path", "reason"} <= set(r)
    out = tmp_path / "triage.jsonl"
    at.append_records(str(out), recs)
    at.append_records(str(out), recs)
    assert len(out.read_text().splitlines()) == 2


def test_wake_message_names_only_woken(monkeypatch):
    sent = []
    pane = "─" * 40 + "\n❯ \n" + "─" * 40 + "\n  ⏵⏵ bypass\n"
    monkeypatch.setattr(fp, "capture", lambda p: pane)
    monkeypatch.setattr(fp, "send_to_supervisor", lambda p, *k: sent.append(k))
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    monkeypatch.setattr(fp, "_box_full_text", lambda raw: fp._norm(sent[0][1]) if sent else "")
    monkeypatch.setattr(fp.input_state, "classify", lambda raw: ("empty", ""))
    panes = [{"session": "tm-architect", "pane": "%0", "active": "1"}]
    fp.wake(2, panes, False, logged=14)
    assert "2 alerts need you (14 logged)" in sent[0][1]
    assert fp._own_pointer(fp._norm(sent[0][1]))


def test_cycle_step_writes_all_alerts_triage_and_counts_woken(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "INBOX", str(tmp_path))
    monkeypatch.setenv("TRIAGE_LOG", str(tmp_path / "triage.jsonl"))
    monkeypatch.setattr(fp, "TRIAGE_RULES", str(tmp_path / "none.json"))
    monkeypatch.setenv("TRIAGE_HAIKU", "off")
    woken, logged = fp.triage_step([A("rc_restored"), A("owner_answer"), A("error")], NOW, {}, False)
    assert (len(woken), logged) == (2, 1)
    lines = [json.loads(x) for x in (tmp_path / "triage.jsonl").read_text().splitlines()]
    assert [l["decision"] for l in lines] == ["log", "wake", "wake"]


LIVE_LOG = os.path.join(os.path.dirname(HERE), "inbox", "triage.jsonl")


def test_guard_live_triage_log_untouched_by_cycle_steps():
    """Fails if any test path appends to the live inbox/triage.jsonl."""
    before = os.stat(LIVE_LOG).st_size if os.path.exists(LIVE_LOG) else None
    fp.triage_step([A("owner_answer"), A("rc_restored")], NOW, {}, False)
    after = os.stat(LIVE_LOG).st_size if os.path.exists(LIVE_LOG) else None
    assert before == after

"""Tests for scripts/fleet-poll.py: pane classifiers, alert evaluation, dedup.

Fixtures are synthetic captures shaped like Claude Code 2.1.28x panes under
trusty-mpm 1.7.4 (input box between two rules, status line, agent rows).
"""
import importlib.util
import json
import os

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll", "fleet-poll.py")
input_state = _load("input_state_cli", "input-state.py")

RULE = "─" * 80
DIM, RESET = "\x1b[2m", "\x1b[0m"


def screen(body, box="", dim=False, agents=(), ctx=30, plan=(30, 40), status=None):
    """A pane capture: transcript, optional status line, input box, footer."""
    box_line = "❯\xa0" + (f"{DIM}{box}{RESET}" if dim else box)
    footer = [f"  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx {ctx}% | $1.00 | "
              f"⏳{plan[0]}% 📅{plan[1]}% | 💸1%/1%",
              "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents",
              " " * 70 + "/rc", "", "  ⏺ main"]
    footer += [f"  ◯ {a}  Doing things…  3m 2s · ↓ 12.0k tokens" for a in agents]
    lines = list(body) + ([status] if status else []) + [RULE, box_line, RULE] + footer
    return "\n".join(lines) + "\n\n"


PM_TURN = ["⏺ Merged #12 and the CI run is green.", "", "  Next: the release notes."]
PM_ASKS = ["⏺ Two options for the cache.", "",
           "  - A: keep it", "  - B: drop it", "", "  Which do you want, A or B?"]

PERMISSION = "\n".join([
    "⏺ Bash(rm -rf build/)",
    "",
    RULE,
    " Bash command · from the local-ops agent",
    "",
    "   rm -rf build/",
    "   Remove the build dir",
    "",
    " Do you want to proceed?",
    " ❯ 1. Yes",
    "   2. Yes, and don't ask again for rm commands in this project",
    "   3. No, and tell Claude what to do differently (esc)",
    "",
    " Esc to cancel",
]) + "\n"

ASK_USER = "\n".join([
    "⏺ I need a decision.",
    RULE,
    " ☐ Cache  ✔ Submit  →",
    "",
    "Keep the cache?",
    "",
    "❯ 1. Keep (Recommended)",
    "     Faster builds",
    "  2. Drop",
    "  3. Type something.",
    RULE,
    "  4. Chat about this",
    "",
    "Enter to select · ↑/↓ to navigate · Esc to cancel",
]) + "\n"


def kinds(alerts):
    return sorted(a["kind"] for a in alerts)


def run(capture, ps=None, now=10_000.0):
    info = fp.parse_pane(capture)
    ps = {} if ps is None else ps
    return info, fp.evaluate(info, ps, now, "tm-x", "%1"), ps


# --- input-box classifier shared with input-state.py ------------------------

@pytest.mark.parametrize("box,dim,expected", [
    ("", False, "empty"),
    ("merge PR #7 once CI is green", True, "suggestion"),
    ("half-typed reply", False, "typed"),
])
def test_input_box_classifier_is_input_states(box, dim, expected):
    cap = screen(PM_TURN, box=box, dim=dim)
    assert input_state.classify(cap)[0] == expected
    assert fp.parse_pane(cap)["box"] == expected


# --- dialog ------------------------------------------------------------------

def test_permission_dialog_detected_and_fingerprint_ignores_cursor():
    info, alerts, _ = run(PERMISSION)
    assert info["dialog"] is not None
    assert kinds(alerts) == ["dialog"]  # a dialog pane is never idle or a question
    assert "Do you want to proceed?" in alerts[0]["detail"]
    moved = PERMISSION.replace(" ❯ 1. Yes", "   1. Yes").replace("   2. Yes,", " ❯ 2. Yes,")
    assert fp.evaluate(fp.parse_pane(moved), {}, 0, "tm-x", "%1")[0]["fingerprint"] == \
        alerts[0]["fingerprint"]


def test_ask_user_question_box_is_a_dialog():
    info, alerts, _ = run(ASK_USER)
    assert kinds(alerts) == ["dialog"]
    assert "Keep the cache?" in alerts[0]["detail"]


def test_dialog_words_in_transcript_above_the_box_are_not_a_dialog():
    body = ["⏺ The prompt said:", "  Do you want to proceed?", "  ❯ 1. Yes"]
    info, _, _ = run(screen(body, agents=["engineer"]))
    assert info["dialog"] is None


# --- box detection against a real capture with a labeled top rule -----------
# tests/fixtures/idle-pane-labeled-rule.txt: the Architect's own idle pane,
# captured 2026-09-27. Claude Code now draws the box's TOP rule with a short
# date label ("──…── 9-26/2026 ─"); the old plain-rule-only check made
# find_box() miss it (has_box=False), so the poller could never wake the
# Architect even while genuinely idle (poll-state.json: wake_pending=239,
# no wake sent since 2026-09-26T19:15Z).

def test_labeled_top_rule_is_still_recognized_as_the_box():
    with open(os.path.join(FIXTURES, "idle-pane-labeled-rule.txt")) as f:
        cap = f.read()
    info = fp.parse_pane(cap)
    assert info["has_box"] is True
    assert info["box"] == "suggestion"
    assert info["dialog"] is None


def test_a_dialogs_option_lines_are_never_mistaken_for_the_box():
    """The lenient top-rule match must not let a dialog's option rows —
    which never carry a real rule line above them — pass as a box."""
    info, alerts, _ = run(PERMISSION)
    assert info["dialog"] is not None and info["has_box"] is False
    assert kinds(alerts) == ["dialog"]


# --- idle / busy / question --------------------------------------------------

def test_idle_prompt_alerts_after_ten_minutes_only():
    cap = screen(PM_TURN, status="✻ Worked for 3m 2s")
    info, alerts, ps = run(cap, now=1000.0)
    assert not info["busy"] and alerts == []
    assert ps["idle_since"] == 1000.0
    _, alerts, _ = run(cap, ps, now=1000.0 + fp.IDLE_AFTER)
    assert kinds(alerts) == ["idle"]


def test_suggestion_box_counts_as_idle():
    cap = screen(PM_TURN, box="do the next thing", dim=True)
    _, _, ps = run(cap, now=0.0)
    _, alerts, _ = run(cap, ps, now=fp.IDLE_AFTER)
    assert kinds(alerts) == ["idle"]


@pytest.mark.parametrize("agents,status", [
    (["python-engineer", "research"], "✻ Waiting for 2 background agents to finish"),
    ([], "✳ Undulating… (5m 23s · ↓ 19.1k tokens)"),
    ([], "✻ Waiting for 1 background agent to finish"),
])
def test_busy_pane_is_neither_idle_nor_question(agents, status):
    info, alerts, ps = run(screen(PM_ASKS, agents=agents, status=status), now=0.0)
    assert info["busy"] and alerts == [] and ps["idle_since"] is None


def test_old_waiting_line_above_a_newer_turn_is_not_busy():
    body = ["✻ Waiting for 1 background agent to finish"] + PM_TURN
    assert not fp.parse_pane(screen(body))["busy"]


def test_question_is_last_pm_line_ending_in_question_mark():
    _, alerts, _ = run(screen(PM_ASKS, status="✻ Cooked for 23s · done 7:36 AM"))
    assert kinds(alerts) == ["question"]
    assert alerts[0]["detail"] == "Which do you want, A or B?"
    user_turn = PM_TURN + ["", "❯ can you check the logs?"]
    assert fp.parse_pane(screen(user_turn))["question"] is None


# --- ctx / plan-limit / error ------------------------------------------------

def test_ctx_47_alerts_and_crossing_50_changes_fingerprint():
    _, alerts, _ = run(screen(PM_TURN, agents=["x"], ctx=47))
    assert kinds(alerts) == ["ctx"] and alerts[0]["fingerprint"] == "45"
    _, alerts, _ = run(screen(PM_TURN, agents=["x"], ctx=52))
    assert alerts[0]["fingerprint"] == "50"
    _, alerts, _ = run(screen(PM_TURN, agents=["x"], ctx=44))
    assert alerts == []


def test_plan_limit_text_and_status_line():
    body = PM_TURN + ["  ⎿  Claude usage limit reached. Your limit will reset at 5pm."]
    _, alerts, _ = run(screen(body, agents=["x"]))
    assert kinds(alerts) == ["plan-limit"]
    prose = ["⏺ Both agents died at the account's usage limit."]
    assert run(screen(prose, agents=["x"]))[1] == []
    infos = [("tm-a", fp.parse_pane(screen(PM_TURN, plan=(91, 50)))),
             ("tm-b", fp.parse_pane(screen(PM_TURN, plan=(88, 50))))]
    plan = fp.fleet_plan_alerts(infos, 0.0)
    assert len(plan) == 1 and plan[0]["session"] == "" and "91%" in plan[0]["detail"]
    assert not plan[0]["display"].startswith("[")


def test_error_alerts_only_for_lines_new_since_last_cycle():
    old = PM_TURN + ["  ⎿  Error: Exit code 1"]
    _, alerts, ps = run(screen(old, agents=["x"]))
    assert alerts == []  # first sight seeds the set
    new = old + ["⏺ API Error: 529 overloaded"]
    _, alerts, ps = run(screen(new, agents=["x"]), ps)
    assert kinds(alerts) == ["error"] and "API Error" in alerts[0]["detail"]
    assert run(screen(new, agents=["x"]), ps)[1] == []


# --- dedup / state -----------------------------------------------------------

def a(kind, f="f1", session="tm-x"):
    return fp.alert(kind, f, "d", session, "%1")


def test_dedup_suppresses_until_cleared_for_one_cycle():
    st = {}
    assert len(fp.dedup(st, [a("dialog")], 0)[0]) == 1
    assert fp.dedup(st, [a("dialog")], 60) == ([], 1)
    fp.dedup(st, [], 120)                     # cleared for one cycle
    assert len(fp.dedup(st, [a("dialog")], 180)[0]) == 1
    assert len(fp.dedup(st, [a("dialog", "f2")], 240)[0]) == 1  # new fingerprint


def test_dedup_reemits_idle_every_30_minutes_but_not_dialog():
    st = {}
    fp.dedup(st, [a("idle"), a("dialog")], 0)
    new, sup = fp.dedup(st, [a("idle"), a("dialog")], 1799)
    assert new == [] and sup == 2
    new, sup = fp.dedup(st, [a("idle"), a("dialog")], 1800)
    assert kinds(new) == ["idle"] and sup == 1
    assert st["alerts"]["tm-x|%1|idle|f1"] == {"first": 0, "last": 1800}


def test_alert_display_and_redaction():
    x = fp.alert("dialog", "f", "export API_KEY=abc123 then run", "tm-api", "%3")
    assert x["display"].startswith("[tm-api]: dialog: ")
    assert "abc123" not in x["detail"]
    assert fp.alert("load", "load", "load 20.0 > 16 cores")["display"] == \
        "load: load 20.0 > 16 cores"


def test_events_become_alerts_except_idle_prompt(tmp_path, monkeypatch):
    ev = tmp_path / "events.jsonl"
    monkeypatch.setattr(fp, "EVENTS", str(ev))
    ev.write_text("")
    st = {}
    assert fp.read_events(st) == [] and st["events_offset"] == 0
    rows = [{"tmux_session": "tm-api", "tmux_pane": "%3", "type": t, "message": m}
            for t, m in (("permission_prompt", "Claude needs your permission to use Bash"),
                         ("idle_prompt", "Claude is waiting for your input"),
                         ("agent_needs_input", "Agent needs input"))]
    rows.append({"tmux_session": fp.ARCHITECT, "type": "permission_prompt", "message": "x"})
    ev.write_text("".join(json.dumps(r) + "\n" for r in rows))
    got = fp.read_events(st)
    assert kinds(got) == ["agent_needs_input", "permission_prompt"]
    assert fp.read_events(st) == []


# --- self_ctx (the Architect's own transcript has no `ctx N%` field) --------

def _usage_line(usage, msg_type="assistant", entrypoint="cli"):
    return json.dumps({"type": msg_type, "message": {"usage": usage},
                       "entrypoint": entrypoint}) + "\n"


def test_self_ctx_alerts_only_when_over_threshold(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path))
    monkeypatch.delenv("SELF_CTX_WINDOW", raising=False)
    monkeypatch.delenv("SELF_CTX_THRESHOLD", raising=False)

    over = tmp_path / "over.jsonl"
    over.write_text(
        _usage_line({"input_tokens": 1}, "user")  # non-assistant line: ignored
        + _usage_line({"input_tokens": 100_000, "cache_read_input_tokens": 300_000,
                       "cache_creation_input_tokens": 5_000})  # 405,000 / 1,000,000 = 40.5%
    )
    os.utime(over, (1000, 1000))
    alerts = fp.self_ctx_alerts(0.0)
    assert kinds(alerts) == ["self_ctx"]
    assert "self ctx 41%" in alerts[0]["detail"] or "self ctx 40%" in alerts[0]["detail"]
    assert alerts[0]["session"] == fp.ARCHITECT

    # A newer transcript (higher mtime) under the threshold: no alert, and the
    # newer file — not the older over-threshold one — is the one read.
    under = tmp_path / "under.jsonl"
    under.write_text(_usage_line({"input_tokens": 1_000, "cache_read_input_tokens": 2_000}))
    os.utime(under, (2000, 2000))
    assert fp.self_ctx_alerts(0.0) == []


def test_self_ctx_alerts_ignores_a_newer_sdk_cli_transcript(tmp_path, monkeypatch):
    """question-collector.py's `claude -p --agent` runs (entrypoint "sdk-cli")
    write to the same directory every few minutes and can out-mtime the
    Architect's own session; they must never be the one measured."""
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path))
    monkeypatch.delenv("SELF_CTX_WINDOW", raising=False)
    monkeypatch.delenv("SELF_CTX_THRESHOLD", raising=False)
    architect = tmp_path / "architect.jsonl"
    architect.write_text(_usage_line({"input_tokens": 900_000}, entrypoint="cli"))
    os.utime(architect, (1000, 1000))
    collector = tmp_path / "collector.jsonl"
    collector.write_text(_usage_line({"input_tokens": 1}, entrypoint="sdk-cli"))
    os.utime(collector, (9000, 9000))  # newer than the Architect's own transcript
    alerts = fp.self_ctx_alerts(0.0)
    assert kinds(alerts) == ["self_ctx"]  # would be [] if the collector file won


def test_self_ctx_missing_or_empty_dir_never_crashes(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path / "does-not-exist"))
    assert fp.self_ctx_alerts(0.0) == []
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path))  # exists but has no *.jsonl
    assert fp.self_ctx_alerts(0.0) == []


def test_self_ctx_window_and_threshold_are_env_overridable(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path))
    f = tmp_path / "t.jsonl"
    f.write_text(_usage_line({"input_tokens": 50, "cache_read_input_tokens": 50}))  # 100 tokens
    monkeypatch.setenv("SELF_CTX_WINDOW", "1000")
    monkeypatch.setenv("SELF_CTX_THRESHOLD", "5")  # 100/1000 = 10% >= 5%
    alerts = fp.self_ctx_alerts(0.0)
    assert kinds(alerts) == ["self_ctx"]
    monkeypatch.setenv("SELF_CTX_THRESHOLD", "50")  # 10% < 50%: no alert
    assert fp.self_ctx_alerts(0.0) == []


def test_self_ctx_reemit_interval_matches_ctx(tmp_path, monkeypatch):
    monkeypatch.setattr(fp, "SELF_CTX_DIR", str(tmp_path))
    (tmp_path / "t.jsonl").write_text(
        _usage_line({"input_tokens": 500_000, "cache_read_input_tokens": 0}))
    assert fp.REEMIT["self_ctx"] == fp.REEMIT["ctx"]
    st = {}
    a1, s1 = fp.dedup(st, fp.self_ctx_alerts(0.0), 0.0)
    assert len(a1) == 1 and s1 == 0
    a2, s2 = fp.dedup(st, fp.self_ctx_alerts(1799.0), 1799.0)
    assert a2 == [] and s2 == 1
    a3, s3 = fp.dedup(st, fp.self_ctx_alerts(1800.0), 1800.0)
    assert len(a3) == 1


# --- wake() skip reasons / dispatch_wake pending -----------------------------

SUP_PANE = [{"session": fp.ARCHITECT, "pane": "%1", "cmd": "claude", "active": "11"}]


def test_wake_reports_no_input_box_found_not_a_misleading_box_state(monkeypatch):
    """A busy pane's TUI drops the rule-bounded box find_box() requires while
    it works, even though a stale "❯" line elsewhere still makes
    input_state.classify() guess "empty" — that guess is not the real skip
    reason and must not appear in its place (poll.log, 2026-09-27: 980+196
    such entries while the Architect was continuously busy)."""
    cap = "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n"
    monkeypatch.setattr(fp, "capture", lambda pane: cap)
    assert fp.parse_pane(cap)["has_box"] is False  # the actual cause
    assert fp.wake(1, SUP_PANE, dry_run=False) == "skipped: no input box found"


def test_wake_reports_input_typed_when_a_real_box_holds_a_draft(monkeypatch):
    cap = screen(PM_TURN, box="half-typed reply")
    monkeypatch.setattr(fp, "capture", lambda pane: cap)
    assert fp.parse_pane(cap)["has_box"] is True
    assert fp.wake(1, SUP_PANE, dry_run=False) == "skipped: input typed"


def _wrapped_box_screen(body, text):
    """A box whose typed `text` is split across two screen rows with no rule
    between them — the shape a narrow pane (the Architect's own is ~47 cols)
    produces, and which input_state.classify()'s single-line read never sees
    in full."""
    half = len(text) // 2
    return "\n".join(list(body) + [RULE, f"❯\xa0{text[:half]}", text[half:], RULE]) + "\n"


def test_wake_confirms_a_message_that_wraps_across_screen_rows(monkeypatch):
    """poll.log 11:02:25Z/11:15:30Z/11:41:39Z: `wake()` reported the text
    unconfirmed and skipped Enter even though it WAS in the box, split across
    wrapped rows the old single-line prefix check never read past."""
    sent, captures = {}, {"n": 0}
    pre_cap = screen(PM_TURN, box="")

    def fake_capture(pane):
        captures["n"] += 1
        if captures["n"] == 1:
            return pre_cap                                 # box empty, pre-send
        if captures["n"] == 2:
            return _wrapped_box_screen(PM_TURN, sent["msg"])  # just-typed text, wrapped
        return screen(PM_TURN, box="")                     # box empty again, post-Enter

    def fake_send(pane, *keys):
        if keys and keys[0] == "-l":
            sent["msg"] = keys[1]

    monkeypatch.setattr(fp, "capture", fake_capture)
    monkeypatch.setattr(fp, "send_to_architect", fake_send)
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    result = fp.wake(3, SUP_PANE, dry_run=False)
    assert result.startswith("sent to")
    assert sent["msg"].startswith("[poll ") and "3 new alerts ->" in sent["msg"]


def test_wake_cleans_up_its_own_text_when_confirmation_fails(monkeypatch):
    """A genuine confirmation failure (text never landed) must never leave the
    poller's own typed text sitting in the box for later cycles to trip over."""
    keys_sent = []
    empty_cap = screen(PM_TURN, box="")

    monkeypatch.setattr(fp, "capture", lambda pane: empty_cap)  # box stays empty throughout
    monkeypatch.setattr(fp, "send_to_architect", lambda pane, *k: keys_sent.append(k))
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    result = fp.wake(2, SUP_PANE, dry_run=False)
    assert result.startswith("failed:") and "cleaned up, box empty" in result
    assert keys_sent[0][0] == "-l"
    msg = keys_sent[0][1]
    bspace_calls = [k for k in keys_sent if k and k[0] == "BSpace"]
    assert len(bspace_calls) == 1
    assert len(bspace_calls[0]) == len(msg)     # removes exactly what it typed, no more
    assert not any(k[0] == "Enter" for k in keys_sent)  # never submits unconfirmed text


def test_wake_detects_its_own_stale_pointer_and_does_not_send(monkeypatch):
    """A pointer left over from an earlier failed cycle, wrapped across rows,
    is recognized as the poller's own and never treated like a human draft —
    and never deleted here, since deletion belongs only to the just-typed
    cleanup path above."""
    stale = "[poll 11:15Z] 4 new alerts -> /srv/architect/inbox/alerts.jsonl"
    cap = _wrapped_box_screen(PM_TURN, stale)
    monkeypatch.setattr(fp, "capture", lambda pane: cap)
    keys_sent = []
    monkeypatch.setattr(fp, "send_to_architect", lambda pane, *k: keys_sent.append(k))
    assert fp.parse_pane(cap)["box"] == "typed"
    result = fp.wake(1, SUP_PANE, dry_run=False)
    assert result == "skipped: own stale pointer in box"
    assert keys_sent == []


def test_wake_does_not_mistake_a_human_draft_starting_with_poll_for_its_own(monkeypatch):
    """Only the full '[poll ...] N new alerts -> path' shape is recognized as
    the poller's own; a draft that merely starts similarly stays a draft."""
    cap = screen(PM_TURN, box="poll the team about the release date")
    monkeypatch.setattr(fp, "capture", lambda pane: cap)
    assert fp.wake(1, SUP_PANE, dry_run=False) == "skipped: input typed"


def test_dispatch_wake_carries_pending_across_a_skip_then_wakes_once(monkeypatch):
    """The required scenario: a skipped cycle (3 new alerts) followed by an
    idle cycle (2 more) must wake exactly once, carrying both cycles' total."""
    calls = []

    def fake_wake(n, panes, dry_run):
        calls.append(n)
        return "skipped: no input box found" if len(calls) == 1 else "sent to %1"

    monkeypatch.setattr(fp, "wake", fake_wake)
    state = {}
    r1 = fp.dispatch_wake(state, [], 3, False)   # cycle 1: skipped (busy)
    assert r1 == "skipped: no input box found" and state["wake_pending"] == 3
    r2 = fp.dispatch_wake(state, [], 2, False)   # cycle 2: box now free
    assert r2 == "sent to %1"
    assert calls == [3, 5]                       # one wake(), carrying both cycles
    assert state["wake_pending"] == 0


def test_dispatch_wake_carries_pending_on_a_failed_confirm(monkeypatch):
    calls = []

    def fake_wake(n, panes, dry_run):
        calls.append(n)
        return "failed: text not confirmed in the input box; Enter not sent"

    monkeypatch.setattr(fp, "wake", fake_wake)
    state = {}
    fp.dispatch_wake(state, [], 4, False)
    assert state["wake_pending"] == 4
    fp.dispatch_wake(state, [], 0, False)        # no new alerts, pending still retried
    assert calls == [4, 4] and state["wake_pending"] == 4


def test_dispatch_wake_none_when_nothing_pending():
    state = {}
    assert fp.dispatch_wake(state, [], 0, False) == "none"
    assert state.get("wake_pending", 0) == 0


# --- quiet-sessions.txt / test-fixture "exited" suppression (cycle-level) --

def _pane(session, pane_id, cmd="2.1.283"):
    return {"session": session, "pane": pane_id, "cmd": cmd, "active": "11"}


def _cycle_env(monkeypatch, tmp_path, panes, quiet_lines=""):
    """Wire cycle() to a fake fleet: the given (pane, capture) pairs, no real
    tmux/host/self_ctx/event noise, alerts/log written under tmp_path.
    quiet_lines=None leaves quiet-sessions.txt absent (the missing-file case)."""
    captures = {p["pane"]: cap for p, cap in panes}
    monkeypatch.setattr(fp, "list_panes", lambda: [p for p, _ in panes])
    monkeypatch.setattr(fp, "capture", lambda pid: captures[pid])
    monkeypatch.setattr(fp, "host_alerts", lambda now: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    events = tmp_path / "events.jsonl"
    events.write_text("")
    monkeypatch.setattr(fp, "EVENTS", str(events))
    alerts = tmp_path / "alerts.jsonl"
    monkeypatch.setattr(fp, "ALERTS", str(alerts))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    quiet_file = tmp_path / "quiet-sessions.txt"
    if quiet_lines is not None:
        quiet_file.write_text(quiet_lines)
    monkeypatch.setattr(fp, "QUIET_SESSIONS_FILE", str(quiet_file))
    return alerts


def _written_kinds(alerts_path, session):
    if not os.path.exists(alerts_path):
        return []
    with open(alerts_path) as f:
        return [json.loads(l)["kind"] for l in f if json.loads(l)["session"] == session]


def test_quiet_session_suppresses_idle_but_not_ctx(tmp_path, monkeypatch):
    parked, active = _pane("tm-parked", "%10"), _pane("tm-active", "%11")
    cap = screen(PM_TURN, ctx=50)  # empty box, no agents: idle-eligible; ctx>=45 too
    alerts_path = _cycle_env(monkeypatch, tmp_path, [(parked, cap), (active, cap)],
                             quiet_lines="tm-parked\n")
    state = {"panes": {"tm-parked|%10": {"idle_since": 0.0},
                       "tm-active|%11": {"idle_since": 0.0}}}
    fp.cycle(state, dry_run=False, now=fp.IDLE_AFTER + 1.0)
    assert _written_kinds(alerts_path, "tm-parked") == ["ctx"]           # idle dropped
    assert sorted(_written_kinds(alerts_path, "tm-active")) == ["ctx", "idle"]


def test_quiet_sessions_file_missing_means_nothing_is_quiet(tmp_path, monkeypatch):
    p = _pane("tm-active", "%11")
    alerts_path = _cycle_env(monkeypatch, tmp_path, [(p, screen(PM_TURN))], quiet_lines=None)
    state = {"panes": {"tm-active|%11": {"idle_since": 0.0}}}
    fp.cycle(state, dry_run=False, now=fp.IDLE_AFTER + 1.0)
    assert _written_kinds(alerts_path, "tm-active") == ["idle"]


def test_exited_suppressed_for_a_fixture_shell_pane_but_not_a_real_one(tmp_path, monkeypatch):
    fixture, real = _pane("tm-r7", "%20", cmd="bash"), _pane("tm-realproj", "%21", cmd="bash")
    alerts_path = _cycle_env(monkeypatch, tmp_path, [(fixture, ""), (real, "")])
    fp.cycle({"panes": {}}, dry_run=False, now=0.0)
    assert _written_kinds(alerts_path, "tm-r7") == []
    assert _written_kinds(alerts_path, "tm-realproj") == ["exited"]


def test_exited_suppressed_for_a_vanished_fixture_session_but_not_a_real_one(tmp_path, monkeypatch):
    alerts_path = _cycle_env(monkeypatch, tmp_path, [])
    state = {"panes": {}, "sessions": {"tm-qa1-run": "%30", "tm-realproj": "%31"}}
    fp.cycle(state, dry_run=False, now=0.0)
    assert _written_kinds(alerts_path, "tm-qa1-run") == []
    assert _written_kinds(alerts_path, "tm-realproj") == ["exited"]


# --- load alert: the 5-min average, not the 1-min spike ---------------------

def _loadavg(monkeypatch, load1, load5, load15, cores=16, factor=None):
    monkeypatch.setattr(fp.os, "getloadavg", lambda: (load1, load5, load15))
    monkeypatch.setattr(fp.os, "cpu_count", lambda: cores)
    if factor is None:
        monkeypatch.delenv("LOAD_FACTOR", raising=False)
    else:
        monkeypatch.setenv("LOAD_FACTOR", str(factor))


def test_a_brief_1min_spike_with_a_low_5min_average_does_not_alert(monkeypatch):
    """2026-09-27 06:28Z+: a build spike hit 29.7 one-minute / 16.8 five-minute
    on 16 cores — 1.5x16=24, so the 5-min average must NOT trip this."""
    _loadavg(monkeypatch, 29.7, 16.8, 10.0, cores=16)
    assert [a["kind"] for a in fp.host_alerts(0.0) if a["kind"] == "load"] == []


def test_a_sustained_5min_average_above_1_5x_cores_alerts(monkeypatch):
    _loadavg(monkeypatch, 30.0, 25.0, 20.0, cores=16)   # 1.5x16=24; 25.0 > 24
    alerts = [a for a in fp.host_alerts(0.0) if a["kind"] == "load"]
    assert len(alerts) == 1
    assert "30.0/25.0/20.0" in alerts[0]["detail"]        # all three averages shown


def test_load_factor_is_env_overridable(monkeypatch):
    _loadavg(monkeypatch, 10.0, 17.0, 5.0, cores=8, factor=2.0)   # 2.0x8=16; 17>16
    assert len([a for a in fp.host_alerts(0.0) if a["kind"] == "load"]) == 1
    _loadavg(monkeypatch, 10.0, 17.0, 5.0, cores=8, factor=3.0)   # 3.0x8=24; 17<24
    assert [a for a in fp.host_alerts(0.0) if a["kind"] == "load"] == []


def test_load_reemit_interval_is_unchanged_at_1800s():
    assert fp.REEMIT["load"] == 1800


def test_send_refuses_any_pane_outside_the_architect(monkeypatch):
    calls = []

    def fake_tmux(*args):
        calls.append(args)
        return "tm-api\n" if args[0] == "display-message" else ""

    monkeypatch.setattr(fp, "tmux", fake_tmux)
    with pytest.raises(fp.TmuxError):
        fp.send_to_architect("%3", "-l", "hello")
    assert not any(c[0] == "send-keys" for c in calls)

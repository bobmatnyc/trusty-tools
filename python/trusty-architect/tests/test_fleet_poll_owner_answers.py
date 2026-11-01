"""#10670 (#10579): an owner's dashboard answer reaches the Architect through the poller.

Why: a fresh install's fleet-poll.py never read the answer store, so an answered item
woke nobody until a human noticed.
What: drives cycle() against a real temporary store and a fake Architect pane that models
the input box (no tmux): idle and busy delivery, bounded re-alert until consumed, failed
store reads, drafts in the box, restarts, and the wiring in every seed copy. The fixture quiets
the watchdog: with no Architect pane its architect_dead alert wakes on its own, hiding a missing
answer alert.
"""
import ast
import importlib.util
import json
import os

import dashboard_store
import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
# The shipped seed, when this runs in the monorepo; a standalone tree has only SCRIPTS.
SHIPPED = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(HERE))),
                       "crates", "trusty-mpm", "src", "assets", "architect", "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll_owner_answers", "fleet-poll.py")

RULE = "─" * 60
T0 = 1_000_000.0
AT = "2026-01-02T03:04:05Z"


FOOTER = ["  TM 1.7.4 ● | org/proj | Opus 5.5 | ctx 10%",
          "  ⏵⏵ bypass permissions on (shift+tab to cycle)", " " * 70 + "/rc"]


class FakePane:
    """The Architect's pane: an input box that takes keys, with no tmux behind it."""

    def __init__(self):
        self.box, self.continuation, self.submitted, self.keys = "", "", [], []
        self.busy = self.box_hidden = self.no_bottom_rule = False
        self.dim = False  # the box row is a dim auto-suggestion; typing replaces it
        self.plain_continuation = False  # #10670: an undimmed draft row under the dim row
        self.footer = FOOTER
        self.typed_during_send = None  # (text, "before" | "after"): a human racing the wake
        self.raw = None

    def capture(self, _pane=None):
        if self.raw is not None:  # a literal -e capture, shown until a key is typed
            return self.raw
        if self.box_hidden:  # the TUI drops the rule-bounded box mid-turn
            return "❯\n\n  ✻ Working… (2m 14s · esc to interrupt)\n"
        status = ["✻ Working… (2m 14s · esc to interrupt)"] if self.busy else []
        rows = [f"❯\xa0{self.box}"] + ([self.continuation] if self.continuation else [])
        if self.dim and self.box:  # #10670: capture-pane -e carries SGR 2 onto a wrapped row
            ends_dim = len(rows) == 1 or self.plain_continuation
            rows[0] = f"❯\xa0\x1b[2m{self.box}" + ("\x1b[0m" if ends_dim else "")
            if len(rows) > 1 and not self.plain_continuation:
                rows[1] += "\x1b[0m"
        if self.no_bottom_rule:  # #10670: footer rows straight under the box
            return "\n".join(["⏺ Done."] + status + [RULE] + rows + self.footer) + "\n"
        return "\n".join(["⏺ Done."] + status + [RULE] + rows + [RULE]) + "\n"

    def send(self, _pane, *keys):
        self.keys.append(keys)
        if keys[0] == "-l":
            self.raw = None
            if self.dim:  # typing over a suggestion replaces all of it
                self.box = self.continuation = ""
                self.dim = False
            if self.typed_during_send:
                text, side = self.typed_during_send
                self.typed_during_send = None
                self.box = text + keys[1] if side == "before" else keys[1] + text
            else:
                self.box += keys[1]
        elif keys[0] == "BSpace":
            self.box = self.box[:-len(keys)]
        elif keys[0] == "Enter":
            self.submitted.append(self.box)
            self.box = ""


@pytest.fixture
def pane(monkeypatch, tmp_path):
    p = FakePane()
    monkeypatch.setattr(fp, "list_panes", lambda: [{"session": fp.ARCHITECT, "pane": "%1",
                                                    "cmd": "claude", "active": "11"}])
    monkeypatch.setattr(fp, "capture", p.capture)
    monkeypatch.setattr(fp, "send_to_architect", p.send)
    monkeypatch.setattr(fp.time, "sleep", lambda s: None)
    monkeypatch.setattr(fp, "host_alerts", lambda now: [])
    monkeypatch.setattr(fp, "self_ctx_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "read_events", lambda state: [])
    # #10571's cycle steps: none reaches a live process or file from a test here.
    monkeypatch.setattr(fp, "watchdog_alerts", lambda *a: ([], "architect=ok"))
    monkeypatch.setattr(fp, "il_cron_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "ctx_watch_alerts", lambda now, dry_run: [])
    monkeypatch.setattr(fp, "self_clear_running", lambda arch_dir: False)
    monkeypatch.setattr(fp, "ALERTS", str(tmp_path / "alerts.jsonl"))
    monkeypatch.setattr(fp, "LOG", str(tmp_path / "poll.log"))
    monkeypatch.setattr(fp, "STATE", str(tmp_path / "poll-state.json"))
    monkeypatch.setattr(fp, "QUIET_SESSIONS_FILE", str(tmp_path / "quiet.txt"))
    return p


@pytest.fixture
def store(monkeypatch, tmp_path):
    root = tmp_path / "store"
    (root / "items").mkdir(parents=True)
    monkeypatch.setattr(dashboard_store, "DEFAULT_ROOT", str(root))
    monkeypatch.setenv("DASHBOARD_ROOT", str(root))  # conftest points it at a scratch root
    return root


def _answer(store, item_id="D1", **extra):
    record = {"version": 1, "kind": "question", "status": "answered", "title": "Pick one",
              "answer": "Option A", "answered_at": AT, **extra}
    (store / "items" / f"{item_id}.json").write_text(json.dumps(record))
    return record


def _alerts(tmp_path):
    path = tmp_path / "alerts.jsonl"
    if not path.exists():
        return []
    return [a for a in map(json.loads, path.read_text().splitlines())
            if a["kind"] == "owner_answer"]


def _log(tmp_path):
    path = tmp_path / "poll.log"
    return path.read_text() if path.exists() else ""


# --- criterion 1: idle and busy delivery -------------------------------------

def test_a_new_answer_alerts_and_wakes_on_the_next_idle_cycle(pane, store, tmp_path):
    _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    assert [a["fingerprint"] for a in _alerts(tmp_path)] == [f"D1@{AT}"]
    assert "D1 answered" in _alerts(tmp_path)[0]["display"]
    assert len(pane.submitted) == 1 and "1 new alerts ->" in pane.submitted[0]
    assert state["wake_pending"] == 0


@pytest.mark.parametrize("attr", ["busy", "box_hidden"])
def test_a_busy_turn_keeps_the_answer_and_wakes_when_it_ends(pane, store, tmp_path, attr):
    _answer(store)
    setattr(pane, attr, True)
    state = {}
    fp.cycle(state, now=T0)
    fp.cycle(state, now=T0 + 60)
    assert len(_alerts(tmp_path)) == 1  # recorded once, not dropped
    assert pane.keys == [] and state["wake_pending"] == 1
    setattr(pane, attr, False)
    fp.cycle(state, now=T0 + 120)
    assert len(pane.submitted) == 1 and state["wake_pending"] == 0
    assert len(_alerts(tmp_path)) == 1


def test_background_agents_alone_do_not_defer_the_wake(pane, store, monkeypatch):
    """A pane waiting on background agents accepts input; only a running turn defers."""
    _answer(store)
    capture = pane.capture
    monkeypatch.setattr(fp, "capture", lambda _p=None: capture() + "  ◯ qa  Doing things…  3m 2s\n")
    fp.cycle({}, now=T0)
    assert len(pane.submitted) == 1


@pytest.mark.parametrize("kind, field", [("answered", "answer"), ("cancelled", "cancel_note")])
def test_answered_and_cancelled_alert_but_other_owner_kinds_do_not(pane, store, tmp_path, kind, field):
    extra = {"status": kind, field: "x", "cancelled_at": AT}
    _answer(store, **extra)
    (store / "items" / "W1.json").write_text(json.dumps(
        {"status": "wont_do", "wont_do": {"at": AT, "note": "no"}}))
    fp.cycle({}, now=T0)
    assert [a["fingerprint"].split("@")[0] for a in _alerts(tmp_path)] == ["D1"]
    assert kind in _alerts(tmp_path)[0]["detail"]


# --- criterion 2: bounded re-alert until consumed ----------------------------

def test_an_unconsumed_answer_realerts_at_the_interval_not_every_cycle(pane, store, tmp_path):
    _answer(store)
    state = {}
    for minute in range(0, 30):
        fp.cycle(state, now=T0 + minute * 60)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1
    fp.cycle(state, now=T0 + fp.REEMIT["owner_answer"])
    assert len(_alerts(tmp_path)) == 2 and len(pane.submitted) == 2


@pytest.mark.parametrize("consumed", [
    {"answer_note": "Relayed to the PM"},
    {"relayed": "2026-01-02T04:00:00Z S1"},
    {"status": "done", "answer_note": "Acknowledged"},
])
def test_a_consumed_answer_stops_alerting(pane, store, tmp_path, consumed):
    record = _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    (store / "items" / "D1.json").write_text(json.dumps({**record, **consumed}))
    for mult in (1, 2, 3):
        fp.cycle(state, now=T0 + mult * fp.REEMIT["owner_answer"])
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1


def test_an_acknowledged_note_on_an_answered_item_is_not_consumed(pane, store, tmp_path):
    """The repo's rule: only "Relayed" (or `relayed`) consumes status answered."""
    _answer(store, answer_note="Acknowledged, working on it")
    fp.cycle({}, now=T0)
    assert len(_alerts(tmp_path)) == 1


# --- criterion 3: a bad store fails open and visibly -------------------------

def _malformed(store):
    (store / "items" / "D1.json").write_text("{not json")


def _partial(store):
    path = store / "items" / "D1.json"
    path.write_text(path.read_text()[:20])


def _not_an_object(store):
    (store / "items" / "D1.json").write_text("[1, 2]")


def _bad_bytes(store):
    (store / "items" / "D1.json").write_bytes(b'\xff\xfe{"status"')


def _missing_items(store):
    (store / "items").rename(store / "items.away")


def _missing_root(store):
    store.rename(store.with_name("store.away"))


def _unreadable(store):
    os.chmod(store / "items", 0)


FAULTS = [_malformed, _partial, _not_an_object, _bad_bytes, _missing_items, _missing_root, _unreadable]


def _repair(store, record):
    """Undo any fault above and put the good document back."""
    away = store.with_name("store.away")
    if away.exists():
        away.rename(store)
    if (store / "items.away").exists():
        (store / "items.away").rename(store / "items")
    if (store / "items").exists():
        os.chmod(store / "items", 0o755)
    (store / "items" / "D1.json").write_text(json.dumps(record))


QUIET_FAULTS = [_missing_items, _missing_root]  # no store yet: not an install, log only
LOUD_FAULTS = [f for f in FAULTS if f not in QUIET_FAULTS]


def _errors(tmp_path):
    path = tmp_path / "alerts.jsonl"
    if not path.exists():
        return []
    return [a for a in map(json.loads, path.read_text().splitlines())
            if a["kind"] == "owner_answer_error"]


@pytest.mark.parametrize("fault", LOUD_FAULTS)
def test_a_failed_read_alerts_once_per_episode_and_wakes(pane, store, tmp_path, fault):
    record = _answer(store)
    fault(store)
    state = {}
    try:
        fp.cycle(state, now=T0)
        fp.cycle(state, now=T0 + 60)
        fp.cycle(state, now=T0 + 120)
        assert len(_errors(tmp_path)) == 1           # once, not one per cycle
        assert _alerts(tmp_path) == []               # no answer consumed or invented
        assert "owner answers ERROR" in _log(tmp_path)
        assert len(pane.submitted) == 1 and "1 new alerts ->" in pane.submitted[0]  # it woke
    finally:
        _repair(store, record)
    fp.cycle(state, now=T0 + 180)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 2  # delivery after repair
    assert len(_errors(tmp_path)) == 1


@pytest.mark.parametrize("fault", QUIET_FAULTS)
def test_a_store_that_is_not_there_yet_is_logged_not_alerted(pane, store, tmp_path, fault):
    record = _answer(store)
    fault(store)
    state = {}
    try:
        fp.cycle(state, now=T0)
        assert _errors(tmp_path) == [] and _alerts(tmp_path) == [] and pane.keys == []
        assert "owner answers ERROR" in _log(tmp_path)
    finally:
        _repair(store, record)
    fp.cycle(state, now=T0 + 60)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1


def test_a_store_that_vanishes_after_a_good_read_is_alerted(pane, store, tmp_path):
    record = _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    _missing_items(store)
    try:
        fp.cycle(state, now=T0 + 60)
        assert len(_errors(tmp_path)) == 1
    finally:
        _repair(store, record)


def test_the_error_alert_wakes_under_triage():
    """alert_triage always wakes an owner_* kind, so the failure is never only a log line."""
    triage = _load("alert_triage_r2", "alert_triage.py")
    verdict = triage.decide({"kind": "owner_answer_error", "session": "", "detail": "x"}, {}, T0, {})
    assert verdict["decision"] == "wake"


# --- a bad document outside items does not stop answers ----------------------

def test_a_corrupt_document_outside_items_does_not_stop_answers(pane, store, tmp_path):
    _answer(store)
    (store / "goals").mkdir()
    (store / "goals" / "G1.json").write_text("{not json")
    fp.cycle({}, now=T0)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1
    assert _errors(tmp_path) == [] and "owner answers ERROR" not in _log(tmp_path)


def test_a_corrupt_document_in_items_still_holds_the_answers_keys(pane, store, tmp_path):
    _answer(store)
    (store / "items" / "D2.json").write_text("{not json")
    state = {}
    fp.cycle(state, now=T0)
    assert _alerts(tmp_path) == [] and len(_errors(tmp_path)) == 1
    (store / "items" / "D2.json").unlink()
    fp.cycle(state, now=T0 + 60)
    assert len(_alerts(tmp_path)) == 1
    (store / "items" / "D2.json").write_text("{not json")
    fp.cycle(state, now=T0 + 120)
    assert any(k.startswith(fp.ANSWER_KEY) for k in state["alerts"])  # held, not cleared


@pytest.mark.parametrize("fault", FAULTS)
def test_a_bad_read_after_an_alert_neither_consumes_nor_resets_the_clock(pane, store, tmp_path, fault):
    """dedup clears a key absent from a cycle, so a failed read must not look like consumed:
    that would re-alert on the next good read, every time the store flaps."""
    record = _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    key = next(iter(state["alerts"]))
    fault(store)
    try:
        fp.cycle(state, now=T0 + 60)
        assert state["alerts"][key]["last"] == T0
        assert "owner answers ERROR" in _log(tmp_path)
        assert len(_errors(tmp_path)) == 1      # after a good read every fault alerts
    finally:
        _repair(store, record)
    fp.cycle(state, now=T0 + 120)
    assert len(_alerts(tmp_path)) == 1          # no duplicate on recovery
    fp.cycle(state, now=T0 + fp.REEMIT["owner_answer"])
    assert len(_alerts(tmp_path)) == 2          # and the retry is still due


def test_a_failed_read_keeps_other_kinds_on_their_own_dedup(pane, store):
    """Only answer keys are held; another kind absent from the cycle still clears."""
    state = {"alerts": {"||load|x": {"first": 1.0, "last": 1.0}}}
    _malformed(store)
    fp.cycle(state, now=T0)
    assert "||load|x" not in state["alerts"]


def test_a_dry_run_failed_read_prints_and_writes_nothing(pane, store, tmp_path, capsys):
    _malformed(store)
    fp.cycle({}, dry_run=True, now=T0)
    assert "owner answers ERROR" in capsys.readouterr().out
    assert _log(tmp_path) == ""


# --- the architect_do kind ---------------------------------------------------

def test_an_architect_do_answer_alerts_and_wakes(pane, store, tmp_path):
    _answer(store, answer="Architect Do", answer_note="run the migration")
    fp.cycle({}, now=T0)
    got = _alerts(tmp_path)
    assert [a["fingerprint"] for a in got] == [f"D1@{AT}"]
    assert "run the migration" in got[0]["display"] and len(pane.submitted) == 1


# --- a wake batch's timing during a running turn -----------------------------

def _other_alert():
    return fp.alert("permission_prompt", "x", "needs input", "tm-x", "%9", T0)


def test_a_batch_with_another_kind_does_not_wait_for_a_running_turn(pane, monkeypatch):
    """Pre-hotfix timing: only an owner_answer-only batch defers to a running turn."""
    pane.busy = True
    monkeypatch.setattr(fp, "read_events", lambda state: [_other_alert()])
    state = {}
    fp.cycle(state, now=T0)
    assert len(pane.submitted) == 1 and state["wake_pending"] == 0


def test_an_answer_batch_turns_early_once_another_kind_joins_it(pane, store, monkeypatch):
    _answer(store)
    pane.busy = True
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and state["wake_pending"] == 1  # answer alone waits
    monkeypatch.setattr(fp, "read_events", lambda s: [_other_alert()])
    fp.cycle(state, now=T0 + 60)
    assert len(pane.submitted) == 1             # now mixed: no wait


def test_an_answer_only_batch_still_waits_for_a_running_turn(pane, store):
    _answer(store)
    pane.busy = True
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and state["wake_pending"] == 1 and state["wake_pending_answers"] == 1


# --- no plain bottom rule under the box --------------------------------------

def test_an_empty_box_with_no_bottom_rule_skips_and_sends_no_keys(pane, store):
    _answer(store)
    pane.no_bottom_rule = True
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and pane.submitted == []
    assert state["wake_pending"] == 1


@pytest.mark.parametrize("box, continuation, footer", [
    ("half-typed reply", "", FOOTER),
    ("first row of a draft", "second row", FOOTER),
    ("", "a draft whose first row is blank", FOOTER),
    ("", "TM 2 is the plan", FOOTER),
    ("", "/rc", FOOTER),
    ("", "⏺ quoted", FOOTER),
    ("", "a draft on the screen's last row", []),
])
def test_a_draft_with_no_bottom_rule_is_still_never_touched(pane, store, box, continuation, footer):
    _answer(store)
    pane.no_bottom_rule, pane.footer = True, footer
    pane.box, pane.continuation = box, continuation
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and (pane.box, pane.continuation) == (box, continuation)
    assert state["wake_pending"] == 1


@pytest.mark.parametrize("box, continuation, dim", [
    ("suggested next prompt", "", True),
    ("suggested next prompt", "a draft on the next row", True),
    ("a typed draft", "", False),
    ("", "", False),
])
def test_no_bottom_rule_sends_no_keys_in_any_box_state(pane, store, box, continuation, dim):
    pane.no_bottom_rule, pane.dim = True, dim
    pane.box, pane.continuation = box, continuation
    result = fp.wake(1, fp.list_panes(), False)
    assert result.startswith("skipped: no bottom rule"), result
    assert pane.keys == []


def test_the_pending_wake_is_delivered_once_the_bottom_rule_returns(pane, store):
    _answer(store)
    pane.no_bottom_rule, pane.dim, pane.box = True, True, "suggested next prompt"
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and state["wake_pending"] == 1
    with open(fp.LOG) as f:
        assert "no bottom rule" in f.read()
    pane.no_bottom_rule = False
    fp.cycle(state, now=T0 + 60)
    assert len(pane.submitted) == 1 and state["wake_pending"] == 0


def test_a_wrapped_suggestion_under_a_plain_rule_still_wakes(pane, store):
    _answer(store)
    pane.dim, pane.box, pane.continuation = True, "suggested next prompt that", "wraps onto a second row"
    fp.cycle({}, now=T0)
    assert len(pane.submitted) == 1 and "1 new alerts ->" in pane.submitted[0]


def test_a_dim_row_over_an_undimmed_draft_is_never_typed_beside(pane, store):
    """#10670: one SGR 2 on the ❯ row made classify() call the whole box a suggestion."""
    _answer(store)
    pane.dim, pane.plain_continuation = True, True
    pane.box, pane.continuation = "suggested next prompt", "a real draft under it"
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == [] and state["wake_pending"] == 1


# #10670: synthetic -e captures in Claude Code's escape layout: grey (SGR 38;5;244) rules,
# a labelled top rule, SGR 39 before the ❯, a no-break space, SGR 2 text closed by SGR 0,
# and a bottom rule whose colour runs to the end. Text and label are invented.
GREY, LABELLED = "\x1b[38;5;244m", "─" * 60 + " demo-box ─"


def _box(*rows):
    return "\n".join([GREY + LABELLED, "\x1b[39m❯\xa0" + rows[0], *rows[1:], GREY + "─" * 72]) + "\n"


SUGGESTION_BOXES = {
    "single dim row": _box("\x1b[2mlist the open pull requests\x1b[0m"),
    "wrapped dim row, no escape of its own": _box("\x1b[2mlist the open pull requests", "and their checks\x1b[0m"),
    "dim with an inverse cursor cell": _box("\x1b[7ml\x1b[27;2mist the open pull requests\x1b[0m"),
    "dim after bold-off and a 256-colour fg": _box("\x1b[1;22;38;5;2;2mlist the open pull requests\x1b[0m"),
}
TYPED_BOXES = {
    "dim row above an undimmed draft": _box("\x1b[2mlist the open pull requests\x1b[0m", "my own draft"),
    "typed command with a dim ghost hint": _box("/review\x1b[2m <pr-number>\x1b[0m"),
    "typed text with an inverse cursor mid-text": _box("fix the\x1b[7m \x1b[27mlogin bug"),
    "typed text, cursor mid-text, dim hint after": _box("fix t\x1b[7mh\x1b[27me bug\x1b[2m and test it\x1b[0m"),
    "dim reset by SGR 22 mid-row": _box("\x1b[2mlist the\x1b[22m open pull requests\x1b[0m"),
    "inverse cells with no dim cell": _box("\x1b[7mmy own draft\x1b[27m"),
    "a 256-colour 2 is not dim":_box("\x1b[38;5;2mlist the open pull requests\x1b[0m"),
}


@pytest.mark.parametrize("name", SUGGESTION_BOXES)
def test_a_box_is_a_suggestion_only_when_every_cell_is_dim_or_inverse(name):
    assert fp.fleet_classify.box_is_suggestion(SUGGESTION_BOXES[name])


@pytest.mark.parametrize("name", TYPED_BOXES)
def test_a_typed_cell_anywhere_in_the_box_is_not_a_suggestion(name):
    assert not fp.fleet_classify.box_is_suggestion(TYPED_BOXES[name])


WAKE_BOXES = {**{n: (r, True) for n, r in SUGGESTION_BOXES.items()},
              "empty box with a cursor cell": (_box("\x1b[7m \x1b[27m"), True),
              **{n: (r, False) for n, r in TYPED_BOXES.items()}}


@pytest.mark.parametrize("name", WAKE_BOXES)
def test_a_suggestion_wakes_only_when_the_whole_box_is_dim(pane, store, name):
    """#10670: a typed cell anywhere in the box, on any row, holds the wake with no keys sent."""
    _answer(store)
    raw, wakes = WAKE_BOXES[name]
    pane.raw, pane.dim = raw, True  # typing over a suggestion replaces it
    state = {}
    fp.cycle(state, now=T0)
    if wakes:
        assert len(pane.submitted) == 1 and state["wake_pending"] == 0
    else:
        assert pane.keys == [] and state["wake_pending"] == 1


def test_a_rule_that_vanishes_after_typing_strands_the_pointer_without_enter(pane, store, monkeypatch):
    """The one stranded-pointer window wake()'s docstring names: the confirming read sees the
    footer as box text, so no Enter and no BSpace; the wake stays pending, the pointer stays."""
    _answer(store)
    real_send = pane.send

    def rule_goes(target, *keys):
        real_send(target, *keys)
        pane.no_bottom_rule = True

    monkeypatch.setattr(fp, "send_to_architect", rule_goes)
    state = {}
    fp.cycle(state, now=T0)
    assert pane.submitted == [] and not any(k[0] == "BSpace" for k in pane.keys)
    assert "1 new alerts ->" in pane.box and state["wake_pending"] == 1
    pane.no_bottom_rule = False
    fp.cycle(state, now=T0 + 60)  # the rule is back: its own pointer is recognised, never submitted
    assert pane.submitted == []
    assert "own stale pointer" in _log(store.parent)


def test_find_box_keeps_every_row_as_box_text_when_no_rule_closes_it():
    """No footer heuristic: a draft reading like a footer row is box text, last row included."""
    rows = ["⏺ Done.", RULE, "❯ ", "TM 2 is the plan", "/rc", "⏺ quoted", "last row"]
    assert fp.find_box(rows) == (1, len(rows))
    assert fp._norm("TM 2 is the plan/rc⏺ quotedlast row") == fp._box_full_text("\n".join(rows))


def test_a_stalled_wake_starts_no_silent_clock_and_a_sent_one_does(pane, store):
    """#10571's watchdog times silence from `wake_sent_at`: only a confirmed send sets it."""
    _answer(store)
    pane.no_bottom_rule = True
    state = {}
    fp.cycle(state, now=T0)
    assert "wake_sent_at" not in state
    pane.no_bottom_rule = False
    fp.cycle(state, now=T0 + 60)
    assert state["wake_sent_at"]


# --- DASHBOARD_ROOT and DASHBOARD_FORMAT through the real start script -------

def _forwarded_env(tmp_path, extra):
    """Run the real start-fleet-poll.sh against a fake tmux; return the -e pairs it forwards."""
    import test_start_fleet_poll as sfp
    _, calls = sfp._run(tmp_path, "exit 0", extra)
    tokens = calls[-1].split()
    return {t.split("=", 1)[0]: t.split("=", 1)[1]
            for prev, t in zip(tokens, tokens[1:]) if prev == "-e"}


def _jsonl_store(root, **extra):
    store_ = dashboard_store.Store(str(root), fmt="jsonl", actor="test")
    store_.set("items", "D1", {"kind": "question", "status": "answered", "title": "Pick",
                               "answer": "Option A", "answered_at": AT, **extra})


def test_a_jsonl_store_at_an_overridden_root_is_read_through_the_forward_list(
        pane, tmp_path, monkeypatch):
    root = tmp_path / "elsewhere"
    _jsonl_store(root)
    forwarded = _forwarded_env(tmp_path, {"DASHBOARD_ROOT": str(root), "DASHBOARD_FORMAT": "jsonl"})
    assert forwarded["DASHBOARD_ROOT"] == str(root) and forwarded["DASHBOARD_FORMAT"] == "jsonl"
    monkeypatch.delenv("DASHBOARD_ROOT")
    for k, v in forwarded.items():  # what the poller's own process inherits
        monkeypatch.setenv(k, v)
    fp.cycle({}, now=T0)
    assert [a["fingerprint"] for a in _alerts(tmp_path)] == [f"D1@{AT}"]
    assert len(pane.submitted) == 1


def test_a_jsonl_store_without_the_format_is_a_visible_failure(pane, tmp_path, monkeypatch):
    root = tmp_path / "elsewhere"
    _jsonl_store(root)
    forwarded = _forwarded_env(tmp_path, {"DASHBOARD_ROOT": str(root)})
    assert "DASHBOARD_FORMAT" not in forwarded
    monkeypatch.setenv("DASHBOARD_ROOT", forwarded["DASHBOARD_ROOT"])
    monkeypatch.delenv("DASHBOARD_FORMAT", raising=False)
    fp.cycle({}, now=T0)
    assert len(_errors(tmp_path)) == 1 and "set DASHBOARD_FORMAT=jsonl" in _log(tmp_path)


def test_the_default_dir_install_forwards_nothing_and_reads_the_default_root(
        pane, store, tmp_path, monkeypatch):
    monkeypatch.delenv("DASHBOARD_ROOT")
    monkeypatch.delenv("DASHBOARD_FORMAT", raising=False)
    forwarded = _forwarded_env(tmp_path, {})
    assert "DASHBOARD_ROOT" not in forwarded and "DASHBOARD_FORMAT" not in forwarded
    _answer(store)
    fp.cycle({}, now=T0)
    assert len(_alerts(tmp_path)) == 1


# --- criterion 4: a human's draft stays untouched ----------------------------

@pytest.mark.parametrize("box, continuation", [
    ("half-typed reply", ""),
    ("first row of a draft", "second row"),
    ("", "a draft whose first row is blank"),
])
def test_a_draft_is_never_typed_over_and_the_wake_waits(pane, store, tmp_path, box, continuation):
    _answer(store)
    pane.box, pane.continuation = box, continuation
    state = {}
    fp.cycle(state, now=T0)
    assert pane.keys == []                                  # no key of any kind
    assert (pane.box, pane.continuation) == (box, continuation)
    assert state["wake_pending"] == 1 and len(_alerts(tmp_path)) == 1
    pane.box, pane.continuation = "", ""
    fp.cycle(state, now=T0 + 60)
    assert len(pane.submitted) == 1 and state["wake_pending"] == 0


def test_a_draft_typed_as_the_wake_lands_is_kept_and_never_submitted(pane, store):
    """Preflight saw an empty box; the human's keys arrived first. The pointer comes back
    out and Enter is never pressed, so the draft is not submitted merged with it."""
    _answer(store)
    pane.typed_during_send = ("my reply", "before")
    state = {}
    fp.cycle(state, now=T0)
    assert pane.box == "my reply" and pane.submitted == []
    assert state["wake_pending"] == 1


def test_a_draft_typed_after_the_pointer_is_not_deleted_or_submitted(pane, store):
    """Backspacing here would eat the human's text, so nothing is removed."""
    _answer(store)
    pane.typed_during_send = ("my reply", "after")
    state = {}
    fp.cycle(state, now=T0)
    assert pane.box.endswith("my reply") and pane.submitted == []
    assert not any(k[0] == "BSpace" for k in pane.keys)
    assert state["wake_pending"] == 1


def test_an_unconfirmed_send_never_backspaces_into_other_text(pane, store, monkeypatch):
    _answer(store)
    real_send = pane.send

    def replaced_by_human(target, *keys):  # the pointer never lands; a draft is there instead
        if keys[0] == "-l":
            pane.keys.append(keys)
            pane.box = "my reply"
        else:
            real_send(target, *keys)

    monkeypatch.setattr(fp, "send_to_architect", replaced_by_human)
    state = {}
    fp.cycle(state, now=T0)
    assert pane.box == "my reply" and pane.submitted == []
    assert not any(k[0] == "BSpace" for k in pane.keys)


# --- criterion 5: cycles and restarts ----------------------------------------

def test_a_restart_neither_loses_nor_duplicates(pane, store, tmp_path):
    _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    fp.save_state(state)
    state = fp.load_state()
    fp.cycle(state, now=T0 + 60)
    fp.cycle(state, now=T0 + 120)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1
    fp.cycle(state, now=T0 + fp.REEMIT["owner_answer"])
    assert len(_alerts(tmp_path)) == 2


def test_a_restart_during_a_busy_turn_still_wakes_after_it(pane, store, tmp_path):
    _answer(store)
    pane.busy = True
    state = {}
    fp.cycle(state, now=T0)
    fp.save_state(state)
    pane.busy = False
    state = fp.load_state()
    fp.cycle(state, now=T0 + 60)
    assert len(_alerts(tmp_path)) == 1 and len(pane.submitted) == 1


def test_a_second_answer_on_the_same_item_alerts_again(pane, store, tmp_path):
    record = _answer(store)
    state = {}
    fp.cycle(state, now=T0)
    (store / "items" / "D1.json").write_text(json.dumps({**record, "answered_at": "2026-01-02T05:00:00Z"}))
    fp.cycle(state, now=T0 + 60)
    assert len(_alerts(tmp_path)) == 2


# --- the seed set carries the wiring -----------------------------------------

SEED_DIRS = [SCRIPTS] + ([SHIPPED] if os.path.isdir(SHIPPED) else [])


@pytest.mark.parametrize("scripts", SEED_DIRS)
def test_every_seed_copy_wires_the_answer_alerts_into_the_cycle(scripts):
    """A seed dropped over this one, or one that lost the wiring, fails here. #10571 split the
    poller, so the wiring spans the cycle, the alert step and the wake module."""
    def funcs(name):
        with open(os.path.join(scripts, name), encoding="utf-8") as f:
            tree = ast.parse(f.read())
        return {n.name: n for n in tree.body if isinstance(n, ast.FunctionDef)}

    called = {n.func.id for n in ast.walk(funcs("fleet-poll.py")["cycle"])
              if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)}
    assert "owner_answer_alerts" in called
    step = funcs("fleet_poll_alerts.py")["owner_answer_alerts"]
    assert "pending" in {n.attr for n in ast.walk(step) if isinstance(n, ast.Attribute)}
    wake = funcs("fleet_poll_wake.py")
    assert "defer_turn" in [a.arg for a in wake["wake"].args.args]
    assert "answer_count" in [a.arg for a in wake["dispatch_wake"].args.args]
    with open(os.path.join(scripts, "fleet-classify.py"), encoding="utf-8") as f:
        classify = f.read()
    assert '"owner_answer":' in classify
    assert os.path.isfile(os.path.join(scripts, "owner_answers.py"))


def test_the_answer_alert_reemits_every_1800s():
    assert fp.REEMIT["owner_answer"] == 1800

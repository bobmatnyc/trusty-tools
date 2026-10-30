"""Architect watchdog: dead/silent/ok classification, relaunch debounce, relauncher."""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


wd = _load("architect_watchdog", "architect-watchdog.py")
# #10161: the tests that drive fleet-poll.py's watchdog wiring
# (run_architect_relaunch, architect_busy, the frozen-alert debounce) ship
# with that poller in P1b.

S = "tm-architect"
SHELLS = {"zsh", "bash"}
LIVE = [{"session": S, "pane": "%1", "cmd": "claude"}]
SHELL = [{"session": S, "pane": "%1", "cmd": "zsh"}]
OTHER = [{"session": "tm-api", "pane": "%2", "cmd": "claude"}]
NOW = 1_000_000.0


def cls(panes, wake_at=None, mtime=None, now=NOW, limit=45):
    return wd.classify(panes, S, SHELLS, wake_at, mtime, now, limit)[0]


def run(book, panes, now=NOW, relauncher=None, wake_at=None, mtime=None, dry_run=False):
    calls = []

    def default():
        calls.append(1)
        return "ok", "x"
    alerts, phrase, relaunched = wd.step(
        book, panes, now, session=S, shells=SHELLS, wake_at=wake_at, transcript_mtime=mtime,
        silent_min=45, relauncher=relauncher or default, dry_run=dry_run)
    return [a[0] for a in alerts], phrase, relaunched, calls


def test_classify_dead_when_session_missing():
    assert cls(OTHER) == "dead"
    assert cls([]) == "dead"


def test_classify_dead_when_pane_at_shell():
    assert cls(SHELL) == "dead"
    assert cls([{"session": S, "pane": "%1", "cmd": "-zsh"}]) == "dead"


def test_classify_silent_after_deadline():
    wake = NOW - 46 * 60
    assert cls(LIVE, wake_at=wake, mtime=wake - 5) == "silent"
    assert cls(LIVE, wake_at=wake, mtime=None) == "silent"


def test_classify_ok_cases():
    wake = NOW - 46 * 60
    assert cls(LIVE) == "ok"                                   # no wake outstanding
    assert cls(LIVE, wake_at=NOW - 44 * 60, mtime=0) == "ok"   # inside the deadline
    assert cls(LIVE, wake_at=wake, mtime=wake + 1) == "ok"     # wrote since the wake


def test_step_dead_relaunches_once_then_debounces():
    book = {}
    kinds, phrase, relaunched, calls = run(book, SHELL)
    assert kinds == ["architect_dead"] and relaunched and len(calls) == 1
    kinds, phrase, relaunched, calls = run(book, OTHER, now=NOW + 600)
    assert not relaunched and not calls and "debounced" in phrase
    kinds, phrase, relaunched, calls = run(book, OTHER, now=NOW + 1801)
    assert relaunched and len(calls) == 1


def test_step_gives_up_after_two_failures():
    book = {}
    fail = lambda: ("failed", "boom")
    k1 = run(book, SHELL, now=NOW, relauncher=fail)[0]
    k2 = run(book, SHELL, now=NOW + 1801, relauncher=fail)[0]
    assert k1 == ["architect_dead"]
    assert k2 == ["architect_dead", "architect_relaunch_failed"]
    kinds, phrase, relaunched, calls = run(book, SHELL, now=NOW + 9000)
    assert "architect_relaunch_failed" in kinds and not relaunched and not calls
    assert "suspended" in phrase


def test_step_success_then_dead_counts_failure():
    book = {}
    run(book, SHELL, now=NOW)
    run(book, SHELL, now=NOW + 1801)       # relaunch "ok" last time, dead again: failure 1
    assert book["failures"] == 1
    kinds = run(book, SHELL, now=NOW + 3700)[0]
    assert kinds == ["architect_dead", "architect_relaunch_failed"]


def test_step_ok_resets_failures():
    book = {"failures": 1, "pending": True}
    assert run(book, LIVE)[0] == []
    assert book["failures"] == 0 and book["pending"] is False


def test_step_silent_never_relaunches():
    wake = NOW - 50 * 60
    kinds, phrase, relaunched, calls = run({}, LIVE, wake_at=wake, mtime=wake - 1)
    assert kinds == ["architect_silent"] and not relaunched and not calls


def test_step_dry_run_does_not_relaunch():
    book = {}
    kinds, phrase, relaunched, calls = run(book, SHELL, dry_run=True)
    assert kinds == ["architect_dead"] and "would relaunch" in phrase
    assert not calls and not relaunched and "last_relaunch" not in book


# --- frozen: a running turn with a stale transcript or a stuck spinner -------

SPIN = "✻ Running PreToolUse hook… (2m 20s)"


def frozen_cls(busy=True, spinner="", prev=None, mtime=None, wake_at=None):
    return wd.classify(LIVE, S, SHELLS, wake_at, mtime, NOW, 45, busy=busy,
                       spinner=spinner, prev_spinner=prev, frozen_min=15)[0]


def test_classify_frozen_by_stale_transcript():
    assert frozen_cls(mtime=NOW - 16 * 60) == "frozen"
    assert frozen_cls(mtime=NOW - 14 * 60) == "ok"          # inside FROZEN_MIN


def test_classify_frozen_by_identical_spinner():
    assert frozen_cls(spinner=SPIN, prev=SPIN, mtime=NOW - 10) == "frozen"


def test_classify_not_frozen_when_idle():
    assert frozen_cls(busy=False, spinner=SPIN, prev=SPIN, mtime=NOW - 3600) == "ok"


def test_classify_not_frozen_when_spinner_changes():
    assert frozen_cls(spinner=SPIN.replace("2m 20s", "2m 30s"), prev=SPIN,
                      mtime=NOW - 10) == "ok"
    assert frozen_cls(spinner=SPIN, prev=None, mtime=NOW - 10) == "ok"   # first poll


def run_frozen(book, now=NOW, spinner=SPIN, mtime=None, calls=None):
    alerts, phrase, relaunched = wd.step(
        book, LIVE, now, session=S, shells=SHELLS, wake_at=None, transcript_mtime=mtime,
        silent_min=45, relauncher=lambda: calls.append(1) or ("ok", "x"),
        busy=True, spinner=spinner, frozen_min=15)
    return [a[0] for a in alerts], phrase, relaunched


def test_step_frozen_never_relaunches():
    calls = []
    kinds, phrase, relaunched = run_frozen({}, mtime=NOW - 20 * 60, calls=calls)
    assert kinds == ["architect_frozen"] and not relaunched and not calls
    assert "no relaunch" in phrase


def test_step_frozen_by_spinner_needs_two_polls():
    book, calls = {}, []
    assert run_frozen(book, mtime=NOW - 5, calls=calls)[0] == []
    assert run_frozen(book, now=NOW + 600, mtime=NOW + 595, calls=calls)[0] == ["architect_frozen"]
    assert not calls

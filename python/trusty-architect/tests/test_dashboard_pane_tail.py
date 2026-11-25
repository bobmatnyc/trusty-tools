"""Pane-tail provider: dashboard_pane_tail.session_pane_tail (#11065 phase 2).

Covers: the n clamp and its type refusal; masking through mask_pane_text; the line cap, trailing blank rows and
`truncated`; exact-name session match; the capture depth and -J handed to capture_pane;
a secret an application hard-wrapped over two rows; the TTL cache; the error arm
(a failed capture is a PaneTailFailure, never an empty tail); and, end to end through the real fleet-poll.py with a
stub tmux, that no pane text lands on disk under the dashboard store, the inbox or the working directory.
Every fake secret is assembled at run time, so no literal in this file matches a secret scanner or the leak gate.
Run: python3 -m pytest tests/test_dashboard_pane_tail.py
Test: tests/test_dashboard_pane_tail.py
"""
import importlib.util
import os
import random
import string

import pytest

import dashboard_pane_tail as pt

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
_rng = random.Random(110652)


def rand(n):
    return "".join(_rng.choice(string.ascii_letters + string.digits) for _ in range(n))


class FakeTmuxError(Exception):
    pass


class FakePoller:
    """list_panes/capture_pane/TmuxError, the three names the provider reads from fleet-poll.py."""
    TmuxError = FakeTmuxError

    def __init__(self, text="", panes=None, fail=None, list_fail=None):
        self.text, self.fail, self.list_fail = text, fail, list_fail
        self.panes = panes if panes is not None else [{"session": "work", "pane": "%1", "cmd": "claude", "active": "11"}]
        self.calls = []

    def list_panes(self):
        if self.list_fail:
            raise FakeTmuxError(self.list_fail)
        return self.panes

    def capture_pane(self, p, **kw):
        self.calls.append((p["pane"], kw))
        if self.fail:
            raise FakeTmuxError(self.fail)
        return self.text


def provider(poller, now=1000.0):
    clock = {"t": now}
    return pt.PaneTailProvider(poller, clock=lambda: clock["t"]), clock


@pytest.mark.parametrize("n,want", [(None, 40), (-5, 10), (0, 10), (9, 10), (10, 10), (40, 40), (200, 200),
                                    (201, 200), (10_000, 200)])
def test_clamp_lines_clamps_into_ten_to_two_hundred(n, want):
    assert pt.clamp_lines(n) == want


@pytest.mark.parametrize("n", [True, "40", 40.0])
def test_clamp_lines_refuses_a_non_int(n):
    with pytest.raises(TypeError):
        pt.clamp_lines(n)


def test_default_is_forty_lines_and_older_lines_set_truncated():
    text = "\n".join(f"line {i}" for i in range(100))
    tail, _ = provider(FakePoller(text))
    out = tail.session_pane_tail("work")
    assert isinstance(out, pt.PaneTail)
    assert out.lines == tuple(f"line {i}" for i in range(60, 100))
    assert out.truncated is True and out.session == "work" and out.captured_at == 1000.0


def test_short_pane_drops_trailing_blank_rows_and_is_not_truncated():
    tail, _ = provider(FakePoller("$ ls\nfile\n\n   \n\n"))
    out = tail.session_pane_tail("work", 10)
    assert out.lines == ("$ ls", "file") and out.truncated is False


def test_each_line_is_cut_to_line_max():
    tail, _ = provider(FakePoller("x" * 1000))
    (line,) = tail.session_pane_tail("work").lines
    assert line == "x" * pt.LINE_MAX + "…"


def test_secrets_are_masked_and_escapes_stripped():
    tok, bearer = "ghp_" + rand(36), rand(40)
    # #11065: a row that ends in a secret also masks the row below it (hard wraps), so a blank row precedes "done".
    text = f"\x1b[31mpushing with {tok} now\x1b[0m\ncurl -H 'Authorization: Bearer {bearer}'\n\ndone"
    tail, _ = provider(FakePoller(text))
    joined = "\n".join(tail.session_pane_tail("work").lines)
    assert tok not in joined and bearer not in joined and "\x1b" not in joined
    assert "[masked:gh-token]" in joined and "[masked:auth]" in joined and joined.endswith("done")


def test_capture_depth_follows_the_clamped_n():
    poller = FakePoller("a")
    tail, _ = provider(poller)
    tail.session_pane_tail("work", 500)
    assert poller.calls == [("%1", {"history": 200, "join": True})]


def _windows(s, n=8):
    return {s[i:i + n] for i in range(len(s) - n + 1)}


def test_hard_wrapped_token_leaves_no_fragment():
    """#11065 critic repro: a 40-character token an application wrapped at column 20 kept its second row."""
    tok = "gh" + "p_" + rand(36)
    tail, _ = provider(FakePoller(f"{tok[:20]}\n{tok[20:]}\n$ ls"))
    joined = "\n".join(tail.session_pane_tail("work").lines)
    assert not [w for w in _windows(tok) if w in joined], joined
    assert joined == "[masked:gh-token]\n[masked:gh-token]\n$ ls"


def test_hard_wrapped_password_leaves_no_fragment():
    """#11065 critic repro: a key=value value wrapped onto the next row kept its last words."""
    tail, _ = provider(FakePoller("password: correct horse battery\nstaple tail\n$ ls"))
    joined = "\n".join(tail.session_pane_tail("work").lines)
    assert "staple" not in joined and "tail" not in joined and "battery" not in joined, joined
    assert joined.endswith("\n$ ls")


@pytest.fixture
def fleet_poll(monkeypatch):
    """fleet-poll.py with tmux replaced by a recorder; returns (module, recorded argument tuples)."""
    spec = importlib.util.spec_from_file_location("fleet_poll_capture_test", os.path.join(SCRIPTS, "fleet-poll.py"))
    fp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fp)
    calls = []
    monkeypatch.setattr(fp, "tmux", lambda *a, **k: calls.append((a, k)) or "")
    return fp, calls


def test_capture_default_still_issues_eighty_lines_without_join(fleet_poll):
    fp, calls = fleet_poll
    fp.capture("%1")
    assert calls == [(("capture-pane", "-e", "-p", "-S", "-80", "-t", "%1"), {})]


def test_capture_pane_join_adds_minus_j_on_the_listed_server(fleet_poll):
    fp, calls = fleet_poll
    fp.capture_pane({"pane": "%2", "server": "tm-ws-a"}, history=40, join=True)
    assert calls == [(("capture-pane", "-e", "-p", "-J", "-S", "-40", "-t", "%2"), {"server": "tm-ws-a"})]


def test_session_match_is_exact_and_prefers_the_active_pane():
    panes = [{"session": "work-2", "pane": "%9", "active": "11"},
             {"session": "work", "pane": "%2", "active": "10"},
             {"session": "work", "pane": "%3", "active": "11"}]
    poller = FakePoller("a", panes=panes)
    tail, _ = provider(poller)
    tail.session_pane_tail("work")
    assert poller.calls[0][0] == "%3"
    out = tail.session_pane_tail("wor")
    assert isinstance(out, pt.PaneTailFailure) and out.kind == pt.NOT_RUNNING


@pytest.mark.parametrize("kw", [{"fail": "capture-pane: can't find pane %1"}, {"list_fail": "no server running"}])
def test_failed_capture_is_a_typed_failure_never_an_empty_tail(kw):
    """#11065 fail-open check: an error turned into "" or an empty PaneTail would read as an idle pane."""
    tail, _ = provider(FakePoller("secret-free text", **kw))
    out = tail.session_pane_tail("work")
    assert isinstance(out, pt.PaneTailFailure), f"error arm returned {out!r}"
    assert out.kind == pt.CAPTURE_FAILED and out.detail and out.session == "work"


def test_failure_is_not_cached_and_recovery_is_seen_at_once():
    poller = FakePoller("back", fail="boom")
    tail, _ = provider(poller)
    assert isinstance(tail.session_pane_tail("work"), pt.PaneTailFailure)
    poller.fail = None
    assert tail.session_pane_tail("work").lines == ("back",)


def test_ttl_cache_serves_from_memory_then_expires():
    poller = FakePoller("one")
    tail, clock = provider(poller)
    first = tail.session_pane_tail("work")
    poller.text = "two"
    clock["t"] += pt.TTL - 0.1
    assert tail.session_pane_tail("work") is first and len(poller.calls) == 1
    clock["t"] += 0.2
    assert tail.session_pane_tail("work").lines == ("two",) and len(poller.calls) == 2


def test_non_str_name_is_refused():
    tail, _ = provider(FakePoller("a"))
    with pytest.raises(TypeError):
        tail.session_pane_tail(None)


def _files_holding(root, needles):
    """Paths under `root` whose bytes contain any needle."""
    hits = []
    for d, _, files in os.walk(root):
        for f in files:
            path = os.path.join(d, f)
            with open(path, "rb") as fh:
                data = fh.read()
            if any(n.encode() in data for n in needles):
                hits.append(path)
    return hits


def test_no_pane_text_is_persisted_under_store_inbox_or_cwd(tmp_path, tmp_path_factory, monkeypatch):
    """End to end: the real fleet-poll.py capture path with a stub tmux; afterwards no file holds the pane text."""
    stub = tmp_path_factory.mktemp("tmuxstub")   # outside the scanned tree: it holds the pane text itself
    sentinel, tok = "pane-sentinel-" + rand(8), "ghp_" + rand(36)
    (stub / "pane.txt").write_text(f"building\n{sentinel} ok\npush {tok}\n")
    (stub / "tmux").write_text(
        "#!/bin/sh\n"
        'echo "$*" >> "$PANE_STUB_DIR/args.log"\n'
        'case "$*" in\n'
        "  *list-panes*) printf 'work\\t%%1\\tclaude\\t11\\n' ;;\n"
        '  *capture-pane*) cat "$PANE_STUB_DIR/pane.txt" ;;\n'
        "  *) exit 1 ;;\n"
        "esac\n")
    os.chmod(stub / "tmux", 0o755)
    store, inbox, cwd = tmp_path / "dashroot", tmp_path / "inbox", tmp_path / "cwd"
    for d in (store, inbox, cwd):
        d.mkdir()
    monkeypatch.setenv("PATH", f"{stub}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("PANE_STUB_DIR", str(stub))
    monkeypatch.setenv("TMUX_TMPDIR", str(stub))          # no tm-ws-* sockets: the default server only
    monkeypatch.setenv("TMUX_SOCKET", "")
    monkeypatch.setenv("DASHBOARD_ROOT", str(store))
    monkeypatch.setenv("ARCHITECT_INBOX_DIR", str(inbox))
    monkeypatch.chdir(cwd)
    spec = importlib.util.spec_from_file_location("fleet_poll_pane_tail_test", os.path.join(SCRIPTS, "fleet-poll.py"))
    fp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fp)

    out = pt.PaneTailProvider(fp).session_pane_tail("work", 40)

    assert isinstance(out, pt.PaneTail), out
    assert f"{sentinel} ok" in out.lines and tok not in "\n".join(out.lines)
    assert "capture-pane -e -p -J -S -40 -t %1" in (stub / "args.log").read_text()
    control = tmp_path_factory.mktemp("control")
    (control / "planted").write_text(sentinel)
    assert _files_holding(control, [sentinel]), "the scan must be able to find a planted copy"
    assert _files_holding(tmp_path, [sentinel, tok, "building"]) == []

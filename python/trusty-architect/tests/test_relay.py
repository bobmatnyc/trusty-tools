"""Tests for scripts/relay.sh and scripts/posted.sh (#9997 AC3) against a
stub `tmux` first on PATH, so no tmux server is reached.

The stub logs each call, one argv per line. `capture-pane` prints the next
numbered capture file (`capture.1`, `capture.2`, ...; the last one repeats)
and fails when there is none, which is how a missing pane looks."""
import os
import subprocess

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")
RELAY = os.path.join(SCRIPTS, "relay.sh")
POSTED = os.path.join(SCRIPTS, "posted.sh")

STUB = """#!/bin/sh
echo "$*" >> "{dir}/tmux.log"
if [ -f "{dir}/fail_literal" ]; then
  case "$*" in *" -l "*) exit 1 ;; esac
fi
case "$1" in
  capture-pane)
    n=$(cat "{dir}/n" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "{dir}/n"
    f="{dir}/capture.$n"
    if [ ! -f "$f" ]; then f=$(ls "{dir}"/capture.* 2>/dev/null | sort -t. -k2 -n | tail -n 1); fi
    [ -n "$f" ] && [ -f "$f" ] || {{ echo "can't find pane" >&2; exit 1; }}
    cat "$f" ;;
esac
exit 0
"""

# A python3 that fails from its Nth call on (N in "{dir}/py_fail_from"); earlier
# calls run the real interpreter. Tests the box_state error arms of relay.sh.
FAKE_PY = """#!/bin/sh
n=$(cat "{dir}/py_n" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "{dir}/py_n"
if [ "$n" -ge "$(cat "{dir}/py_fail_from")" ]; then exit 1; fi
exec "{real}" "$@"
"""

RULE = "─" * 40
DIM, RESET = "\x1b[2m", "\x1b[0m"
MARK = "[Ada Architect 13:18Z]"
TEXT = f"{MARK} Architect answer: merge on green (basis: ruling D04)"


def box(text=""):
    """A pane capture whose input box holds `text`."""
    return "\n".join(["⏺ Merged.", RULE, f"❯ {text}", RULE, "  TM 1.7.4 | ctx 10%"]) + "\n"


class Stub:
    def __init__(self, tmp_path, captures):
        self.dir = tmp_path / "stub"
        (self.dir / "bin").mkdir(parents=True)
        tmux = self.dir / "bin" / "tmux"
        tmux.write_text(STUB.format(dir=self.dir))
        tmux.chmod(0o755)
        for i, text in enumerate(captures, 1):
            (self.dir / f"capture.{i}").write_text(text)
        self.cwd = tmp_path / "elsewhere"  # never the project root
        self.cwd.mkdir()

    def fail_literal_send(self):
        (self.dir / "fail_literal").write_text("")

    def fail_python_from(self, n):
        import shutil
        py = self.dir / "bin" / "python3"
        py.write_text(FAKE_PY.format(dir=self.dir, real=shutil.which("python3")))
        py.chmod(0o755)
        (self.dir / "py_fail_from").write_text(str(n))

    def run(self, script, *args):
        env = {k: v for k, v in os.environ.items()
               if k not in ("TMUX", "TMUX_SOCKET", "TMUX_PANE")}
        env["PATH"] = f"{self.dir / 'bin'}:{env.get('PATH', '')}"
        return subprocess.run(["bash", script, *args], env=env, cwd=self.cwd,
                              capture_output=True, text=True, timeout=30)

    def calls(self):
        log = self.dir / "tmux.log"
        return log.read_text().splitlines() if log.exists() else []

    def sends(self):
        return [c for c in self.calls() if c.startswith("send-keys")]


# --- relay.sh ----------------------------------------------------------------

@pytest.mark.parametrize("captures,state", [
    ([box("a draft the owner typed")], "typed"),
    ([], "missing"),
])
def test_relay_refuses_unless_the_box_is_empty_or_a_suggestion(tmp_path, captures, state):
    stub = Stub(tmp_path, captures)
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1, out
    assert out.stdout.startswith(f"tm-api SKIP input={state}"), out
    assert stub.sends() == []


def test_relay_types_then_refuses_when_the_box_lacks_the_marker(tmp_path):
    stub = Stub(tmp_path, [box(), box("someone else's text")])
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1, out
    assert "NOT-OURS" in out.stdout
    sends = stub.sends()
    assert len(sends) == 1 and sends[0].startswith("send-keys -t =tm-api: -l "), sends
    assert not any(s.endswith(" Enter") for s in sends)


def test_relay_refuses_a_box_that_still_shows_a_dim_suggestion(tmp_path):
    stub = Stub(tmp_path, [box(), box(f"{DIM}{TEXT}{RESET}")])
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1 and "NOT-OURS" in out.stdout, out
    assert not any(s.endswith(" Enter") for s in stub.sends())


@pytest.mark.parametrize("before", [box(), box(f"{DIM}merge it now{RESET}")])
def test_relay_sends_enter_only_after_both_checks(tmp_path, before):
    stub = Stub(tmp_path, [before, box(TEXT)])
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 0, out
    assert out.stdout.startswith("tm-api sent "), out
    sends = stub.sends()
    assert sends == [f"send-keys -t =tm-api: -l {TEXT}", "send-keys -t =tm-api: Enter"]
    captures = [c for c in stub.calls() if c.startswith("capture-pane")]
    assert len(captures) == 2 and all("-t =tm-api:" in c for c in captures)


# #10780: python3 call 1 is relay_provenance.py, 2 and 3 are the two box classifications.
@pytest.mark.parametrize("fail_from,expect", [(1, "PROVENANCE input=error"), (2, "SKIP input=error"),
                                              (3, "NOT-OURS input=error")])
def test_relay_fails_closed_when_the_classifier_exits_nonzero(tmp_path, fail_from, expect):
    stub = Stub(tmp_path, [box(), box(TEXT)])
    stub.fail_python_from(fail_from)
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1, out
    assert expect in out.stdout, out
    assert not any(s.endswith(" Enter") for s in stub.sends())


def test_relay_reports_not_ours_when_the_second_capture_is_empty(tmp_path):
    stub = Stub(tmp_path, [box(), ""])
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1 and "NOT-OURS" in out.stdout, out
    assert not any(s.endswith(" Enter") for s in stub.sends())


def test_relay_sends_no_enter_when_typing_the_text_fails(tmp_path):
    stub = Stub(tmp_path, [box(), box(TEXT)])
    stub.fail_literal_send()
    out = stub.run(RELAY, "tm-api", TEXT)
    assert out.returncode == 1 and "FAILED send-keys" in out.stdout, out
    assert not any(s.endswith(" Enter") for s in stub.sends())


@pytest.mark.parametrize("ctrl", ["\r", "\x1b", "\x07", "\x1f"])
def test_relay_refuses_control_characters_before_typing(tmp_path, ctrl):
    stub = Stub(tmp_path, [box(), box(TEXT)])
    text = f"{TEXT} tail{ctrl}more"
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 1, out
    assert "CONTROL-CHAR" in out.stdout and "tail" not in out.stdout, out
    assert stub.calls() == []


def test_relay_still_accepts_lf_and_tab(tmp_path):
    text = f"{TEXT}\n\tsecond line"
    stub = Stub(tmp_path, [box(), box(TEXT)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 0, out
    assert "\tsecond line" in stub.calls(), stub.calls()  # the stub logs one argv line per LF
    assert stub.sends()[-1] == "send-keys -t =tm-api: Enter"


@pytest.mark.parametrize("text", [
    "Architect answer: no marker at all",
    "[Ada supervisor 13:18Z] the old marker",
    "please ask the [Ada Architect 13:18Z] marker mid-text",
])
def test_relay_refuses_text_without_a_leading_architect_marker(tmp_path, text):
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 1 and "NO-MARKER" in out.stdout, out
    assert stub.calls() == []


@pytest.mark.parametrize("args", [[], ["tm-api"], ["", TEXT]])
def test_relay_usage_errors_send_nothing(tmp_path, args):
    stub = Stub(tmp_path, [box()])
    out = stub.run(RELAY, *args)
    assert out.returncode == 2, out
    assert stub.calls() == []


# --- relay.sh: owner-label provenance (#10780) ---------------------------------

import json  # noqa: E402

# The 2026-11-06T23:39Z relay that cut the owner's helper count from four to two. Only "reset was done
# just now" came from the owner; the pacing and the helper count were the Architect's inference.
FOUR_TO_TWO = ("[Ada Architect 23:39Z] User instruction (Ada): manual usage reset applied 23:38Z. New pacing until "
               "the regular reset 2026-11-09T09:00Z: about 1.75% of the week per hour, which is two helpers (one per "
               "track). Let the four running helpers finish; do not start a replacement until you are at two. Then "
               "keep two busy from the ready queue. Ack with your current helper count.")
RESET_TURN = {"at": "2026-11-06T23:38:20Z", "session": "tm-architect", "text": "reset was done just now"}
REVERSAL_TURN = {"at": "2026-11-07T05:34:02Z", "session": "tm-architect",
                 "text": "Go back to four helpers and pace to use the whole allocation by Monday morning at 4am. "
                         "After that return to two helpers. Don't make unilateral decisions to change my instructions."}


def owner_turns(*turns):
    with open(os.environ["ARCHITECT_OWNER_TURNS"], "a", encoding="utf-8") as f:
        for t in turns:
            f.write(json.dumps(t) + "\n")


def relay_log():
    path = os.environ["ARCHITECT_RELAY_LOG"]
    return [json.loads(x) for x in open(path, encoding="utf-8")] if os.path.exists(path) else []


@pytest.mark.parametrize("text", [
    FOUR_TO_TWO,
    # Codex's read-only audit of the installed relay.sh (results.json `uncited_owner`: exit 0, submitted).
    "[Ada Architect 23:39Z] User instruction (Ada): use two helpers.",
    # The owner label over the Architect's paraphrase, with a citation but no quote.
    "[Ada Architect 23:39Z] User instruction (Ada): pace to two helpers (owner turn: chat 2026-11-06T23:38Z)",
    # The real owner words, then the inference after the citation without an Architect label.
    '[Ada Architect 23:39Z] User instruction (Ada): "reset was done just now" (owner turn: chat 2026-11-06T23:38Z)'
    " which is two helpers, one per track.",
    # A quote the cited turn does not hold (audit `irrelevant_citation`: the turn said only "reset done").
    '[Ada Architect 23:39Z] User instruction (Ada): "two helpers until Monday" (owner turn: chat 2026-11-06T23:38Z)',
])
def test_relay_refuses_an_owner_label_without_the_owners_cited_words_10780(tmp_path, text):
    owner_turns(RESET_TURN)
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 3, out
    assert out.stdout.startswith("tm-api NO-OWNER-CITE: User instruction (Ada):"), out
    assert "Architect answer: ... (basis: ...)" in out.stdout, out
    assert stub.calls() == []   # refused before the box was read or typed into
    assert [r["outcome"] for r in relay_log()] == ["refused"]


def test_relay_refuses_a_citation_to_an_unrecorded_turn_10780(tmp_path):
    text = '[Ada Architect 23:39Z] User ruling (Ada): "use two helpers" (owner turn: chat 2026-11-06T22:00Z)'
    owner_turns(RESET_TURN)
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 3 and "no owner turn recorded" in out.stdout, out
    assert stub.calls() == []


def _sent_and_logged(tmp_path, text, cite, outcome="verified"):
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 0 and out.stdout.startswith("tm-api sent "), out
    assert stub.sends()[-1] == "send-keys -t =tm-api: Enter"
    logged = relay_log()
    assert [(r["outcome"], r["cite"], r["session"]) for r in logged] == [(outcome, cite, "tm-api")], logged
    return logged[0]


def test_relay_sends_an_owner_reversal_quoted_and_cited_10780(tmp_path):
    """False-refusal guard: the owner's 05:34Z reversal of the two-helper relay, with the inference labelled."""
    owner_turns(RESET_TURN, REVERSAL_TURN)
    text = ('[Ada Architect 05:34Z] User ruling (Ada): "Go back to four helpers and pace to use the whole allocation '
            'by Monday morning at 4am." "After that return to two helpers." (owner turn: chat 2026-11-07T05:34Z) '
            "Architect note: this reverses my 23:39Z relay, which was my inference, not a ruling.")
    rec = _sent_and_logged(tmp_path, text, "chat 2026-11-07T05:34Z")
    assert rec["label"] == "User ruling (Ada)" and len(rec["quotes"]) == 2


def test_relay_sends_an_expired_ruling_relayed_as_history_10780(tmp_path):
    """False-refusal guard: a superseded dashboard ruling, cited, relayed as history (expired: superseded by a
    later owner turn). The label says it is history; the in-force ruling is named after an Architect label."""
    import dashboard_store as ds
    ds.Store(os.environ["DASHBOARD_ROOT"]).set("items", "D70", {
        "kind": "question", "status": "answered", "title": "helpers this weekend",
        "answer": "Keep one helper on the rules and one on the restructure.",
        "answer_note": "Relayed 2026-11-05T23:15Z tm-api"})
    text = ('[Ada Architect 06:10Z] User ruling (Ada, 2026-11-05, superseded 2026-11-06T20:48Z): "Keep one helper on '
            'the rules and one on the restructure." (owner turn: dashboard D70) Architect note: history only; the '
            "20:48Z four-helper ruling is in force.")
    _sent_and_logged(tmp_path, text, "dashboard D70")


def test_relay_never_takes_the_architects_relayed_note_as_owner_words_10780(tmp_path):
    import dashboard_store as ds
    ds.Store(os.environ["DASHBOARD_ROOT"]).set("items", "D71", {
        "kind": "question", "status": "answered", "title": "pace", "answer": "yes",
        "answer_note": "Relayed 2026-11-06T23:39Z: two helpers"})
    text = '[Ada Architect 06:10Z] User ruling (Ada): "two helpers" (owner turn: dashboard D71)'
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 3 and "is not in the owner turn" in out.stdout, out


class FakeMemory:
    """A trusty-memory stand-in on a Unix socket: answers `memory.drawers_list` with `drawers`."""

    def __init__(self, monkeypatch, drawers):
        import socket
        import tempfile
        import threading
        self.dir = tempfile.mkdtemp(dir="/tmp")   # a short path: AF_UNIX caps it near 104 bytes
        self.path = os.path.join(self.dir, "m.sock")
        self.calls = []
        self.srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.srv.bind(self.path)
        self.srv.listen(4)
        self.drawers = drawers
        threading.Thread(target=self._serve, daemon=True).start()
        monkeypatch.setenv("ARCHITECT_MEMORY_SOCKET", self.path)

    def _serve(self):
        while True:
            try:
                conn, _ = self.srv.accept()
            except OSError:
                return
            with conn:
                buf = b""
                while not buf.endswith(b"\n"):
                    chunk = conn.recv(65536)
                    if not chunk:
                        break
                    buf += chunk
                req = json.loads(buf)
                self.calls.append((req["method"], req["params"]["palace_id"]))
                conn.sendall((json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": self.drawers}) + "\n").encode())

    def close(self):
        self.srv.close()
        import shutil
        shutil.rmtree(self.dir, ignore_errors=True)


HOLD_DRAWER = {"id": "a5f3053b-1c2d-4e5f-8a9b-0c1d2e3f4a5b", "content": "Ruling (owner, 2026-11-07): hold all merges until I am back."}


@pytest.mark.parametrize("text,cite,outcome", [
    # A resource HOLD on the Architect's own authority: no owner label, so no citation is owed.
    ("[Ada Architect 08:55Z] Architect answer: HOLD new dispatches until the 09:00Z reset; usage is 97% "
     "(basis: usage read 08:54Z; standing rule on the reset window)", None, None),
    # The owner's own HOLD, cited to a palace drawer (the fallback when the turn is not in the record).
    ('[Ada Architect 08:55Z] User ruling (Ada): "hold all merges until I am back" (owner turn: palace architect:a5f3053b)',
     "palace architect:a5f3053b", "verified"),
])
def test_relay_sends_a_resource_hold_10780(tmp_path, monkeypatch, text, cite, outcome):
    """False-refusal guard: a HOLD sent as an Architect answer, or as a cited owner HOLD."""
    mem = FakeMemory(monkeypatch, [HOLD_DRAWER, {"id": "0b54f115-0000-0000-0000-000000000000", "content": "x"}])
    if cite is not None:
        _sent_and_logged(tmp_path, text, cite, outcome)
        assert mem.calls == [("memory.drawers_list", "architect")]
        mem.close()
        return
    mem.close()
    if cite is None:
        stub = Stub(tmp_path, [box(), box(text)])
        out = stub.run(RELAY, "tm-api", text)
        assert out.returncode == 0 and out.stdout.startswith("tm-api sent "), out
        assert relay_log() == []   # nothing owner-labelled to record
        assert "NO-OWNER-CITE" not in out.stdout
        # The pre-#10780 script also sent this one; the line below is what it could not do: tell it apart.
        assert "Architect answer:" in stub.sends()[0]
    else:
        _sent_and_logged(tmp_path, text, cite, outcome)


@pytest.mark.parametrize("drawers,cite,why", [
    (None, "palace deadbeef", "did not answer"),                 # trusty-memory unreachable: fail closed
    ([HOLD_DRAWER], "palace deadbeef", "holds no drawer"),        # any hex no longer passes
    ([dict(HOLD_DRAWER, content="merge everything now")], "palace architect:a5f3053b", "is not in the owner turn"),
])
def test_relay_refuses_an_unchecked_palace_cite_10780(tmp_path, monkeypatch, drawers, cite, why):
    """#10780 critic H2: a palace cite passes only when trusty-memory holds the drawer and the quote."""
    monkeypatch.setenv("ARCHITECT_MEMORY_PALACE", "architect")
    mem = FakeMemory(monkeypatch, drawers) if drawers is not None else None
    text = f'[Ada Architect 08:55Z] User ruling (Ada): "hold all merges until I am back" (owner turn: {cite})'
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    if mem:
        mem.close()
    assert out.returncode == 3 and why in out.stdout, out
    assert stub.calls() == []


@pytest.mark.parametrize("label", [
    "User instruction: two helpers",
    "user instruction (Ada): two helpers",
    "User Instruction (Ada): two helpers",
    "User instruction(Ada): two helpers",
    "User instruction (Ada) : two helpers",
    "User\u00a0instruction (Ada): two helpers",
    "User instruction (Ada)\uff1a two helpers",
    "Owner ruling (Ada): two helpers",
    "User decision (Ada): two helpers",
    'Pacing to two helpers per User instruction (Ada): "reset was done just now" (owner turn: chat 2026-11-06T23:38Z)',
])
def test_relay_refuses_an_owner_claim_in_any_other_spelling_10780(tmp_path, label):
    """#10780 critic H1: only the exact labels make an owner claim, and no lead text precedes one unlabelled."""
    owner_turns(RESET_TURN)
    text = f"[Ada Architect 23:39Z] {label}"
    stub = Stub(tmp_path, [box(), box(text)])
    out = stub.run(RELAY, "tm-api", text)
    assert out.returncode == 3 and "NO-OWNER-CITE" in out.stdout, out
    assert stub.calls() == []


def test_relay_allows_an_architect_label_before_the_owner_label_10780(tmp_path):
    owner_turns(RESET_TURN)
    text = ('[Ada Architect 23:39Z] Architect note: for the record. User instruction (Ada): "reset was done just now" '
            "(owner turn: chat 2026-11-06T23:38Z)")
    _sent_and_logged(tmp_path, text, "chat 2026-11-06T23:38Z")


# --- posted.sh ---------------------------------------------------------------

SHARED = "\n".join([
    "❯ [Ops Architect 13:10Z] User ruling (Ada): take option A",
    "⏺ Acknowledged.",
    "› Message from @9-28-26: [Lin Architect 13:20Z] Architect answer: merge on green",
    "⏺ Done.",
    "❯ [Ada Architect 13:25Z] Architect question: which branch?",
    "⏺ main.",
    RULE,
    "❯ ",
    RULE,
]) + "\n"


def test_posted_lists_relays_at_or_after_the_time_and_exits_1(tmp_path):
    stub = Stub(tmp_path, [SHARED])
    out = stub.run(POSTED, "tm-api", "13:20Z")
    assert out.returncode == 1, out
    assert "[Lin Architect 13:20Z]" in out.stdout and "[Ada Architect 13:25Z]" in out.stdout
    assert "13:10Z" not in out.stdout
    assert stub.calls() == ["capture-pane -p -J -t =tm-api: -S -400"]


def test_posted_prints_none_and_exits_0_when_no_relay_followed(tmp_path):
    stub = Stub(tmp_path, [SHARED])
    out = stub.run(POSTED, "tm-api", "13:30Z")
    assert (out.returncode, out.stdout.strip()) == (0, "NONE"), out


def test_posted_excludes_the_callers_own_relays(tmp_path):
    stub = Stub(tmp_path, [SHARED])
    out = stub.run(POSTED, "tm-api", "13:21Z", "Ada")
    assert (out.returncode, out.stdout.strip()) == (0, "NONE"), out
    out = stub.run(POSTED, "tm-api", "13:21Z", "Lin")
    assert out.returncode == 1 and "[Ada Architect 13:25Z]" in out.stdout, out


@pytest.mark.parametrize("args", [["tm-api"], ["tm-gone", "13:00Z"]])
def test_posted_never_says_none_when_it_cannot_check(tmp_path, args):
    """A usage error or a capture failure exits 2 and prints no NONE: the
    caller must not read either as permission to send."""
    stub = Stub(tmp_path, [])
    out = stub.run(POSTED, *args)
    assert out.returncode == 2, out
    assert "NONE" not in out.stdout

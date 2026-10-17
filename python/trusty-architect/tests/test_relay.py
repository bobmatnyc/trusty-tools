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


@pytest.mark.parametrize("fail_from,expect", [(1, "SKIP input=error"), (2, "NOT-OURS input=error")])
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

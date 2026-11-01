"""self-clear.sh readiness gate: refuses (non-zero, no /clear) unless `datg.py ready` exits 0, before and after the idle wait (#10599)."""
import os
import shutil
import stat
import subprocess

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
SCRIPT = os.path.join(SCRIPTS, "self-clear.sh")

# #10599: a datg.py whose old fresh+digest checks pass, so only `ready` decides.
_DIGEST = "IL DIGEST x\nSESSION\n  task: t\n  next: n"
# turn-active.py's contract (#10599): 0 running, 10 idle, 3 internal error, anything else busy.
IDLE = "import sys\nsys.exit(10)\n"
REASONS = "what is owed is not recorded\nthe scheduled jobs are not recorded"


def _install(tmp_path):
    """#10599: self-clear.sh finds its Architect dir from its own path, so each
    test runs a copy placed in tmp_path/scripts/, beside the fakes."""
    dest = tmp_path / "scripts" / "self-clear.sh"
    shutil.copy(SCRIPT, dest)
    return str(dest)


def _fake_datg(ready, reasons="", flip_after=None):
    """A datg.py stub: `ready` prints `reasons` and exits `ready`; with
    `flip_after`, calls after the first `flip_after` exit `flip_after`'s code
    1 instead. `fresh` exits 0 and `digest` prints a recorded SESSION block."""
    return (
        "import os, sys\n"
        "if 'ready' in sys.argv:\n"
        f"    n = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ready.count')\n"
        "    c = (int(open(n).read()) if os.path.exists(n) else 0) + 1\n"
        "    open(n, 'w').write(str(c))\n"
        f"    if {reasons!r}: print({reasons!r})\n"
        f"    sys.exit(1 if {flip_after!r} is not None and c > {flip_after!r} else {ready})\n"
        "if 'fresh' in sys.argv: sys.exit(0)\n"
        f"print({_DIGEST!r})\n"
    )


def _arch(tmp_path, datg, turn_active=IDLE):
    """Lay out tmp_path as an Architect: fakes in scripts/, inbox/, a fake tmux
    that logs its argv. `turn_active` None leaves the helper missing."""
    s = tmp_path / "scripts"
    s.mkdir()
    (tmp_path / "inbox").mkdir()
    (s / "datg.py").write_text(datg)
    if turn_active is not None:
        (s / "turn-active.py").write_text(turn_active)
    (s / "architect-idle.py").write_text("import sys\nsys.exit(0)\n")
    (s / "input-state.py").write_text("print('p empty')\n")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    tmux = bindir / "tmux"
    tmux.write_text(f'#!/bin/sh\necho "$@" >> {tmp_path}/tmux.log\n')
    tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
    return dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}",
                SELF_CLEAR_POLL="0.05", SELF_CLEAR_SETTLE="0.05", SELF_CLEAR_ECHO="0.05")


def _live(tmp_path, datg, turn_active=IDLE, wait=6):
    """Run `self-clear.sh --live`; return (exit code or None on timeout, tmux log, self-clear log)."""
    env = _arch(tmp_path, datg, turn_active)
    try:
        r = subprocess.run(["bash", _install(tmp_path), "--live"], env=env,
                           capture_output=True, text=True, timeout=wait)
        code = r.returncode
    except subprocess.TimeoutExpired:
        code = None  # still waiting for idle, which is the busy answer
    read = lambda p: p.read_text() if p.exists() else ""
    return code, read(tmp_path / "tmux.log"), read(tmp_path / "inbox" / "self-clear.log")


def test_a_partial_session_refuses_and_dry_run_logs_the_reasons(tmp_path):
    """#10599 (a): a session holding only task+next is not ready; the dry run exits non-zero and logs why."""
    env = _arch(tmp_path, _fake_datg(1, REASONS))
    r = subprocess.run(["bash", _install(tmp_path)], env=env, capture_output=True, text=True, timeout=20)
    log = (tmp_path / "inbox" / "self-clear.log").read_text()
    assert r.returncode == 4, r.stdout + r.stderr
    assert "what is owed is not recorded" in log and "the scheduled jobs are not recorded" in log
    assert not (tmp_path / "tmux.log").exists()


@pytest.mark.parametrize("ready", [
    1,  # (b) hold-only: not ready
    2,  # (c) IL missing or unreadable
    3,  # any other code
])
def test_a_ready_failure_sends_no_clear(tmp_path, ready):
    code, tmux, _ = _live(tmp_path, _fake_datg(ready))
    assert code == 4
    assert "/clear" not in tmux


def test_ready_flipping_during_the_idle_wait_sends_no_clear(tmp_path):
    """#10599 (d): the post-idle recheck refuses when the IL stops being ready."""
    code, tmux, log = _live(tmp_path, _fake_datg(0, flip_after=1))
    assert code == 4, log
    assert "/clear" not in tmux
    assert "no longer ready after the idle wait" in log


def test_ready_and_turn_active_idle_sends_clear(tmp_path):
    """#10599 (e) control: ready exits 0 and turn-active exits 10, so /clear goes out."""
    _, tmux, log = _live(tmp_path, _fake_datg(0))
    assert "-l /clear" in tmux, log


@pytest.mark.parametrize("turn_active", [
    None,                          # helper missing
    "import sys\nsys.exit(1)\n",   # (f) a crash exits 1
    "import sys\nsys.exit(2)\n",
    "import sys\nsys.exit(3)\n",   # internal error
    "import sys\nsys.exit(0)\n",   # running
])
def test_a_turn_active_answer_other_than_10_is_busy(tmp_path, turn_active):
    """#10599: only turn-active.py's idle exit (10) counts as idle."""
    _, tmux, _ = _live(tmp_path, _fake_datg(0), turn_active)
    assert "/clear" not in tmux


def test_the_old_fresh_and_digest_path_cannot_allow_a_clear(tmp_path):
    """#10599 (g): fresh exits 0 and the digest records SESSION, but `ready` exits 1: no clear."""
    code, tmux, _ = _live(tmp_path, _fake_datg(1))
    assert code == 4
    assert "/clear" not in tmux


def test_resume_sent_after_clear_when_transcript_check_reports_unknown(tmp_path):
    """2026-10-21 RCA: a freshly cleared transcript made architect-idle.py exit 2 forever, so the resume was never sent."""
    env = _arch(tmp_path, _fake_datg(0))
    s = tmp_path / "scripts"
    # transcript idle before the clear (no Enter sent yet), unknown (exit 2) afterwards
    (s / "architect-idle.py").write_text(
        "import os, sys\nlog = %r\nsent = os.path.exists(log) and 'Enter' in open(log).read()\nsys.exit(2 if sent else 0)\n"
        % str(tmp_path / "tmux.log"))
    # box: typed text between a literal send and its Enter, else empty
    (s / "input-state.py").write_text(
        "import os\nlines = open(%r).read().splitlines() if os.path.exists(%r) else []\n"
        "last = [l for l in lines if l.startswith('send-keys')]\n"
        "if last and ' -l ' in last[-1]:\n    print('p typed |', last[-1].split(' -l ', 1)[1])\nelse:\n    print('p empty')\n"
        % (str(tmp_path / "tmux.log"), str(tmp_path / "tmux.log")))
    r = subprocess.run(["bash", _install(tmp_path), "--live"], env=env, capture_output=True, text=True, timeout=60)
    log = (tmp_path / "inbox" / "self-clear.log").read_text()
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert "sent /clear" in log and "sent resume prompt" in log
    assert "not idle after clear" not in log


def test_default_is_dry_run_and_sends_no_keys(tmp_path):
    env = _arch(tmp_path, _fake_datg(0))
    r = subprocess.run(["bash", _install(tmp_path)], env=env, capture_output=True, text=True, timeout=20)
    assert r.returncode == 0 and "dry run" in r.stdout, r.stdout + r.stderr
    assert not (tmp_path / "tmux.log").exists()


def test_sourced_from_another_cwd_resolves_root_to_the_architect_dir(tmp_path):
    """#10599: under `source`, $0 is the caller's shell; the root must still be the script's own Architect dir."""
    arch = tmp_path / "arch"
    (arch / "scripts").mkdir(parents=True)
    (arch / "inbox").mkdir()
    (arch / "scripts" / "datg.py").write_text(_fake_datg(1, REASONS))
    script = arch / "scripts" / "self-clear.sh"
    shutil.copy(SCRIPT, script)
    cwd = tmp_path / "elsewhere" / "sub"
    cwd.mkdir(parents=True)
    r = subprocess.run(["bash", "-c", '. "$1"', "bash", str(script)], cwd=cwd,
                       capture_output=True, text=True, timeout=20)
    assert r.returncode == 4, r.stdout + r.stderr
    assert "what is owed is not recorded" in (arch / "inbox" / "self-clear.log").read_text()

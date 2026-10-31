"""self-clear.sh freshness gate: refuses (non-zero, no tmux call) unless the IL is fresh and its SESSION block is recorded."""
import os
import shutil
import stat
import subprocess

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
# #10161: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
SCRIPT = os.path.join(SCRIPTS, "self-clear.sh")


def _install(tmp_path):
    """#10599: self-clear.sh finds its Architect dir from its own path, so each
    test runs a copy placed in tmp_path/scripts/, beside the fakes."""
    dest = tmp_path / "scripts" / "self-clear.sh"
    shutil.copy(SCRIPT, dest)
    return str(dest)


def _fake_datg(fresh_code, digest):
    return (
        "import sys\n"
        f"if 'fresh' in sys.argv: sys.exit({fresh_code})\n"
        f"print({digest!r})\n"
    )


def _run(tmp_path, fresh_code, digest, wait=60):
    (tmp_path / "scripts").mkdir()
    (tmp_path / "inbox").mkdir()
    (tmp_path / "scripts" / "datg.py").write_text(_fake_datg(fresh_code, digest))
    bindir = tmp_path / "bin"
    bindir.mkdir()
    tmux = bindir / "tmux"
    tmux.write_text(f'#!/bin/sh\necho "$@" >> {tmp_path}/tmux.log\nexit 1\n')
    tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    try:
        r = subprocess.run(["bash", _install(tmp_path), "--live"], env=env, capture_output=True, text=True, timeout=wait)
    except subprocess.TimeoutExpired:
        r = None  # past the gate the idle loop polls for up to 30 minutes
    return r, (tmp_path / "tmux.log").exists()


@pytest.mark.parametrize("fresh_code,digest", [
    (1, "IL DIGEST x\nSESSION\n  task: t\n  next: n"),   # stale
    (2, "IL DIGEST x\nSESSION\n  task: t\n  next: n"),   # unreadable
    (0, "IL DIGEST x\nSESSION\n  none recorded\nTASKS"),  # fresh by time but no SESSION block
])
def test_gate_refuses_without_sending_keys(tmp_path, fresh_code, digest):
    r, tmux_called = _run(tmp_path, fresh_code, digest)
    assert r.returncode == 4, r.stdout + r.stderr
    assert not tmux_called


def test_gate_passes_when_fresh_and_recorded(tmp_path):
    # Past the gate the script reaches the idle loop and calls tmux; it must not exit 4.
    r, tmux_called = _run(tmp_path, 0, "IL DIGEST x\nSESSION\n  task: t\n  next: n", wait=4)
    assert r is None or r.returncode != 4
    assert tmux_called


def test_resume_sent_after_clear_when_transcript_check_reports_unknown(tmp_path):
    """2026-10-21 RCA: a freshly cleared transcript made architect-idle.py exit 2 forever, so the resume was never sent."""
    (tmp_path / "scripts").mkdir()
    (tmp_path / "inbox").mkdir()
    bindir = tmp_path / "bin"
    bindir.mkdir()
    s = tmp_path / "scripts"
    (s / "datg.py").write_text(_fake_datg(0, "IL DIGEST x\nSESSION\n  task: t\n  next: n"))
    (s / "turn-active.py").write_text("import sys\nsys.exit(1)\n")  # not turn-active
    # transcript idle before the clear (no Enter sent yet), unknown (exit 2) afterwards
    (s / "architect-idle.py").write_text(
        "import os, sys\nlog = %r\nsent = os.path.exists(log) and 'Enter' in open(log).read()\nsys.exit(2 if sent else 0)\n"
        % str(tmp_path / "tmux.log"))
    # box: typed text between a literal send and its Enter, else empty
    (s / "input-state.py").write_text(
        "lines = open(%r).read().splitlines()\nlast = [l for l in lines if l.startswith('send-keys')]\n"
        "if last and ' -l ' in last[-1]:\n    print('p typed |', last[-1].split(' -l ', 1)[1])\nelse:\n    print('p empty')\n"
        % str(tmp_path / "tmux.log"))
    tmux = bindir / "tmux"
    tmux.write_text(f'#!/bin/sh\necho "$@" >> {tmp_path}/tmux.log\n')
    tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}",
               SELF_CLEAR_POLL="0.05", SELF_CLEAR_SETTLE="0.05", SELF_CLEAR_ECHO="0.05")
    r = subprocess.run(["bash", _install(tmp_path), "--live"], env=env, capture_output=True, text=True, timeout=60)
    log = (tmp_path / "inbox" / "self-clear.log").read_text()
    assert r.returncode == 0, r.stdout + r.stderr + log
    assert "sent /clear" in log and "sent resume prompt" in log
    assert "not idle after clear" not in log


def test_default_is_dry_run_and_sends_no_keys(tmp_path):
    (tmp_path / "scripts").mkdir()
    (tmp_path / "scripts" / "datg.py").write_text(_fake_datg(0, "IL DIGEST x\nSESSION\n  task: t\n  next: n"))
    bindir = tmp_path / "bin"
    bindir.mkdir()
    tmux = bindir / "tmux"
    tmux.write_text(f'#!/bin/sh\necho "$@" >> {tmp_path}/tmux.log\n')
    tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    r = subprocess.run(["bash", _install(tmp_path)], env=env, capture_output=True, text=True, timeout=20)
    assert r.returncode == 0 and "dry run" in r.stdout, r.stdout + r.stderr
    assert not (tmp_path / "tmux.log").exists()


def test_sourced_from_another_cwd_resolves_root_to_the_architect_dir(tmp_path):
    """#10599: under `source`, $0 is the caller's shell; the root must still be the script's own Architect dir."""
    arch = tmp_path / "arch"
    (arch / "scripts").mkdir(parents=True)
    (arch / "inbox").mkdir()
    (arch / "scripts" / "datg.py").write_text(_fake_datg(1, "IL DIGEST x\nSESSION\n  task: t\n  next: n"))
    script = arch / "scripts" / "self-clear.sh"
    shutil.copy(SCRIPT, script)
    cwd = tmp_path / "elsewhere" / "sub"
    cwd.mkdir(parents=True)
    r = subprocess.run(["bash", "-c", '. "$1"', "bash", str(script)], cwd=cwd,
                       capture_output=True, text=True, timeout=20)
    assert r.returncode == 4, r.stdout + r.stderr
    assert "IL stale" in (arch / "inbox" / "self-clear.log").read_text()


def _run_idle(tmp_path, turn_active):
    """#10599: the --live idle loop with every other check idle and `turn_active`
    as turn-active.py's body (None: the helper is missing). Returns the tmux log."""
    s = tmp_path / "scripts"
    s.mkdir()
    (tmp_path / "inbox").mkdir()
    (s / "datg.py").write_text(_fake_datg(0, "IL DIGEST x\nSESSION\n  task: t\n  next: n"))
    if turn_active is not None:
        (s / "turn-active.py").write_text(turn_active)
    (s / "architect-idle.py").write_text("import sys\nsys.exit(0)\n")
    (s / "input-state.py").write_text("print('p empty')\n")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    tmux = bindir / "tmux"
    tmux.write_text(f'#!/bin/sh\necho "$@" >> {tmp_path}/tmux.log\n')
    tmux.chmod(tmux.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}",
               SELF_CLEAR_POLL="0.05", SELF_CLEAR_SETTLE="0.05", SELF_CLEAR_ECHO="0.05")
    try:
        subprocess.run(["bash", _install(tmp_path), "--live"], env=env, capture_output=True, text=True, timeout=6)
    except subprocess.TimeoutExpired:
        pass  # still waiting for idle, which is the busy answer
    log = tmp_path / "tmux.log"
    return log.read_text() if log.exists() else ""


@pytest.mark.parametrize("turn_active", [
    None,                              # helper missing: python3 exits 2
    "import sys\nsys.exit(2)\n",       # helper error
])
def test_a_missing_or_failing_turn_active_helper_is_busy(tmp_path, turn_active):
    """#10599: only turn-active.py's "not active" exit (1) counts as idle; anything else sends no /clear."""
    assert "/clear" not in _run_idle(tmp_path, turn_active)


def test_turn_active_exit_1_is_idle(tmp_path):
    """#10599 control: the helper's "not active" exit still lets the clear through."""
    assert "/clear" in _run_idle(tmp_path, "import sys\nsys.exit(1)\n")

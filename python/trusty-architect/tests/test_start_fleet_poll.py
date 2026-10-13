"""Tests for scripts/start-fleet-poll.sh against a fake `tmux` first on PATH,
so no tmux server is reached. `tm fleet init` reads the script's exit status:
a failed `new-session` must exit 1, never 0 (#8436 P4 fix)."""
import os
import subprocess

SCRIPT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "scripts", "start-fleet-poll.sh")

# $1 is the tmux subcommand; each call is logged, one argv per line.
FAKE_TMUX = """#!/bin/sh
echo "$*" >> "{log}"
case "$1" in
  has-session) exit 1 ;;
  new-session) {new_session} ;;
esac
exit 0
"""


def _run(tmp_path, new_session):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    log = tmp_path / "tmux.log"
    fake = bin_dir / "tmux"
    fake.write_text(FAKE_TMUX.format(log=log, new_session=new_session))
    fake.chmod(0o755)
    env = {k: v for k, v in os.environ.items()
           if k not in ("TMUX", "TMUX_SOCKET", "TMUX_PANE")}
    env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
    env["ARCHITECT_POLL_SESSION"] = "arch-test-poll"
    out = subprocess.run(["bash", SCRIPT], env=env, capture_output=True,
                         text=True, timeout=30)
    return out, log.read_text().splitlines()


def test_a_failing_new_session_exits_1(tmp_path):
    out, calls = _run(tmp_path, "echo 'no server' >&2; exit 1")
    assert out.returncode == 1, out
    assert "failed to start: arch-test-poll" in out.stderr
    assert "started" not in out.stdout
    assert [c.split()[0] for c in calls] == ["has-session", "new-session"]


def test_a_successful_new_session_exits_0(tmp_path):
    out, calls = _run(tmp_path, "exit 0")
    assert out.returncode == 0, out
    assert "started: arch-test-poll" in out.stdout
    assert calls[-1].startswith("new-session -d -s arch-test-poll")

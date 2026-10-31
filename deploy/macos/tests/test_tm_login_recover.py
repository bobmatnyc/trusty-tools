"""Tests for deploy/macos/tm-login-recover.sh, the login recovery script (#10597 S2).

Why: the script runs at login against the operator's real fleet, so every
decision it makes is proven here against stubs first.
What: each case puts stub `tm` and `tmux` executables in a tmp dir and points
the script's PATH at that dir alone. The stubs record their argv to a log and
answer from a small state dir: which tmux sessions are live, whether the
Architect is bound, and after how many `tm health` calls the daemon is up.
HOME is a separate tmp dir. No real tm, tmux, daemon or launchctl is reached.
Run: python3 -m pytest deploy/macos/tests/test_tm_login_recover.py -q -p no:cacheprovider
"""

import os
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "tm-login-recover.sh"

# Bash stubs. PATH holds only the stub dir, so they use builtins alone.
TM_STUB = r"""#!/bin/bash
printf 'tm %s\n' "$*" >> "$STUB_LOG"
S="$STUB_STATE"
case "$1 $2" in
  "health ")
    n=0; [[ -f "$S/health_calls" ]] && n=$(<"$S/health_calls")
    n=$((n + 1)); echo "$n" > "$S/health_calls"
    up=$(<"$S/health_up_after")
    if (( up > 0 && n >= up )); then echo "daemon: ok"; exit 0; fi
    echo "daemon: unreachable" >&2; exit 1 ;;
  "fleet status")
    if [[ -f "$S/live/tm-architect" && -f "$S/bound" ]]; then echo complete; exit 0; fi
    if [[ -f "$S/live/tm-architect" ]]; then
      echo "incomplete: the Architect is not bound; run \`tm fleet relaunch --dir /x\` to rebind it"
      exit 1
    fi
    echo "incomplete: run \`tm fleet init --dir /x\` to finish the setup"; exit 1 ;;
  "fleet init")
    : > "$S/live/tm-architect"; : > "$S/bound"; echo "Architect set up."; exit 0 ;;
  "fleet relaunch")
    : > "$S/bound"; echo "relaunched"; exit 0 ;;
  "sessions resume")
    : > "$S/live/$3"; echo "resumed $3"; exit 0 ;;
  "sessions ls")
    # The legacy rows a real `ls --no-prune` would show as dead.
    echo "tm-brave-summit   dead"; echo "tm-eager-island   dead"; exit 0 ;;
esac
exit 3
"""

TMUX_STUB = r"""#!/bin/bash
printf 'tmux %s\n' "$*" >> "$STUB_LOG"
if [[ "$1" == has-session && "$2" == -t ]]; then
  name="${3#=}"
  [[ -f "$STUB_STATE/live/$name" ]] && exit 0
  exit 1
fi
exit 3
"""


class Fleet:
    """A stub fleet: tool dir, state dir, argv log and an isolated HOME."""

    def __init__(self, tmp: Path, health_up_after: int = 1) -> None:
        self.bin = tmp / "bin"
        self.state = tmp / "state"
        self.home = tmp / "home"
        self.log = tmp / "argv.log"
        for d in (self.bin, self.state / "live", self.home):
            d.mkdir(parents=True)
        (self.state / "health_up_after").write_text(str(health_up_after))
        for name, body in (("tm", TM_STUB), ("tmux", TMUX_STUB)):
            path = self.bin / name
            path.write_text(body)
            path.chmod(0o755)
        self.log.write_text("")

    def live(self, *names: str) -> None:
        for name in names:
            (self.state / "live" / name).write_text("")

    def bind(self) -> None:
        (self.state / "bound").write_text("")

    def run(self, **extra: str) -> subprocess.CompletedProcess:
        env = {
            "HOME": str(self.home),
            "STUB_LOG": str(self.log),
            "STUB_STATE": str(self.state),
            "TM_RECOVER_PATH": str(self.bin),
            "TM_RECOVER_HEALTH_TIMEOUT": "5",
            "TM_RECOVER_HEALTH_INTERVAL": "0",
            **extra,
        }
        return subprocess.run(
            ["/bin/bash", str(SCRIPT)],
            env=env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def calls(self) -> list[str]:
        return self.log.read_text().splitlines()

    def tm_calls(self) -> list[str]:
        """The `tm` calls that change state (not health, status or ls)."""
        reads = ("tm health", "tm fleet status", "tm sessions ls")
        return [c for c in self.calls() if c.startswith("tm ") and not c.startswith(reads)]


ALL_CALLS: list[str] = []


@pytest.fixture
def fleet(tmp_path: Path):
    f = Fleet(tmp_path)
    yield f
    ALL_CALLS.extend(f.calls())


def test_cold_start_inits_the_architect_and_resumes_the_allowlist(tmp_path: Path) -> None:
    f = Fleet(tmp_path, health_up_after=3)
    r = f.run()
    ALL_CALLS.extend(f.calls())
    assert r.returncode == 0, r.stdout + r.stderr
    assert f.calls().count("tm health") == 3
    assert f.tm_calls() == ["tm fleet init", "tm sessions resume tm-rentalist-pro"]


def test_second_run_after_cold_start_changes_nothing(fleet: Fleet) -> None:
    assert fleet.run().returncode == 0
    first = len(fleet.calls())
    r = fleet.run()
    assert r.returncode == 0, r.stdout + r.stderr
    second = fleet.calls()[first:]
    assert not [c for c in second if c.startswith(("tm fleet init", "tm fleet relaunch",
                                                    "tm sessions resume"))], second


def test_a_live_unbound_architect_is_relaunched_only(fleet: Fleet) -> None:
    fleet.live("tm-architect", "tm-rentalist-pro")
    r = fleet.run()
    assert r.returncode == 0, r.stdout + r.stderr
    assert fleet.tm_calls() == ["tm fleet relaunch"]


def test_a_daemon_that_never_comes_up_exits_nonzero_and_touches_nothing(tmp_path: Path) -> None:
    f = Fleet(tmp_path, health_up_after=0)
    r = f.run(TM_RECOVER_HEALTH_TIMEOUT="1", TM_RECOVER_HEALTH_INTERVAL="0.1")
    ALL_CALLS.extend(f.calls())
    assert r.returncode != 0
    assert f.calls(), "health was never polled"
    assert set(f.calls()) == {"tm health"}, f.calls()


def test_a_dead_legacy_session_is_never_resumed(fleet: Fleet) -> None:
    fleet.live("tm-architect")
    fleet.bind()
    r = fleet.run()
    assert r.returncode == 0, r.stdout + r.stderr
    resumed = [c for c in fleet.calls() if c.startswith("tm sessions resume")]
    assert resumed == ["tm sessions resume tm-rentalist-pro"]


def test_a_wildcard_in_the_allowlist_is_refused(fleet: Fleet) -> None:
    fleet.live("tm-architect")
    fleet.bind()
    r = fleet.run(TM_RECOVER_ALLOWLIST="tm-*")
    assert r.returncode != 0
    assert not [c for c in fleet.calls() if c.startswith("tm sessions resume")]


def test_the_allowlist_file_names_the_sessions_to_resume(fleet: Fleet, tmp_path: Path) -> None:
    fleet.live("tm-architect", "tm-a")
    fleet.bind()
    listing = tmp_path / "allow.txt"
    listing.write_text("# managed PMs\ntm-a\n\ntm-b\n")
    r = fleet.run(TM_RECOVER_ALLOWLIST_FILE=str(listing))
    assert r.returncode == 0, r.stdout + r.stderr
    assert fleet.tm_calls() == ["tm sessions resume tm-b"]


def test_an_empty_allowlist_resumes_nothing(fleet: Fleet, tmp_path: Path) -> None:
    fleet.live("tm-architect")
    fleet.bind()
    listing = tmp_path / "allow.txt"
    listing.write_text("# nothing listed\n")
    r = fleet.run(TM_RECOVER_ALLOWLIST_FILE=str(listing))
    assert r.returncode == 0, r.stdout + r.stderr
    assert fleet.tm_calls() == []


def test_home_holds_only_the_log(fleet: Fleet) -> None:
    assert fleet.run().returncode == 0
    files = sorted(p.relative_to(fleet.home).as_posix()
                   for p in fleet.home.rglob("*") if p.is_file())
    assert files == [".trusty-mpm/logs/login-recover.log"]
    text = (fleet.home / files[0]).read_text()
    assert "fleet init" in text and "tm-rentalist-pro" in text


def test_zz_no_forbidden_command_ran_in_any_case() -> None:
    # Defined last, so pytest runs it last: checks the argv of every case.
    assert ALL_CALLS, "no case recorded any call"
    # An absolute-path call (e.g. /bin/launchctl) bypasses the stub PATH, so
    # the script's code lines are checked too.
    code = [line for line in SCRIPT.read_text().splitlines()
            if line.strip() and not line.lstrip().startswith("#")]
    for word in ("launchctl", "pkill", "killall", "osascript", "auto-resume", "worktree"):
        assert not [line for line in code if word in line], word
    for call in ALL_CALLS:
        assert not call.startswith("tm stop"), call
        assert "launchctl" not in call, call
        if call.startswith("tm sessions ls") or call.startswith("tm ls"):
            assert "--no-prune" in call, call
        assert not call.startswith(("tm worktree", "tm sessions prune")), call

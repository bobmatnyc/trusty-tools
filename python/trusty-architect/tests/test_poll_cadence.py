"""#11369: one poll cadence for every start path.

Why: architect-start.sh said --interval 600 while the running poller logged 60 s; whoever started it by another
route got another cadence. poll_interval_s in local/architect.json is now the one source.
What: in a scratch Architect tree (copies of the shipped scripts, a stub start-fleet-poll.sh and tmux), the interval
architect-start.sh prints, the one poll-supervise.sh logs and the one fleet-poll.py's loader resolves are equal for
the same config; no start path passes its own --interval; a bad config stops architect-start.sh before any start;
a failed start-fleet-poll.sh makes it exit nonzero without the interval line (#10571).
Test: python3 -m pytest -q tests/test_poll_cadence.py
"""
import os
import re
import shutil
import stat
import subprocess

import pytest

import architect_config

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
START_PATHS = ("architect-start.sh", "start-fleet-poll.sh", "poll-supervise.sh")


def _exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


@pytest.fixture
def tree(tmp_path):
    root = tmp_path / "arch"
    (root / "scripts").mkdir(parents=True)
    (root / "local").mkdir()
    (root / "inbox").mkdir()
    for name in ("architect-start.sh", "poll-supervise.sh", "architect_config.py"):
        shutil.copy2(os.path.join(SCRIPTS, name), root / "scripts" / name)
    # the stub records the arguments the poller session would get
    _exe(root / "scripts" / "start-fleet-poll.sh", '#!/usr/bin/env bash\necho "$*" > "$(dirname "$0")/../started.args"\n')
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _exe(bin_dir / "tmux", "#!/usr/bin/env bash\nexit 1\n")   # no session is running
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ARCHITECT_", "POLL_"))}
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
    env.pop("TMUX_SOCKET", None)
    return root, env


def _config(root, text):
    (root / "local" / "architect.json").write_text(text)


def _start(root, env):
    return subprocess.run(["bash", str(root / "scripts" / "architect-start.sh")], env=env,
                          capture_output=True, text=True, timeout=60)


def _supervise(root, env, *args):
    env = dict(env, ARCHITECT_ROOT=str(root), POLL_CMD="sleep 4322", POLL_MATCH="sleep 4322",
               POLL_SUPERVISE_TICK="1", POLL_SUPERVISE_MAX_TICKS="1")
    subprocess.run(["bash", str(root / "scripts" / "poll-supervise.sh"), *args], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    return (root / "inbox" / "poll.log").read_text()


@pytest.mark.parametrize("seconds", [60, 600])
def test_every_start_path_uses_the_same_interval(tree, seconds):
    root, env = tree
    _config(root, '{"poll_interval_s": %d}' % seconds)
    r = _start(root, env)
    assert r.returncode == 0, r.stderr
    printed = int(re.search(r"poll interval: (\d+)s", r.stdout).group(1))
    assert (root / "started.args").read_text().strip() == ""          # no --interval: fleet-poll.py reads the file
    logged = int(re.search(r"interval=(\d+)s", _supervise(root, env)).group(1))
    resolved = architect_config.load(root=str(root), environ={}).poll_interval_s
    assert printed == logged == resolved == seconds


def test_supervisor_stale_limit_follows_the_interval_and_an_explicit_interval_wins(tree):
    root, env = tree
    _config(root, '{"poll_interval_s": 600}')
    assert "interval=600s stale=1200s" in _supervise(root, env)
    (root / "inbox" / "poll.log").unlink()
    assert "interval=30s stale=600s" in _supervise(root, env, "--interval", "30")


def test_a_bad_config_stops_the_start_and_is_logged_by_the_supervisor(tree):
    root, env = tree
    _config(root, '{"poll_interval_s": 0}')
    r = _start(root, env)
    assert r.returncode == 1 and "poll_interval_s" in r.stderr and not (root / "started.args").exists()
    log = _supervise(root, env)
    assert "config: architect_config:" in log and "poll_interval_s" in log and "stale=600s" in log


def test_no_start_path_passes_its_own_interval():
    for name in START_PATHS:
        with open(os.path.join(SCRIPTS, name), encoding="utf-8") as f:
            code = "\n".join(ln for ln in f.read().splitlines() if not ln.lstrip().startswith("#"))
        assert not re.search(r"--interval[ =]+\d", code), f"{name} hard-codes a poll interval"


def test_config_cli_prints_one_value_or_fails_with_the_key(tmp_path):
    cli = os.path.join(SCRIPTS, "architect_config.py")
    good = tmp_path / "a.json"
    good.write_text('{"poll_interval_s": 600, "owner_name": "Ann"}')
    env = dict(os.environ, ARCHITECT_CONFIG=str(good))
    env.pop("ARCHITECT_OWNER_NAME", None)
    run = lambda *a, e=env: subprocess.run(["python3", cli, *a], env=e, capture_output=True, text=True)  # noqa: E731
    assert run("poll-interval").stdout == "600\n" and run("owner-name").stdout == "Ann\n"
    assert run("dashboard-port").returncode == 2
    good.write_text('{"poll_interval_s": "ten"}')
    r = run("poll-interval")
    assert r.returncode == 2 and "poll_interval_s" in r.stderr and r.stdout == ""


def test_a_failed_poll_start_exits_nonzero_without_the_interval_line(tree):
    root, env = tree
    _config(root, '{"poll_interval_s": 60}')
    _exe(root / "scripts" / "start-fleet-poll.sh", "#!/usr/bin/env bash\necho 'stub: start failed' >&2\nexit 1\n")
    r = _start(root, env)
    assert r.returncode != 0, r.stdout
    assert "poll interval:" not in r.stdout

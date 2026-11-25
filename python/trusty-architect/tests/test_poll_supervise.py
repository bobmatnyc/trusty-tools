"""D22 hang watchdog (poll-supervise.sh) and the one-shot wake reset (poll-reset-wake.py)."""
import importlib.util
import json
import os
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
# #10571 S2: the shipped layout keeps the modules in ../scripts, the tests in tests/.
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
SUP = os.path.join(SCRIPTS, "poll-supervise.sh")
_spec = importlib.util.spec_from_file_location("poll_reset", os.path.join(SCRIPTS, "poll-reset-wake.py"))
reset_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(reset_mod)


def _run(tmp_path, state_age, ticks=3, match="sleep 4321"):
    state = tmp_path / "poll-state.json"
    state.write_text("{}")
    old = time.time() - state_age
    os.utime(state, (old, old))
    log = tmp_path / "poll.log"
    env = dict(os.environ, POLL_STATE_FILE=str(state), POLL_LOG=str(log), POLL_CMD="sleep 4321",
               POLL_MATCH=match, POLL_STALE_S="2", POLL_SUPERVISE_TICK="1",
               POLL_SUPERVISE_MAX_TICKS=str(ticks))
    r = subprocess.run(["bash", SUP], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)  # orphans must not hold a pipe open
    return r, log.read_text() if log.exists() else ""


def test_stale_state_restarts_poller_and_logs(tmp_path):
    r, log = _run(tmp_path, state_age=100)
    assert r.returncode == 0
    assert "state stale" in log and log.count("started poller") >= 2, log


def test_fresh_state_is_left_alone(tmp_path):
    r, log = _run(tmp_path, state_age=0, ticks=1)  # one tick of 1 s, stale limit 2 s
    assert "restarting" not in log and log.count("started poller") == 1, log


def test_pid_that_does_not_match_is_not_signalled(tmp_path):
    r, log = _run(tmp_path, state_age=100, match="fleet-poll.py")
    assert "not signalled" in log and "state stale" not in log, log
    subprocess.run(["pkill", "-f", "^sleep 4321$"], capture_output=True)  # our own test children


def test_reset_backs_up_and_clears_backlog(tmp_path):
    p = tmp_path / "poll-state.json"
    p.write_text(json.dumps({"band": "stop", "wake_pending": 2077, "wake_logged": 4,
                             "wake_alerts": [{"k": i} for i in range(5)], "keep": 1}))
    backup, removed = reset_mod.reset(str(p), now=1_790_000_000)
    assert removed == {"wake_pending": 2077, "wake_logged": 4, "wake_alerts": 5}
    assert os.path.basename(backup) == "poll-state.json.bak-20260921T141320Z"
    assert json.load(open(backup))["wake_pending"] == 2077
    now = json.load(open(p))
    assert now["keep"] == 1 and now["band"] == "stop" and "wake_alerts" not in now
    assert now["digest_cursor"] == 1_790_000_000


def test_reset_refuses_while_poller_session_runs(tmp_path, monkeypatch):
    p = tmp_path / "poll-state.json"
    p.write_text('{"wake_pending": 3}')
    monkeypatch.setattr(reset_mod, "poller_running", lambda: True)
    assert reset_mod.main(["--state", str(p)]) == 2
    assert json.load(open(p)) == {"wake_pending": 3} and not list(tmp_path.glob("*.bak-*"))

"""The self-refresh chain end to end: poller pane list and capture -> turn-active.py -> datg ready -> the decision (#10599 PR-1).

Why: the Architect froze the acceptance: on a clean HOME, `tm fleet init --no-launch` seeds a non-default
Architect, and the real poller output feeds turn-active.py, then `datg.py ready`, then the self-clear
decision. A faked idle answer proves nothing. The Codex repro exited 3 with KeyError
'turn_active' before #10571 Phase 2 gave the parser that field.
What: the repro tests run the shipped turn-active.py and fleet-classify.parse_pane on a
synthetic pane fixture (invented session, paths, timestamps and hashes, with the layout of an idle
Architect pane: an earlier prompt, tool-call blocks, the done line, the empty box and the status bar)
and require exit 10 (idle) and agreement. The seeded chain starts a private tmux server (its own
TMUX_TMPDIR) whose Architect pane prints that pane, lists it with the seeded fleet-poll.py, and runs the
seeded self_refresh_step, as the seeded fleet-poll.py binds it, for real: the bar from its
resolve_context_bar (CONTEXT_BAR_TOKENS unset: 250k), socket, capture, turn-active.py, `datg.py ready`
on the seeded ledger, and the context from a transcript file. Only the launch is replaced: it is
recorded instead of run. No claude starts and no key is sent to any tmux server.
Test: this file; unit cases in tests/test_fleet_poll_selfrefresh.py and tests/test_self_clear.py.
"""
import filecmp
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
FIXTURES = os.path.join(HERE, "fixtures")
CODEX_PANE = os.path.join(FIXTURES, "owner_pane_repro.txt")
CODEX_RESULT = os.path.join(FIXTURES, "owner_turn_active_result_repro.json")
SESSION = "tm-arch-e2e"
BAR = 250_000


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _pane():
    with open(CODEX_PANE) as f:
        return f.read()


def _turn_active(scripts, capture, env=None):
    r = subprocess.run([sys.executable, os.path.join(scripts, "turn-active.py")], input=capture,
                       capture_output=True, text=True, timeout=60, env=env)
    return r.returncode, r.stderr


# --- Codex repro --------------------------------------------------------------

def test_repro_the_recorded_failure_was_exit_3_keyerror():
    rec = json.load(open(CODEX_RESULT))
    assert rec["exit"] == 3 and "KeyError: 'turn_active'" in rec["stderr"]


def test_repro_the_pane_reads_10_idle_never_0_or_3_and_parse_pane_agrees():
    # The fixture is an idle pane by construction: only 10 passes; 0 (busy) or 3 (error) fails.
    code, err = _turn_active(SCRIPTS, _pane())
    assert code == 10, err
    fc = _load(os.path.join(SCRIPTS, "fleet-classify.py"), "fc_codex")
    assert fc.parse_pane(_pane())["turn_active"] is False


# --- seeded chain: tm fleet init --no-launch, non-default dir, private tmux ----

def _tm_bin():
    tm = os.environ.get("TM_BIN", "")
    if not tm or not os.access(tm, os.X_OK):
        pytest.skip("set TM_BIN to a tm built from this branch to run the seeded chain")
    if not shutil.which("tmux"):
        pytest.skip("tmux is not installed")
    return tm


@pytest.fixture
def seeded(tmp_path):
    """A non-default Architect seeded by `tm fleet init --no-launch` under a clean HOME, and a private
    tmux server whose Architect pane prints the repro pane."""
    tm = _tm_bin()
    home = tmp_path / "home"
    home.mkdir()
    tmx = tempfile.mkdtemp(prefix="tx", dir="/tmp")   # short: a socket path has a length limit
    arch = tmp_path / "ops" / "arch-x"
    env = {"HOME": str(home), "PATH": os.environ["PATH"], "TMUX_TMPDIR": tmx, "LANG": "C.UTF-8",
           "TRUSTY_MPM_ALLOW_HOST_STATE": "1"}
    r = subprocess.run([tm, "fleet", "init", "--no-launch", "--dir", str(arch)], env=env,
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 0, r.stdout + r.stderr
    for name in ("fleet_poll_selfrefresh.py", "self-clear.sh", "turn-active.py", "fleet-classify.py"):
        assert filecmp.cmp(arch / "scripts" / name, os.path.join(SCRIPTS, name), shallow=False), \
            f"TM_BIN seeds a different {name}: build tm from this branch"
    env.update(ARCHITECT_SESSION=SESSION, ARCHITECT_INBOX_DIR=str(arch / "inbox"),
               CLAUDE_CONFIG_DIR=str(tmp_path / "claude"), DASHBOARD_ROOT=str(tmp_path / "dash"))
    shutil.copy(CODEX_PANE, tmp_path / "pane.txt")
    subprocess.run(["tmux", "new-session", "-d", "-s", SESSION, "-x", "200", "-y", "70",
                    f"cat {tmp_path / 'pane.txt'}; exec sleep 600"], env=env, check=True, timeout=30)
    try:
        yield arch, env
    finally:
        subprocess.run(["tmux", "kill-server"], env=env, capture_output=True, timeout=30)
        shutil.rmtree(tmx, ignore_errors=True)


def _datg(arch, env, *args):
    return subprocess.run([sys.executable, str(arch / "scripts" / "datg.py"), "--project",
                           str(arch / "local" / "il"), *args], env=env, capture_output=True, text=True,
                          timeout=60)


def _transcript(mod, tokens):
    """The Architect's transcript, as Claude Code writes it, with `tokens` of context in use."""
    d = mod.ctx_dir()
    os.makedirs(d, exist_ok=True)
    usage = {"input_tokens": tokens, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    with open(os.path.join(d, "s.jsonl"), "w") as f:
        f.write(json.dumps({"type": "user", "entrypoint": "cli"}) + "\n")
        f.write(json.dumps({"type": "assistant", "entrypoint": "cli", "message": {"usage": usage}}) + "\n")


def _chain(arch, env, monkeypatch, tokens=300_000):
    """One real poller tick's self-refresh step. Returns (decision, launches, panes, captured pane)."""
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    for k in ("TMUX", "TMUX_SOCKET", "SELF_CTX_DIR", "ARCHITECT_PROJECT_DIR", "CONTEXT_BAR_TOKENS"):
        monkeypatch.delenv(k, raising=False)
    poll = _load(str(arch / "scripts" / "fleet-poll.py"), "fleet_poll_seeded")
    panes = poll.list_panes()
    mod = poll._selfrefresh  # the step as the poller's cycle calls it, bound to its bar and inbox
    assert mod._bar(False) == BAR
    _transcript(mod, tokens)
    launched = []
    monkeypatch.setattr(mod, "_spawn", lambda argv, log, lock_fd: launched.append(argv) or 4321)
    arch_pane = next(p for p in panes if p["session"] == SESSION)
    capture = poll.capture_pane(arch_pane)
    return mod.self_refresh_step({}, panes, 1000.0, False), launched, arch_pane, capture


def test_seeded_chain_poller_turn_active_and_ready_agree_and_a_ready_ledger_launches(seeded, monkeypatch):
    """The frozen acceptance: the poller's own capture of the Architect pane, the shared parser and
    turn-active.py agree (10, idle); the seeded ledger made ready passes `datg.py ready`; the step
    launches self-clear.sh on exactly that tmux socket and pane."""
    arch, env = seeded
    assert _datg(arch, env, "add", "G", "--title", "Ship PR-1", "--rank", "1", "--finish", "it refreshes").returncode == 0
    assert _datg(arch, env, "session", "set", "--task", "ship the trigger", "--next", "run the gates",
                 "--owe", "none", "--cron", "none").returncode == 0
    ready = _datg(arch, env, "ready")
    assert ready.returncode == 0, ready.stdout
    decision, launched, pane, capture = _chain(arch, env, monkeypatch)
    fc = _load(str(arch / "scripts" / "fleet-classify.py"), "fc_seeded")
    code, err = _turn_active(str(arch / "scripts"), capture, env)
    assert code == 10, err
    assert fc.parse_pane(capture)["turn_active"] is False
    assert decision == "self-refresh=started", (arch / "inbox" / "self-refresh.log").read_text()
    sock = os.path.join(os.path.realpath(env["TMUX_TMPDIR"]), f"tmux-{os.getuid()}", "default")
    assert launched == [["bash", str(arch / "scripts" / "self-clear.sh"), "--live", "--locked",
                         "--socket", sock, "--target", pane["pane"]]]


def test_seeded_chain_a_fresh_ledger_is_not_ready_and_nothing_launches(seeded, monkeypatch):
    arch, env = seeded
    assert _datg(arch, env, "ready").returncode != 0
    decision, launched, _, _ = _chain(arch, env, monkeypatch)
    assert decision == "self-refresh=il-not-ready" and launched == []
    log = (arch / "inbox" / "self-refresh.log").read_text()
    assert "ready exit" in log


def test_seeded_chain_below_the_bar_reads_nothing_further(seeded, monkeypatch):
    arch, env = seeded
    decision, launched, _, _ = _chain(arch, env, monkeypatch, tokens=BAR - 1)
    assert decision == "self-refresh=below-bar" and launched == []

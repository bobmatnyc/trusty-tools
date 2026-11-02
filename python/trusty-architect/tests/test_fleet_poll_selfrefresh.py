"""fleet_poll_selfrefresh.self_refresh_step: the poller's one trigger for the Architect's self-refresh (#10599 PR-1).

Why: the Architect refreshes itself with no command typed, and only the poller tick may start it (a
foreground self-run deadlocks). Each gate fails closed, so these tests pin every skip and the one launch.
What: loads the module by path and stubs its I/O hooks (bar, context, capture, turn-active, datg ready,
the self-clear liveness probes) with monkeypatch; inbox paths go to tmp_path. No tmux server is reached.
Test: this file; the end-to-end chain is tests/test_fleet_poll_selfrefresh_e2e.py.
"""
import importlib.util
import os
import shutil
import socket
import stat
import subprocess
import sys
import time
import tempfile
import threading
import types

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.join(os.path.dirname(HERE), "scripts")
MODULE = os.path.join(SCRIPTS, "fleet_poll_selfrefresh.py")
BAR = 250_000
IDLE_CAPTURE = "✻ Cooked for 41s\n\n" + "─" * 40 + "\n❯ \n" + "─" * 40 + "\n  ctx 31%\n"
TYPED_CAPTURE = IDLE_CAPTURE.replace("❯ ", "❯ half a message")
PANES = [{"session": "tm-arch-x", "pane": "%7", "cmd": "claude", "active": "11"}]
# #10599: owner-facing text never carries a command (the critic's rule for #10571's ready reasons).
FORBIDDEN = ("datg.py", "--project", "`")
REPAIR = "the ledger has no task: run `python3 scripts/datg.py --project local/il session set --task x`"


def load(path=MODULE):
    spec = importlib.util.spec_from_file_location("fleet_poll_selfrefresh_t", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Stubs:
    """Every hook answers "go"; a test changes the one it is about."""

    def __init__(self):
        self.bar, self.tokens = BAR, BAR + 1
        self.capture, self.turn_active, self.ready = IDLE_CAPTURE, (10, ""), (0, "ready")
        self.alive, self.other = set(), False
        self.sock = "/tmp/tmux-test/default"
        self.spawned = []


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("ARCHITECT_INBOX_DIR", str(tmp_path / "inbox"))
    monkeypatch.setenv("ARCHITECT_SESSION", "tm-arch-x")
    monkeypatch.delenv("SELF_REFRESH_DEBOUNCE", raising=False)
    (tmp_path / "inbox").mkdir()
    mod, s = load(), Stubs()

    def tokens():
        if isinstance(s.tokens, Exception):
            raise s.tokens
        return s.tokens

    def capture(sock, target):
        assert (sock, target) == (s.sock, "%7")
        if isinstance(s.capture, Exception):
            raise s.capture
        return s.capture

    def spawn(argv, log_path, lock_fd):
        s.spawned.append(argv)
        return 4242 + len(s.spawned)

    monkeypatch.setattr(mod, "_bar", lambda dry_run: s.bar)
    monkeypatch.setattr(mod, "_tokens", tokens)
    monkeypatch.setattr(mod, "_capture", capture)
    monkeypatch.setattr(mod, "socket_path", lambda pane: s.sock)
    monkeypatch.setattr(mod, "_turn_active", lambda cap: s.turn_active)
    monkeypatch.setattr(mod, "_ready", lambda: s.ready)
    monkeypatch.setattr(mod, "_spawn", spawn)
    monkeypatch.setattr(mod, "_is_live_self_clear", lambda pid: pid in s.alive)
    monkeypatch.setattr(mod, "_other_self_clear", lambda: s.other)
    return mod, s, tmp_path / "inbox"


def step(mod, state, now=1000.0, dry_run=False, panes=PANES):
    out = mod.self_refresh_step(state, panes, now, dry_run)
    assert isinstance(out, str) and out.startswith("self-refresh="), out
    return out


def test_fires_exactly_once_when_context_crosses_the_bar_then_debounces(env):
    mod, s, inbox = env
    state = {}
    s.tokens = BAR - 1
    assert step(mod, state, 1000) == "self-refresh=below-bar"
    s.tokens = BAR
    assert step(mod, state, 1060) == "self-refresh=started"
    assert len(s.spawned) == 1
    s.alive.add(4243)
    assert step(mod, state, 1120) == "self-refresh=debounced"
    s.alive.clear()  # the self-clear ended but context is still over the bar
    assert step(mod, state, 1180) == "self-refresh=debounced"
    assert len(s.spawned) == 1
    assert (inbox / "self-clear.pid").read_text() == "4243\n"   # the lock file contract: the pid


def test_after_the_debounce_window_a_still_high_context_retries(env):
    mod, s, _ = env
    state = {}
    assert step(mod, state, 1000) == "self-refresh=started"
    assert step(mod, state, 1000 + mod.DEBOUNCE_DEFAULT) == "self-refresh=started"
    assert len(s.spawned) == 2


def test_dropping_below_the_bar_rearms_the_trigger(env):
    mod, s, _ = env
    state = {}
    assert step(mod, state, 1000) == "self-refresh=started"
    s.tokens = 10_000
    assert step(mod, state, 1060) == "self-refresh=below-bar"
    s.tokens = BAR + 5
    assert step(mod, state, 1120) == "self-refresh=started"


def test_below_the_bar_never_fires(env):
    mod, s, inbox = env
    s.tokens = BAR - 1
    assert step(mod, {}) == "self-refresh=below-bar"
    assert s.spawned == [] and not (inbox / "self-clear.pid").exists()


def test_a_typed_box_skips(env):
    mod, s, _ = env
    s.capture = TYPED_CAPTURE
    assert step(mod, {}) == "self-refresh=typed"
    assert s.spawned == []


def test_a_held_lock_skips(env):
    mod, s, inbox = env
    (inbox / "self-clear.pid").write_text("99\n")
    s.alive.add(99)
    assert step(mod, {}) == "self-refresh=lock-held"
    assert s.spawned == []


def test_a_self_clear_started_by_hand_counts_as_held(env):
    mod, s, _ = env
    s.other = True
    assert step(mod, {}) == "self-refresh=lock-held"
    s.other = None  # the process probe failed: unknown is held
    assert step(mod, {}) == "self-refresh=lock-held"
    assert s.spawned == []


def test_a_pid_file_naming_a_dead_pid_is_reused(env):
    mod, s, inbox = env
    (inbox / "self-clear.pid").write_text("99\n")
    assert step(mod, {}) == "self-refresh=started"
    assert (inbox / "self-clear.pid").read_text() == "4243\n"


@pytest.mark.parametrize("content", ["not a pid", "", "{\"pid\": 99}"])
def test_unreadable_content_is_free_when_unlocked_and_held_when_flocked(env, content):
    """The flock decides: no holder means no module run, whatever the file says."""
    import fcntl
    mod, s, inbox = env
    lock = inbox / "self-clear.pid"
    lock.write_text(content)
    fd = os.open(lock, os.O_RDWR)
    fcntl.flock(fd, fcntl.LOCK_EX)
    try:
        assert step(mod, {}) == "self-refresh=lock-held"
    finally:
        os.close(fd)
    assert step(mod, {}) == "self-refresh=started"


@pytest.mark.parametrize("field,value,want", [
    ("bar", None, "bar-unreadable"),
    ("tokens", None, "ctx-unreadable"),
    ("tokens", OSError("gone"), "ctx-unreadable"),
    ("sock", None, "socket-unresolved"),
    ("capture", RuntimeError("no server"), "capture-failed"),
    ("capture", "no prompt here\n", "box-unknown"),
    ("turn_active", (0, ""), "busy"),
    ("turn_active", (3, "KeyError: 'turn_active'"), "busy"),
    ("turn_active", (None, "timeout"), "busy"),
    ("ready", (1, REPAIR), "il-not-ready"),
    ("ready", (2, "IL missing"), "il-not-ready"),
    ("ready", (None, "timeout"), "il-not-ready"),
])
def test_every_unknown_or_failed_gate_fails_closed(env, field, value, want):
    mod, s, inbox = env
    setattr(s, field, value)
    assert step(mod, {}) == f"self-refresh={want}"
    assert s.spawned == [] and not (inbox / "self-clear.pid").exists()


def test_no_architect_pane_skips(env):
    mod, s, _ = env
    assert step(mod, {}, panes=[dict(PANES[0], session="tm-other")]) == "self-refresh=no-pane"


def test_a_dry_run_launches_nothing_and_writes_nothing(env):
    mod, s, inbox = env
    assert step(mod, {}, dry_run=True) == "self-refresh=would-start"
    assert s.spawned == [] and list(inbox.iterdir()) == []


def test_owner_facing_text_never_carries_a_command(env):
    mod, s, inbox = env
    s.ready = (1, REPAIR)
    out = step(mod, {})
    for text in [out, mod.summary(out), *mod.SUMMARIES.values()]:
        assert not any(f in text for f in FORBIDDEN), text
    # The full reasons go to the log only.
    assert REPAIR in (inbox / "self-refresh.log").read_text()


def test_launches_self_clear_detached_with_no_shell(tmp_path, monkeypatch):
    mod = load()
    seen = {}

    class FakePopen:
        def __init__(self, argv, **kw):
            seen["argv"], seen["kw"] = argv, kw
            self.pid = 777

    monkeypatch.setattr(mod.subprocess, "Popen", FakePopen)
    log = tmp_path / "self-refresh.log"
    assert mod._spawn(mod.launch_argv("/tmp/s/sock", "%7"), str(log), 9) == 777
    assert seen["argv"] == ["bash", os.path.join(SCRIPTS, "self-clear.sh"), "--live", "--locked",
                            "--socket", "/tmp/s/sock", "--target", "%7"]
    kw = seen["kw"]
    assert kw["start_new_session"] is True and not kw.get("shell")
    assert kw["stdin"] is subprocess.DEVNULL and kw["stderr"] is subprocess.STDOUT
    assert kw["stdout"].name == str(log)
    assert kw["cwd"] == os.path.dirname(SCRIPTS)
    assert kw["pass_fds"] == (9,)   # the flocked self-clear.pid goes to the child


def test_a_failed_launch_releases_the_lock(env, monkeypatch):
    mod, s, inbox = env

    def boom(argv, log_path, lock_fd):
        raise OSError("no bash")

    monkeypatch.setattr(mod, "_spawn", boom)
    assert step(mod, {}) == "self-refresh=launch-failed"
    assert not (inbox / "self-clear.pid").exists()


def test_the_bar_comes_from_the_bound_poller_resolve_context_bar():
    """#10599: the bar is the poller's resolve_context_bar (fleet_poll_alerts.py); unbound means no bar."""
    mod = load()
    assert mod._bar(False) is None
    mod.bind(types.SimpleNamespace(resolve_context_bar=lambda dry_run: (123456, 1000000)))
    assert mod._bar(False) == 123456
    mod.bind(types.SimpleNamespace(resolve_context_bar=lambda dry_run: (0, 1000000)))
    assert mod._bar(False) is None


def test_the_real_poller_binds_the_step_to_its_250k_bar_and_inbox(tmp_path, monkeypatch):
    """Loaded through fleet-poll.py, the step reads the real bar (CONTEXT_BAR_TOKENS unset: 250000)."""
    monkeypatch.delenv("CONTEXT_BAR_TOKENS", raising=False)
    monkeypatch.setenv("ARCHITECT_INBOX_DIR", str(tmp_path))
    fp = load(os.path.join(SCRIPTS, "fleet-poll.py"))
    sr = fp._selfrefresh
    assert sr._bar(False) == 250_000
    assert sr.inbox() == str(tmp_path)
    monkeypatch.setenv("CONTEXT_BAR_TOKENS", "300000")
    assert sr._bar(False) == 300_000


def test_context_is_read_for_this_architect_dir_by_default(tmp_path, monkeypatch):
    """A non-default Architect reads its own transcripts, not the default dir's."""
    mod = load()
    monkeypatch.delenv("SELF_CTX_DIR", raising=False)
    monkeypatch.delenv("ARCHITECT_PROJECT_DIR", raising=False)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path))
    root = os.path.dirname(SCRIPTS)
    slug = "".join(c if c.isalnum() and c.isascii() else "-" for c in root)
    assert mod.ctx_dir() == os.path.join(str(tmp_path), "projects", slug)
    monkeypatch.setenv("SELF_CTX_DIR", "/x/y")
    assert mod.ctx_dir() == "/x/y"


# --- #10599 PR-1: one tmux server for the capture and the clear ----------------------------------

def _socket_file(directory=None, name="s"):
    """A live unix socket file standing in for a tmux server (a short /tmp path fits the limit)."""
    path = os.path.join(directory or tempfile.mkdtemp(prefix="sr", dir="/tmp"), name)
    sock = socket.socket(socket.AF_UNIX)
    sock.bind(path)
    sock.close()
    return path


def test_the_capture_and_the_launch_use_one_socket_and_pane(env, tmp_path, monkeypatch):
    """#10599 (a): TMUX_SOCKET names a non-default server; the real capture runs `tmux -S <it>` on the
    listed pane and self-clear.sh is launched with the same socket and pane."""
    mod, s, _ = env
    sock = _socket_file()
    monkeypatch.setenv("TMUX_SOCKET", sock)
    monkeypatch.setenv("TMUX", "/tmp/elsewhere,1,0")   # the poller's own server must not win
    monkeypatch.setattr(mod, "socket_path", load().socket_path)
    bindir = tmp_path / "bin"
    bindir.mkdir()
    fake = bindir / "tmux"
    fake.write_text(f"#!{sys.executable}\nimport sys\nopen({str(tmp_path / 'tmux.log')!r}, 'a')"
                    f".write(' '.join(sys.argv[1:]) + '\\n')\nprint({IDLE_CAPTURE!r})\n")
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setenv("PATH", f"{bindir}:{os.environ['PATH']}")
    s.sock = sock
    monkeypatch.setattr(mod, "_capture", load()._capture)
    assert step(mod, {}) == "self-refresh=started"
    calls = (tmp_path / "tmux.log").read_text().splitlines()
    assert calls == [f"-S {sock} capture-pane -e -p -S -80 -t %7"]
    assert s.spawned == [["bash", os.path.join(SCRIPTS, "self-clear.sh"), "--live", "--locked",
                          "--socket", sock, "--target", "%7"]]


def test_socket_path_resolves_each_server_kind(tmp_path, monkeypatch):
    mod = load()
    base = tempfile.mkdtemp(prefix="tt", dir="/tmp")
    sdir = os.path.join(os.path.realpath(base), f"tmux-{os.getuid()}")
    os.makedirs(sdir)
    ws, default = _socket_file(sdir, "tm-ws-a"), _socket_file(sdir, "default")
    other = _socket_file()
    monkeypatch.setenv("TMUX_TMPDIR", base)
    monkeypatch.delenv("TMUX_SOCKET", raising=False)
    monkeypatch.delenv("TMUX", raising=False)
    assert mod.socket_path({"pane": "%1", "server": "tm-ws-a"}) == ws
    assert mod.socket_path({"pane": "%1"}) == default
    monkeypatch.setenv("TMUX", f"{other},4,0")
    assert mod.socket_path({"pane": "%1"}) == other
    monkeypatch.setenv("TMUX_SOCKET", ws)
    assert mod.socket_path({"pane": "%1"}) == ws


def test_an_unresolvable_socket_launches_nothing(env, monkeypatch):
    """#10599 (b): the named socket is not a live socket file: no capture, no launch."""
    mod, s, inbox = env
    monkeypatch.setattr(mod, "socket_path", load().socket_path)
    monkeypatch.setenv("TMUX_SOCKET", "/nonexistent/tmux-sock")
    monkeypatch.setattr(mod, "_capture", lambda sock, target: pytest.fail("captured"))
    assert step(mod, {}) == "self-refresh=socket-unresolved"
    assert s.spawned == [] and not (inbox / "self-clear.pid").exists()


def test_a_missing_inbox_is_created_so_the_reasons_are_logged(env, monkeypatch, tmp_path):
    """#10599: a fresh `tm fleet init` seed has no inbox/; the readiness reasons still reach the log."""
    mod, s, _ = env
    fresh = tmp_path / "fresh" / "inbox"
    monkeypatch.setenv("ARCHITECT_INBOX_DIR", str(fresh))
    s.ready = (1, REPAIR)
    assert step(mod, {}) == "self-refresh=il-not-ready"
    assert REPAIR in (fresh / "self-refresh.log").read_text()


@pytest.mark.parametrize("lag", [0.2, 0.0])
def test_two_concurrent_steppers_over_a_stale_lock_launch_exactly_once(env, monkeypatch, lag):
    """#10599: two steppers that both read the lock as free start one self-clear, not two. fleet-poll.py's
    poll.lock already serialises the poller's own steps (every non-dry-run main() path, --once included);
    this covers any other caller. With `lag` the second acts after the first has launched (its read is out
    of date) and the re-check under the flock sees the live pid; with none they overlap and the
    non-blocking flock on self-clear.pid turns the second away."""
    mod, s, inbox = env
    real_state = mod._lock_state
    for attempt in range(10):
        (inbox / "self-clear.pid").write_text("99\n")
        s.spawned.clear()
        s.alive.clear()
        gate, calls, lock = threading.Barrier(2), [], threading.Lock()

        def both_read_stale():
            with lock:
                calls.append(1)
                n = len(calls)
            if n <= 2:
                gate.wait(5)  # both steppers have read the lock before either acts
                time.sleep(lag if n == 2 else 0)
                return "free", "pid 99 is not a running self-clear.sh"
            return real_state()

        def slow_spawn(argv, log_path, lock_fd):
            time.sleep(0.05)
            s.spawned.append(argv)
            pid = 5000 + len(s.spawned)
            s.alive.add(pid)  # the launched self-clear.sh is running
            return pid

        monkeypatch.setattr(mod, "_lock_state", both_read_stale)
        monkeypatch.setattr(mod, "_spawn", slow_spawn)
        out = []
        threads = [threading.Thread(target=lambda: out.append(mod.self_refresh_step({}, PANES, 1000.0, False)))
                   for _ in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(10)
        assert sorted(out) == ["self-refresh=lock-held", "self-refresh=started"], (attempt, out)
        assert len(s.spawned) == 1, attempt
        assert (inbox / "self-clear.pid").read_text() == "5001\n"


# --- #10599 lock file contract, read by Phase 2's fleet_poll_wake.self_clear_running -------------

def _wake_module():
    return load(os.path.join(SCRIPTS, "fleet_poll_wake.py"))


def test_the_lock_is_held_for_the_whole_run_and_released_when_it_ends(env, monkeypatch, tmp_path):
    """(a) while self-clear.sh runs, self_clear_running is True; (c) still True after the step has closed
    its own fd and late in the run; (b) after the run exits it is False and a new launch can start."""
    mod, s, inbox = env
    arch = inbox.parent
    fake = tmp_path / "bin" / "scripts" / "self-clear.sh"   # ps must show a self-clear.sh command line
    fake.parent.mkdir(parents=True)
    fake.write_text("sleep 1.5\n")
    monkeypatch.setattr(mod, "SELF_CLEAR", str(fake))
    monkeypatch.setattr(mod, "_spawn", load()._spawn)            # a real detached child
    monkeypatch.setattr(mod, "_is_live_self_clear", load()._is_live_self_clear)
    running = _wake_module().self_clear_running
    assert running(str(arch)) is False or not (inbox / "self-clear.pid").exists()
    t0 = time.time()
    assert step(mod, {}, now=1000.0) == "self-refresh=started"
    pid = int((inbox / "self-clear.pid").read_text())
    assert running(str(arch)) is True                          # (a) right after the step returned
    time.sleep(max(0.0, t0 + 1.0 - time.time()))
    assert running(str(arch)) is True                          # (c) late in the run, parent fd long closed
    assert mod._lock_state()[0] == "held"
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        _reap(pid)
        time.sleep(0.05)
    assert running(str(arch)) is False                         # (b) the run ended, the kernel freed the lock
    assert mod._lock_state()[0] == "free"
    assert step(mod, {"self_refresh": {"armed": True}}, now=5000.0) == "self-refresh=started"


def _reap(pid):
    try:
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        pass


def test_a_held_lock_reads_as_running_to_the_wake_and_as_held_to_the_step(env):
    import fcntl
    mod, s, inbox = env
    lock = inbox / "self-clear.pid"
    lock.write_text("")
    fd = os.open(lock, os.O_RDWR)
    fcntl.flock(fd, fcntl.LOCK_EX)
    try:
        assert _wake_module().self_clear_running(str(inbox.parent)) is True
        assert step(mod, {}) == "self-refresh=lock-held"
    finally:
        os.close(fd)

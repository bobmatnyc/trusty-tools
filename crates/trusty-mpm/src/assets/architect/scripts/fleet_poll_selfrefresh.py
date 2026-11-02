"""fleet_poll_selfrefresh.py — the fleet poller's one trigger for the Architect's own self-refresh (#10599).

Why: the Architect must refresh itself at the context bar with no command typed by anyone. Only the
poller can start it: a self-clear the Architect runs in its own foreground Bash waits on its own
pending tool call and deadlocks. The poller tick is the only trigger; this module adds no timer.
What: `self_refresh_step(state, panes, now, dry_run)` runs once per poller cycle and returns one short
decision, "self-refresh=<code>". It starts `scripts/self-clear.sh --live` detached only when every
gate passes, checked in this order, each failing closed (an unknown answer never launches):
the bar (the poller's `resolve_context_bar`, fleet_poll_alerts.py, 250k tokens by default, set by
CONTEXT_BAR_TOKENS; no poller bound by `bind()` means no bar), this Architect's context (self-ctx.py), the debounce, the self-clear lock, the
Architect pane's tmux socket, its input box (typed text skips), turn-active.py (only exit 10 is
idle) and the Intent Ledger readiness check (only exit 0 is ready). self-clear.sh then repeats the turn-active and
readiness checks before and after its own idle wait, so a failed or partial ledger save never clears.
Server: one tmux socket path, resolved once per tick, serves every step. The pane is captured with
`tmux -S <path>`, and self-clear.sh gets `--socket <path> --target <pane id>` and uses both on every
tmux call. The path is the pane's workspace server (`-L` name), else TMUX_SOCKET, else the server in
$TMUX, else tmux's default socket; a path that is not a live socket file means no launch.
Debounce: a launch disarms the trigger; context back under the bar re-arms it, and while it stays over
the bar the trigger retries only after SELF_REFRESH_DEBOUNCE seconds (default 1800).
The full detail of each decision, including the readiness reasons, goes to <inbox>/self-refresh.log
only; `summary()` is the plain-English text safe to show the owner.

Lock file contract (#10599; fleet_poll_wake.self_clear_running reads it, keep it stable):
- Path: <inbox>/self-clear.pid, where <inbox> is the poller's INBOX (ARCHITECT_INBOX_DIR, else
  <architect>/inbox).
- Content: the running self-clear.sh's pid in ASCII decimal and a newline, nothing else. It is empty
  only for the moment between taking the lock and starting self-clear.sh.
- Lock: the step opens the file (creating it), takes a non-blocking LOCK_EX flock, starts
  `bash self-clear.sh --live --locked ...` with that fd inherited (pass_fds), writes the pid in place,
  and closes its own copy. The flock therefore lasts exactly as long as the self-clear.sh run (and any
  child still holding the fd) and is released by the kernel when the run exits, however it exits.
- Removed: only when the start itself fails. After a run the file stays and names a dead pid.
- Running: the flock is held, or the pid names a live self-clear.sh. A reader that cannot open, lock or
  read the file must treat it as running. Free: the flock is not held and the pid is dead or names
  another command. No separate lock file exists.
- A self-clear.sh run by hand (no --locked) takes no lock; it refuses (exit 7) while one is held.
Config (env, optional): ARCHITECT_SESSION, ARCHITECT_INBOX_DIR, SELF_CTX_DIR, ARCHITECT_PROJECT_DIR
(default: the directory holding this scripts/ directory), CLAUDE_CONFIG_DIR, TMUX_SOCKET,
TMUX_TMPDIR, SELF_REFRESH_DEBOUNCE.
Test: tests/test_fleet_poll_selfrefresh.py, tests/test_fleet_poll_selfrefresh_e2e.py
"""
import fcntl
import importlib.util
import os
import stat
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SELF_CLEAR = os.path.join(HERE, "self-clear.sh")
TURN_ACTIVE = os.path.join(HERE, "turn-active.py")
DATG = os.path.join(HERE, "datg.py")
DEBOUNCE_DEFAULT = 1800      # seconds between launches while context stays over the bar
HELPER_TIMEOUT = 60          # seconds for turn-active.py, the readiness check and tmux
IDLE = 10                    # turn-active.py's only idle answer
GO = ("empty", "suggestion")  # input-box states that hold no typed text

# #10599: owner-facing text. It names no command, path or flag (the #10571 critic's rule).
SUMMARIES = {
    "below-bar": "Context is under the refresh bar.",
    "bar-unreadable": "Refresh skipped: the context bar setting could not be read.",
    "ctx-unreadable": "Refresh skipped: the Architect's context use could not be read.",
    "debounced": "Refresh already started recently; waiting before another try.",
    "lock-held": "Refresh skipped: a refresh is already running.",
    "no-pane": "Refresh skipped: the Architect's window was not found.",
    "socket-unresolved": "Refresh skipped: the Architect's terminal server could not be found.",
    "capture-failed": "Refresh skipped: the Architect's window could not be read.",
    "box-unknown": "Refresh skipped: the Architect's input box could not be read.",
    "dialog": "Refresh skipped: a dialog is open in the Architect's window.",
    "typed": "Refresh skipped: there is unsent text in the Architect's input box.",
    "busy": "Refresh skipped: the Architect is in the middle of a turn.",
    "il-not-ready": "Refresh skipped: the ledger is not ready to hand over yet.",
    "would-start": "A refresh would start now (dry run).",
    "started": "Refresh started.",
    "launch-failed": "Refresh could not start.",
    "error": "Refresh skipped after an internal error.",
}

_mods = {}
P = None  # fleet-poll.py's late-bound view; set by bind() (#10571 Phase 2)


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


def summary(decision):
    """Plain-English owner text for a decision string or code; never a command."""
    code = decision.split("=", 1)[-1]
    return SUMMARIES.get(code, SUMMARIES["error"])


def _load(name, filename):
    """A sibling script by path (hyphenated names are not importable); cached."""
    if name not in _mods:
        spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, filename))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _mods[name] = mod
    return _mods[name]


def inbox():
    """The poller's INBOX when bound, else ARCHITECT_INBOX_DIR, else <architect>/inbox."""
    if P is not None:
        return P.INBOX
    return os.environ.get("ARCHITECT_INBOX_DIR") or os.path.join(ROOT, "inbox")


def architect_session():
    return os.environ.get("ARCHITECT_SESSION") or "tm-architect"


def ctx_dir():
    """SELF_CTX_DIR, else self-ctx.py's transcript dir for ARCHITECT_PROJECT_DIR, which defaults to
    this Architect's own directory so a non-default install reads its own transcripts."""
    if os.environ.get("SELF_CTX_DIR"):
        return os.environ["SELF_CTX_DIR"]
    env = dict(os.environ)
    env["ARCHITECT_PROJECT_DIR"] = env.get("ARCHITECT_PROJECT_DIR") or ROOT
    return _load("self_ctx", "self-ctx.py").default_dir(env)


def launch_argv(sock, target):
    """self-clear.sh's argv: the exact tmux socket and pane the gates were checked on; --locked says this
    step holds inbox/self-clear.pid for the run (#10599)."""
    return ["bash", SELF_CLEAR, "--live", "--locked", "--socket", sock, "--target", target]


def socket_path(pane):
    """The absolute tmux socket path `pane` was listed from, or None when it is not a live socket.

    A workspace server is <TMUX_TMPDIR or /tmp, resolved>/tmux-<uid>/<name>; with no server named, the
    poller's own tmux command used TMUX_SOCKET, else the server in $TMUX, else <dir>/default."""
    base = os.path.join(os.path.realpath(os.environ.get("TMUX_TMPDIR") or "/tmp"), f"tmux-{os.getuid()}")
    if pane.get("server"):
        path = os.path.join(base, pane["server"])
    else:
        path = (os.environ.get("TMUX_SOCKET") or os.environ.get("TMUX", "").split(",")[0]
                or os.path.join(base, "default"))
    path = os.path.abspath(path)
    try:
        return path if stat.S_ISSOCK(os.stat(path).st_mode) else None
    except OSError:
        return None


# --- I/O hooks (each replaced in tests) -----------------------------------------

def _bar(dry_run):
    """The context bar in tokens from the poller's resolve_context_bar, or None when unreadable."""
    try:
        bar = P.resolve_context_bar(dry_run)[0]
    except Exception:  # noqa: BLE001 - a missing or broken helper is an unreadable bar
        return None
    return bar if isinstance(bar, int) and not isinstance(bar, bool) and bar > 0 else None


def _tokens():
    return _load("self_ctx", "self-ctx.py").self_ctx_tokens(ctx_dir())


def _capture(sock, target):
    """`tmux -S <sock> capture-pane -e -p` of the Architect pane `target`."""
    r = subprocess.run(["tmux", "-S", sock, "capture-pane", "-e", "-p", "-S", "-80", "-t", target],
                       capture_output=True, text=True, timeout=HELPER_TIMEOUT)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:200])
    return r.stdout


def _run(argv, stdin=None):
    """(exit code or None on timeout/OSError, combined output) of a helper."""
    try:
        r = subprocess.run(argv, input=stdin, capture_output=True, text=True, timeout=HELPER_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as e:
        return None, f"{type(e).__name__}: {e}"
    return r.returncode, (r.stdout + r.stderr).strip()


def _turn_active(capture):
    return _run([sys.executable, TURN_ACTIVE], capture)


def _ready():
    return _run([sys.executable, DATG, "--project", os.path.join(ROOT, "local", "il"), "ready"])


def _is_live_self_clear(pid):
    """True when `pid` is alive and its command line names self-clear.sh."""
    try:
        os.kill(pid, 0)
    except PermissionError:
        pass
    except (OSError, ValueError, TypeError, OverflowError):
        return False
    code, out = _run(["ps", "-p", str(pid), "-o", "command="])
    return code == 0 and "self-clear.sh" in out


def _other_self_clear():
    """True when any process runs this Architect's self-clear.sh; None when that cannot be told."""
    code, _ = _run(["pgrep", "-f", SELF_CLEAR])
    return {0: True, 1: False}.get(code)


def _spawn(argv, log_path, lock_fd):
    """Start `argv` detached in its own session, stdio to `log_path`, inheriting `lock_fd` (the flocked
    self-clear.pid, so the lock lives as long as the run); returns its pid."""
    with open(log_path, "a") as out:
        return subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT,
                                start_new_session=True, close_fds=True, pass_fds=(lock_fd,), cwd=ROOT).pid


# --- lock ---------------------------------------------------------------------------

def _lock_path():
    return os.path.join(inbox(), "self-clear.pid")


def _pid_in(raw):
    raw = raw.strip()
    return int(raw) if raw.isdigit() else None


def _lock_state():
    """("free" | "held", detail) for <inbox>/self-clear.pid under the lock file contract."""
    path = _lock_path()
    if not os.path.lexists(path):
        return "free", ""
    try:
        fd = os.open(path, os.O_RDONLY)
    except OSError as e:
        return "held", f"lock cannot be opened ({e})"
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        except BlockingIOError:
            return "held", "a running self-clear.sh holds self-clear.pid"
        except OSError as e:
            return "held", f"lock cannot be checked ({e})"
        fcntl.flock(fd, fcntl.LOCK_UN)
        pid = _pid_in(os.read(fd, 64).decode("ascii", "replace"))
    except OSError as e:
        return "held", f"lock cannot be read ({e})"
    finally:
        os.close(fd)
    if pid is not None and _is_live_self_clear(pid):
        return "held", f"self-clear.sh pid {pid} is running"
    return "free", f"no run holds the lock (last pid {pid})"


# --- the step -----------------------------------------------------------------------

def _log(book, code, detail, dry_run, force=False):
    """One line in <inbox>/self-refresh.log when the decision changes (or `force`); none in a dry run."""
    if dry_run or (not force and book.get("last") == code):
        return
    book["last"] = code
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    try:
        os.makedirs(inbox(), exist_ok=True)  # #10599: a fresh seed has no inbox/ yet
        with open(os.path.join(inbox(), "self-refresh.log"), "a") as f:
            f.write(f"{stamp} {code}: {detail}\n" if detail else f"{stamp} {code}\n")
    except OSError:
        pass  # logging never changes the decision


def _decide(book, now, panes, dry_run):
    """(code, detail, force_log) for one tick. Launches self-clear.sh on "started"."""
    bar = _bar(dry_run)
    if bar is None:
        return "bar-unreadable", "resolve_context_bar gave no positive bar", False
    try:
        tokens = _tokens()
    except Exception as e:  # noqa: BLE001 - unreadable context never launches
        return "ctx-unreadable", f"{type(e).__name__}: {e}", False
    if not isinstance(tokens, int):
        return "ctx-unreadable", "no assistant usage in the transcript", False
    if tokens < bar:
        book["armed"] = True
        return "below-bar", "", False
    detail = f"ctx {tokens} tokens, bar {bar}"
    debounce = int(os.environ.get("SELF_REFRESH_DEBOUNCE") or DEBOUNCE_DEFAULT)
    if not book.get("armed", True) and now - book.get("started_at", 0) < debounce:
        return "debounced", detail, False
    lock, why = _lock_state()
    if lock == "held":
        return "lock-held", why, False
    other = _other_self_clear()
    if other is not False:
        return "lock-held", "a self-clear.sh is running" if other else "process check failed", False
    sup = sorted((p for p in panes if p.get("session") == architect_session()),
                 key=lambda p: p.get("active") != "11")
    if not sup:
        return "no-pane", f"no {architect_session()} pane", False
    target = sup[0]["pane"]
    sock = socket_path(sup[0])
    if sock is None:
        return "socket-unresolved", f"no live tmux socket for {target}", False
    try:
        capture = _capture(sock, target)
    except Exception as e:  # noqa: BLE001
        return "capture-failed", f"{type(e).__name__}: {e}", False
    try:
        info = _load("fleet_classify", "fleet-classify.py").parse_pane(capture)
    except Exception as e:  # noqa: BLE001
        return "box-unknown", f"{type(e).__name__}: {e}", False
    if info.get("dialog"):
        return "dialog", "", False
    if info.get("box") == "typed":
        return "typed", "", False
    if not info.get("has_box") or info.get("box") not in GO:
        return "box-unknown", f"box={info.get('box')}", False
    code, out = _turn_active(capture)
    if code != IDLE:
        return "busy", f"turn-active exit {code}: {out}", False
    code, out = _ready()
    if code != 0:
        return "il-not-ready", f"ready exit {code}: {out}", False
    if dry_run:
        return "would-start", detail, False
    return _launch(book, now, detail, sock, target)


def _launch(book, now, detail, sock, target):
    """Take the self-clear.pid flock, start self-clear.sh holding it, write its pid (#10599).

    Callers are serialised by Phase 2's poll.lock, which fleet-poll.py's main() takes on every
    non-dry-run path (--once included); a dry run returns "would-start" before reaching here. The
    non-blocking flock is the second guard: a concurrent step, or a run still holding it, gets "lock-held".
    """
    os.makedirs(inbox(), exist_ok=True)
    path = _lock_path()
    try:
        fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o644)
    except OSError as e:
        return "lock-held", f"lock cannot be opened ({e})", False
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return "lock-held", "a running self-clear.sh holds self-clear.pid", False
        pid = _pid_in(os.read(fd, 64).decode("ascii", "replace"))
        if pid is not None and _is_live_self_clear(pid):  # a run that started since the gates
            return "lock-held", f"self-clear.sh pid {pid} is running", False
        os.ftruncate(fd, 0)
        try:
            pid = _spawn(launch_argv(sock, target), os.path.join(inbox(), "self-refresh.log"), fd)
        except Exception as e:  # noqa: BLE001
            os.unlink(path)
            return "launch-failed", f"{type(e).__name__}: {e}", True
        os.pwrite(fd, f"{pid}\n".encode("ascii"), 0)
    finally:
        os.close(fd)  # the child's inherited copy keeps the flock until the run ends
    book.update(armed=False, started_at=now, pid=pid)
    return "started", f"{detail}; self-clear.sh pid {pid} on {sock} {target}", True


def self_refresh_step(state, panes, now, dry_run):
    """One poller tick's self-refresh decision: "self-refresh=<code>" (codes in SUMMARIES).

    Why: #10599: the poller is the only safe trigger for the Architect's own refresh.
    What: runs the gates in the module doc and starts self-clear.sh --live detached only when all
    pass. State lives in state["self_refresh"]. Never raises: an error is "self-refresh=error".
    Test: test_fires_exactly_once_when_context_crosses_the_bar_then_debounces,
    test_every_unknown_or_failed_gate_fails_closed, test_owner_facing_text_never_carries_a_command
    """
    book = state.setdefault("self_refresh", {})
    try:
        code, detail, force = _decide(book, now, panes, dry_run)
    except Exception as e:  # noqa: BLE001 - one bad tick never launches and never stops the poller
        code, detail, force = "error", f"{type(e).__name__}: {e}", True
    _log(book, code, detail, dry_run, force)
    return f"self-refresh={code}"


def _cli(argv):
    """`fleet_poll_selfrefresh.py lock-state`: exit 0 free, 3 held, 2 error; for a self-clear.sh run by hand."""
    if argv != ["lock-state"]:
        print("usage: fleet_poll_selfrefresh.py lock-state", file=sys.stderr)
        return 2
    try:
        state, why = _lock_state()
    except Exception as e:  # noqa: BLE001 - cannot tell: the caller refuses
        print(f"error: {type(e).__name__}: {e}", file=sys.stderr)
        return 2
    print(f"{state} {why}".strip())
    return 0 if state == "free" else 3


if __name__ == "__main__":
    sys.exit(_cli(sys.argv[1:]))

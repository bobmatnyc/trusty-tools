"""fleet_poll_io.py — fleet-poll.py's tmux, log, event and host reads (#10571 Phase 2 split).

Why: fleet-poll.py sat at the 500-SLOC cap; its I/O half moves here unchanged.
What: TmuxError, log, tmux, socket_dir, workspace_servers, list_panes, capture, capture_pane,
send_to_architect, read_events, swap_used_mb and host_alerts. Every name these functions use from the
poller (settings, sibling functions) is read through `P`, the late-bound view fleet-poll.py passes to
bind(), so rebinding `fleet-poll.tmux` or `fleet-poll.LOG` still steers them, as before the split.
Test: tests/test_fleet_poll.py, tests/test_fleet_poll_workspace_servers.py (through fleet-poll.py)
"""
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time

P = None  # fleet-poll.py's late-bound view; set by bind()


def bind(poller):
    """Point this module at the loaded fleet-poll.py (its `_Poller` view)."""
    global P
    P = poller


class TmuxError(Exception):
    pass


def log(msg, dry_run=False):
    line = f"{P.iso()} {msg}"
    if dry_run:
        print(line)
        return
    with open(P.LOG, "a") as f:
        f.write(line + "\n")


def tmux(*args, server=None):
    # #8928: `server` names a workspace's private server (`tmux -L <server>`).
    where = ["-L", server] if server else (["-S", P.SOCKET] if P.SOCKET else [])
    cmd = ["tmux", *where, *args]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise P.TmuxError(f"{args[0]}: {e}") from e
    if r.returncode != 0:
        raise P.TmuxError(f"{args[0]}: {r.stderr.strip()[:200]}")
    return r.stdout


def socket_dir():
    """tmux's socket directory: $TMUX_TMPDIR (or /tmp), resolved, + tmux-<uid>."""
    base = os.path.realpath(os.environ.get("TMUX_TMPDIR") or "/tmp")
    return os.path.join(base, f"tmux-{os.getuid()}")


def workspace_servers(directory=None):
    """#8928: live workspace servers (`tm-ws-*` sockets that answer), sorted."""
    directory = directory or P.socket_dir()
    try:
        names = os.listdir(directory)
    except FileNotFoundError:
        return []
    live = []
    for name in sorted(n for n in names if n.startswith(P.WORKSPACE_PREFIX)):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(1.0)  # a wedged server must not stall the cycle
        try:
            s.connect(os.path.join(directory, name))
            live.append(name)
        except OSError:
            pass  # not a socket, or a server that exited
        finally:
            s.close()
    return live


def list_panes():
    """Panes of the default server and, unless TMUX_SOCKET pins one server,
    of every live workspace server; each pane names the server it is on."""
    fmt = "#{session_name}\t#{pane_id}\t#{pane_current_command}\t#{window_active}#{pane_active}"
    panes = P._rows(P.tmux("list-panes", "-a", "-F", fmt), None)
    for server in ([] if P.SOCKET else P.workspace_servers()):
        try:
            panes += P._rows(P.tmux("list-panes", "-a", "-F", fmt, server=server), server)
        except P.TmuxError as e:
            P.log(f"{server} list-panes ERROR {e}")
    return panes


def _rows(out, server):
    panes = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) == 4:
            pane = dict(zip(("session", "pane", "cmd", "active"), parts))
            if server:
                pane["server"] = server
            panes.append(pane)
    return panes


def capture(pane, server=None):
    if server:
        return P.tmux("capture-pane", "-e", "-p", "-S", "-80", "-t", pane, server=server)
    return P.tmux("capture-pane", "-e", "-p", "-S", "-80", "-t", pane)


def capture_pane(p):
    """Capture listed pane `p` on the server it was listed from (#8928)."""
    return P.capture(p["pane"], p["server"]) if p.get("server") else P.capture(p["pane"])


def send_to_architect(pane, *keys):
    """The only send path. Re-checks that `pane` belongs to the Architect first."""
    owner = P.tmux("display-message", "-p", "-t", pane, "#{session_name}").strip()
    if owner != P.ARCHITECT:
        raise P.TmuxError(f"refusing to send to {pane} (session {owner!r})")
    P.tmux("send-keys", "-t", pane, *keys)


def read_events(state):
    """New permission_prompt / agent_needs_input lines since the saved offset."""
    size = os.path.getsize(P.EVENTS) if os.path.exists(P.EVENTS) else 0
    off = state.get("events_offset")
    if off is None or size < off:   # first run: skip backlog; smaller file: rotated
        off = size if off is None else 0
    evs = []
    if size > off:
        with open(P.EVENTS) as f:
            f.seek(off)
            chunk = f.read()
            off = f.tell()
        for line in chunk.splitlines():
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            s = ev.get("tmux_session") or ""
            if ev.get("type") in P.EVENT_TYPES and s not in P.SKIP_SESSIONS:
                evs.append(P.alert(ev["type"], P.fp(line), ev.get("message") or ev["type"],
                                   s, ev.get("tmux_pane") or "", now=time.time()))
    state["events_offset"] = off
    return evs


def swap_used_mb():
    if sys.platform == "darwin":
        out = subprocess.run(["sysctl", "-n", "vm.swapusage"], capture_output=True,
                             text=True, timeout=5).stdout
        m = re.search(r"used = ([\d.]+)([KMG])", out)
        if not m:
            return None
        return float(m.group(1)) * {"K": 1 / 1024, "M": 1, "G": 1024}[m.group(2)]
    with open("/proc/meminfo") as f:
        mi = {l.split(":")[0]: int(l.split()[1]) for l in f if ":" in l}
    return (mi.get("SwapTotal", 0) - mi.get("SwapFree", 0)) / 1024


def host_alerts(now):
    out = []
    load1, load5, load15 = os.getloadavg()
    cores, factor = os.cpu_count() or 1, float(os.environ.get("LOAD_FACTOR", P.LOAD_FACTOR_DEFAULT))
    if load5 > factor * cores:
        out.append(P.alert("load", "load", f"load {load1:.1f}/{load5:.1f}/{load15:.1f} "
                           f"(5m > {factor}x{cores} cores)", now=now))
    du = shutil.disk_usage("/")
    pct = 100 * du.used / du.total
    if pct > P.DISK_HIGH:
        out.append(P.alert("disk", "disk", f"root disk {pct:.0f}% used", now=now))
    try:
        swap = P.swap_used_mb()
    except (OSError, subprocess.SubprocessError, ValueError):
        swap = None
    if swap:
        out.append(P.alert("swap", "swap", f"swap in use: {swap:.0f} MB", now=now))
    return out

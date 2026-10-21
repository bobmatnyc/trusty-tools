"""#8928 step 4: the fleet poller finds PM panes on workspace tmux servers.

Each workspace runs its own tmux server (`tmux -L tm-ws-<slug>-<hash>`), so a
listing of the default server alone misses those PMs. The poller lists the
default server and every live `tm-ws-*` server, and captures each pane on the
server it was listed from.
"""
import importlib.util
import os
import pathlib
import socket
import tempfile

import pytest

SCRIPTS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts")


def _load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(SCRIPTS, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fp = _load("fleet_poll_ws", "fleet-poll.py")


def _listen(path):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(str(path))
    s.listen(1)
    return s


@pytest.fixture
def short_dir():
    """A directory short enough for a unix socket path (104 bytes on macOS)."""
    with tempfile.TemporaryDirectory(dir="/tmp") as d:
        yield pathlib.Path(d)


def test_live_workspace_servers_are_found_and_dead_ones_skipped(short_dir):
    tmp_path = short_dir
    live = _listen(tmp_path / "tm-ws-api-0123456789ab")
    dead = _listen(tmp_path / "tm-ws-old-ba9876543210")
    dead.close()  # the socket file stays, nothing answers
    other = _listen(tmp_path / "default")
    (tmp_path / "tm-ws-file-000000000000").write_text("not a socket")
    try:
        assert fp.workspace_servers(str(tmp_path)) == ["tm-ws-api-0123456789ab"]
        assert fp.workspace_servers(str(tmp_path / "absent")) == []
    finally:
        live.close()
        other.close()


def test_the_socket_dir_is_tmux_s_own(monkeypatch, tmp_path):
    monkeypatch.setenv("TMUX_TMPDIR", str(tmp_path))
    assert fp.socket_dir() == os.path.join(os.path.realpath(str(tmp_path)),
                                           f"tmux-{os.getuid()}")


def test_panes_on_workspace_servers_are_listed_and_captured_there(monkeypatch):
    calls = []
    rows = {
        None: "tm-default\t%1\tclaude\t11\n",
        "tm-ws-api-0123456789ab": "tm-api\t%1\tclaude\t11\n",
    }

    def fake_tmux(*args, server=None):
        calls.append((server, args))
        if args[0] == "list-panes":
            return rows[server]
        return f"capture of {args[-1]} on {server}"

    monkeypatch.setattr(fp, "SOCKET", "")
    monkeypatch.setattr(fp, "tmux", fake_tmux)
    monkeypatch.setattr(fp, "workspace_servers", lambda *a: ["tm-ws-api-0123456789ab"])
    panes = fp.list_panes()
    by_session = {p["session"]: p for p in panes}
    assert by_session["tm-default"].get("server") is None
    assert by_session["tm-api"]["server"] == "tm-ws-api-0123456789ab"
    # The same pane id on two servers is two panes, each captured on its own.
    assert fp.capture_pane(by_session["tm-api"]) == "capture of %1 on tm-ws-api-0123456789ab"
    assert fp.capture_pane(by_session["tm-default"]) == "capture of %1 on None"


def test_a_failing_workspace_server_does_not_hide_the_default_one(monkeypatch):
    def fake_tmux(*args, server=None):
        if server:
            raise fp.TmuxError("list-panes: server exited")
        return "tm-default\t%1\tclaude\t11\n"

    logged = []
    monkeypatch.setattr(fp, "SOCKET", "")
    monkeypatch.setattr(fp, "tmux", fake_tmux)
    monkeypatch.setattr(fp, "workspace_servers", lambda *a: ["tm-ws-gone-0123456789ab"])
    monkeypatch.setattr(fp, "log", lambda msg, dry_run=False: logged.append(msg))
    assert [p["session"] for p in fp.list_panes()] == ["tm-default"]
    assert any("tm-ws-gone-0123456789ab" in m for m in logged), logged


def test_an_explicit_socket_lists_only_that_server(monkeypatch):
    seen = []

    def fake_tmux(*args, server=None):
        seen.append(server)
        return ""

    monkeypatch.setattr(fp, "SOCKET", "/tmp/explicit.sock")
    monkeypatch.setattr(fp, "tmux", fake_tmux)
    monkeypatch.setattr(fp, "workspace_servers",
                        lambda *a: (_ for _ in ()).throw(AssertionError("not scanned")))
    assert fp.list_panes() == []
    assert seen == [None]

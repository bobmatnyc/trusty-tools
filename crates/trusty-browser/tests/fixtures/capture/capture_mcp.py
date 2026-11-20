"""Capture MCP tool-call parity fixtures from the Python trusty-browser 0.7.0 MCP server.

Why: phase 5 of the Rust port must answer every MCP line and send every
agent.sock frame exactly as ``trusty-browser-mcp`` 0.7.0 does
(docs/plans/trusty-browser-rust-port.md, section 3 items 14 and 16).
What: runs the real ``trusty-browser-mcp`` in connect mode (with and without
``--upload-root``), with the broker down, and in workflow mode, each against a
scripted fake ``agent.sock``. It records every MCP line sent and answered and
every frame the server sent and the reply it got. It also writes
``validation.json`` (FastMCP argument pre-parse and pydantic validation of
each tool for a table of inputs) and ``../../src/mimetypes.tsv`` (this host's
``mimetypes`` tables). No broker, keychain or vault runs: tickets are plain.
Normalised in the fixtures: a fresh uuid4 ``request_id`` is ``<uuid4>``, the
upload root is ``<ROOT>`` (``<REALROOT>`` for its resolved form).

Run: <venv>/bin/python -I capture_mcp.py --out <fixtures dir>
"""

from __future__ import annotations

import argparse
import json
import mimetypes
import os
import re
import socket
import struct
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

HEADER = struct.Struct("=I")
UUID4 = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
CLOSE = "CLOSE"


class FakeBroker:
    """A scripted agent.sock: each received frame gets the next queued reply."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.queue: list[str] = []
        self.frames: list[dict] = []
        self.srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.srv.bind(str(path))
        self.srv.listen(8)
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self) -> None:
        while True:
            try:
                conn, _ = self.srv.accept()
            except OSError:
                return
            threading.Thread(target=self.serve, args=(conn,), daemon=True).start()

    def serve(self, conn: socket.socket) -> None:
        with conn:
            while True:
                head = conn.recv(4, socket.MSG_WAITALL)
                if len(head) < 4:
                    return
                (size,) = HEADER.unpack(head)
                body = conn.recv(size, socket.MSG_WAITALL)
                reply = self.queue.pop(0)
                self.frames.append({"request": body.decode(), "reply": None if reply == CLOSE else reply})
                if reply == CLOSE:
                    return
                data = reply.encode()
                conn.sendall(HEADER.pack(len(data)) + data)

    def stop(self) -> None:
        self.srv.close()
        self.path.unlink()


def call(i: int, name: str, args: dict | None = None) -> dict:
    params: dict = {"name": name}
    if args is not None:
        params["arguments"] = args
    return {"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": params}


INIT = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "fixture-capture", "version": "0"}}}
INITIALIZED = {"jsonrpc": "2.0", "method": "notifications/initialized"}
OK = '{"ok":true}'


def run_session(argv: list[str], steps: list[tuple], broker: FakeBroker | None, norm) -> list[dict]:
    """Drive one MCP process through ``steps`` of (message, replies[, action])."""
    if broker is not None:
        broker.queue[:] = list(steps[0][1])
        broker.frames.clear()
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, env={**os.environ, "PYTHONHASHSEED": "0"})
    assert proc.stdin and proc.stdout
    out = []
    for n, step in enumerate(steps):
        msg, replies = step[0], step[1]
        if len(step) > 2 and step[2] == "stop" and broker is not None:
            broker.stop()
        if broker is not None and n > 0:
            broker.queue[:] = list(replies)
        line = msg if isinstance(msg, str) else json.dumps(msg)
        proc.stdin.write((line + "\n").encode())
        proc.stdin.flush()
        expects = isinstance(msg, dict) and "id" in msg
        got = proc.stdout.readline().decode().rstrip("\n") if expects else None
        frames = list(broker.frames) if broker is not None else []
        if broker is not None:
            broker.frames.clear()
        assert broker is None or not broker.queue, f"unused replies at {line}"
        row = {"send": norm(line), "recv": None if got is None else norm(got),
               "frames": [{"request": norm(f["request"]), "reply": f["reply"]} for f in frames]}
        if len(step) > 2:
            row["action"] = step[2]
        out.append(row)
    proc.stdin.close()
    proc.wait(timeout=10)
    return out


def make_root(base: Path) -> Path:
    root = base / "up"
    (root / "d").mkdir(parents=True)
    (root / "a.txt").write_bytes(b"hello\n")
    (root / "d" / "b.png").write_bytes(bytes(range(256)))
    (root / "X.JSON").write_bytes(b"{}")
    (root / "noext").write_bytes(b"n")
    (root / "t.tar.gz").write_bytes(b"t")
    (root / "big.bin").write_bytes(b"\x01" * (600 * 1024))
    (root / "h1.bin").write_bytes(b"\x02" * (300 * 1024))
    (root / "h2.bin").write_bytes(b"\x03" * (300 * 1024))
    (root / "link").symlink_to("/etc/hosts")
    (root / "dlink").symlink_to(root / "d")
    os.mkfifo(root / "fifo")
    locked = root / "locked"
    locked.write_bytes(b"x")
    locked.chmod(0)
    return root


def connect_steps(root: str) -> list[tuple]:
    attach_ok = '{"ok":true,"grant":"g-0123456789ab","profile":"p1","email":"a@example.com"}'
    up = lambda i, paths, rid="u1": call(i, "page_upload", {"lease": "L1", "ref": "e9", "paths": paths, "request_id": rid})
    return [
        (INIT, []), (INITIALIZED, []),
        (call(2, "browser_tabs", {}), []),
        (call(3, "browser_profiles", {}), ['{"ok":true,"profiles":[{"profile":"p1","email":"a@example.com"},{"profile":"p2","email":"b@example.com"}]}']),
        (call(4, "browser_connect", {"profile": ""}), []),
        (call(5, "browser_connect", {}), ['{"ok":false,"error":"ProfileChoiceRequired","profiles":["p1","p2"]}']),
        (call(6, "browser_tabs"), []),
        (call(7, "browser_connect", {"profile": "p1"}), [attach_ok]),
        (call(8, "browser_connect", {"profile": "p1"}), []),
        (call(9, "browser_tabs", {}), ['{"ok":true,"tabs":[{"id":3,"title":"Caf\\u00e9 \\"q\\"","access":"full","x":1.5,"y":1e20}]}']),
        (call(10, "browser_select", {"tab": "3"}), ['{"ok":true,"lease":"L1","tab":3}']),
        (call(11, "page_read", {"lease": "L1"}), ['{"ok":true,"elements":[],"truncated":false}']),
        (call(12, "page_read", {"lease": "L1", "scope_ref": "e1", "max_chars": 2000, "viewport_only": "yes", "include_text": 1}), [OK]),
        (call(13, "page_find", {"lease": "L1", "name": "Go"}), [OK]),
        (call(14, "page_click", {"lease": "L1", "ref": "e1", "request_id": "r1"}), [OK]),
        (call(15, "page_click", {"lease": "L1", "ref": "e1", "dialog": "accept"}), [OK]),
        (call(16, "page_click", {"lease": "L1", "ref": "e1", "request_id": ""}), ['{"ok":false,"error":"InvalidRequest","field":"request_id"}']),
        (call(17, "page_hover", {"lease": "L1", "ref": "e2", "request_id": "null"}), [OK]),
        (call(18, "page_drag", {"lease": "L1", "ref": "e2", "target": "e3", "request_id": "r2"}), [OK]),
        (call(19, "page_type", {"lease": "L1", "ref": "e2", "text": "héllo\n", "request_id": "r3"}), [OK]),
        (call(20, "page_scroll", {"lease": "L1"}), [OK]),
        (call(21, "page_scroll", {"lease": "L1", "to": "element", "ref": "e4", "wait_ms": "250", "request_id": "r4"}), [OK]),
        (call(22, "page_press", {"lease": "L1", "key": "Enter"}), [OK]),
        (call(23, "page_press", {"lease": "L1", "key": "a", "ref": "e5", "modifiers": '["Shift","Meta"]', "request_id": "r5"}), [OK]),
        (call(24, "page_select", {"lease": "L1", "ref": "e6", "value": "two", "request_id": "r6"}), [OK]),
        (call(25, "page_navigate", {"lease": "L1", "url": "https://example.com/", "request_id": "r7"}), [OK]),
        (call(26, "page_back", {"lease": "L1"}), [OK]),
        (call(27, "page_forward", {"lease": "L1", "request_id": "r8"}), ['{"ok":false,"error":"NoHistory","tab":3}']),
        (call(28, "page_reload", {"lease": "L1", "request_id": "r9"}), [OK]),
        (call(29, "page_downloads", {"lease": "L1"}), ['{"ok":true,"downloads":[]}']),
        (call(30, "browser_switch", {"lease": "L1"}), [OK]),
        (up(31, ["a.txt", "d/b.png", "./X.JSON", "d//../noext"]), []),
        (up(32, ["a.txt", "d//b.png", "./X.JSON", "noext", "t.tar.gz", root + "/a.txt"]), [OK]),
        (up(33, ["a.txt"], None), [OK]),
        (up(34, ["../a.txt"]), []),
        (up(35, ["/etc/hosts"]), []),
        (up(36, ["link"]), []),
        (up(37, ["dlink/b.png"]), []),
        (up(38, ["nope.txt"]), []),
        (up(39, ["d"]), []),
        (up(40, ["big.bin"]), []),
        (up(41, ["h1.bin", "h2.bin"]), []),
        (up(42, []), []),
        (up(43, ["a.txt"] * 11), []),
        (up(44, [""]), []),
        (up(45, ["a\u0000b"]), []),
        (up(46, ["."]), []),
        (up(47, [root]), []),
        (up(48, ["a.txt/x"]), []),
        (up(49, ["fifo"]), []),
        (up(50, ["locked"]), []),
        (up(51, "a.txt"), []),
        (call(52, "page_click", {"lease": "L1", "ref": "e1", "request_id": "r10"}), ['{"ok":false,"error":"StaleElement","ref":"e1","message":"gone \\u2014 read again"}']),
        (call(53, "browser_select", {"tab": "x"}), []),
        (call(54, "page_click", {"lease": "L1"}), []),
        (call(55, "nope", {}), []),
        (call(56, "browser_release", {"lease": "L1"}), ['{"ok":false,"error":"GrantEnded","reason":"revoked"}']),
        (call(57, "browser_tabs", {}), []),
        (call(58, "browser_connect", {"profile": "p1"}), [attach_ok]),
        (call(59, "browser_tabs", {}), [CLOSE]),
        (call(60, "browser_tabs", {}), []),
        (call(61, "browser_connect", {}), [attach_ok]),
        (call(62, "browser_tabs", {}), ['{"ok":true']),
        (call(63, "browser_connect", {}), [attach_ok]),
        (call(64, "browser_tabs", {}), ['[1]']),
        (call(65, "browser_connect", {}), [attach_ok]),
        (call(66, "browser_release", {"lease": "L1"}), ['{"ok":false,"error":"GrantEnded","reason":7}']),
        (call(67, "browser_tabs", {}), []),
        (call(68, "browser_disconnect", {}), []),
        (call(69, "browser_connect", {}), [attach_ok]),
        (call(70, "browser_disconnect", {}), []),
        ({"jsonrpc": "2.0", "id": 71, "method": "ping"}, []),
        ({"jsonrpc": "2.0", "id": "s", "method": "bogus"}, []),
        ({"jsonrpc": "2.0", "id": 72, "method": "tools/call", "params": {}}, []),
        ({"jsonrpc": "2.0", "id": 73, "method": "resources/list"}, []),
        ({"jsonrpc": "2.0", "id": 74, "method": "prompts/list"}, []),
        ({"jsonrpc": "2.0", "id": 75, "method": "resources/templates/list"}, []),
        ("not json", []),
        (call(76, "browser_profiles", {}), [], "stop"),
        (call(77, "browser_connect", {}), []),
    ]


def workflow_steps() -> list[tuple]:
    return [
        (INIT, ['{"ok":true}']), (INITIALIZED, []),
        (call(2, "browser_claim", {}), ['{"ok":true,"lease":"W1","tab":4}']),
        (call(3, "browser_snapshot", {"lease": "W1", "request_id": "s1"}), [OK]),
        (call(4, "browser_snapshot", {"lease": "W1"}), [OK]),
        (call(5, "browser_click", {"lease": "W1", "marker": "mé"}), [OK]),
        (call(6, "browser_click", {"lease": "W1"}), []),
        (call(7, "browser_claim", {}), ['{"ok":false,"error":"StaleLease"}']),
        (call(8, "browser_release", {"lease": "W1"}), ['{"ok":false}']),
        (call(9, "browser_tabs", {}), []),
        (call(10, "browser_release", {"lease": "W1"}), [CLOSE]),
        (call(11, "browser_release", {"lease": "W1"}), []),
    ]


def noroot_steps() -> list[tuple]:
    return [
        (INIT, []), (INITIALIZED, []),
        (call(2, "page_upload", {"lease": "L1", "ref": "e9", "paths": ["a.txt"]}), []),
        (call(3, "page_upload", {"lease": "L1", "ref": "e9", "paths": []}), []),
    ]


def validation_cases() -> list[tuple[str, dict]]:
    cases: list[tuple[str, dict]] = []
    for x in [3, "3", " 3 ", "3.0", "3.00", "+3", "-3", "-0", "1_000", "  -3_0 ", "\t5\n", "00012", "3.5", "3.",
              "+-3", "_3", "3__0", "3_", " ", "", "x", "0x10", "3e2", "٣", 3.0, 3.5, 1e18, -1e18, 9.3e18,
              1e20, True, False, None, [1], {"a": 1}, "[1]", '{"a":1}', "null", "true", 9223372036854775807,
              9999999999999999999]:
        cases.append(("browser_select", {"tab": x}))
    for x in [True, False, 1, 0, 2, -1, 1.0, 0.0, -0.0, 2.0, 0.5, "yes", "YES", "On", "off", "f", "T", "n",
              "No", "1", "0", "2", "", " true", None, [], {}, "null", "[]"]:
        cases.append(("page_read", {"lease": "l", "viewport_only": x}))
    for x in [None, "null", "5", 5, 5.5, True, '["a"]', '{"a":1}', "true", "NaN", "", [], {}]:
        cases.append(("page_hover", {"lease": "l", "ref": "r", "request_id": x}))
    for x in [None, [], ["a"], "a", '["a"]', ["a", 1], [None, "b", 2], {"a": 1}, "null", 5]:
        cases.append(("page_press", {"lease": "l", "key": "k", "modifiers": x}))
    for x in [None, 5, True, 1.5, [], {}, "", "é\n\t\u0000\u007f ​\U0001F600 '\"\\"]:
        cases.append(("browser_switch", {"lease": x}))
    cases += [
        ("page_click", {}),
        ("page_click", {"lease": "x" * 300, "q": [1, 2.5, None, True, "it's", 'a"b', "é", {"k": "v"}]}),
        ("page_click", {"x": "a'b\"c", "lease": "q"}),
        ("page_click", {"x": "a'b", "lease": "q", "y": 1.5e-7, "z": -0.0, "w": 1e16}),
        ("page_click", {"lease": "q", "ref": "r", "dialog": None}),
        ("page_click", {"lease": "q", "ref": "r", "extra": 1}),
        ("browser_select", {"tab": ["a"] * 50}),
        ("browser_select", {"tab": "1" * 30}),
        ("page_upload", {"lease": "l", "ref": "r"}),
        ("page_upload", {"lease": 1, "ref": 2, "paths": 3}),
        ("page_upload", {"lease": "l", "ref": "r", "paths": '["a", "b"]'}),
        ("page_upload", {"lease": "l", "ref": "r", "paths": ["a", 3]}),
        ("page_read", {"lease": "l", "scope_ref": 5, "max_chars": "x", "viewport_only": 3, "include_text": None}),
        ("page_find", {"lease": "l", "role": "button", "limit": "7"}),
        ("page_scroll", {"lease": "l", "to": 5, "wait_ms": 1.5}),
        ("page_navigate", {"lease": "l"}),
        ("page_type", {"lease": "l", "ref": "r", "text": 5}),
        ("page_drag", {"lease": "l", "ref": "r", "target": None}),
        ("page_select", {"lease": "l", "ref": "r", "value": ["v"]}),
        ("page_back", {"lease": "l", "request_id": 1}),
        ("page_forward", {"lease": "l"}),
        ("page_reload", {}),
        ("page_downloads", {"lease": None}),
        ("browser_release", {}),
        ("browser_tabs", {"x": 1}),
        ("browser_connect", {"profile": 5}),
        ("browser_connect", {"profile": "null"}),
        ("browser_profiles", {}),
        ("browser_disconnect", {}),
        ("browser_claim", {}),
        ("browser_snapshot", {"lease": "w"}),
        ("browser_snapshot", {}),
        ("browser_click", {"lease": "w", "marker": 5, "request_id": [1]}),
    ]
    return cases


def validation(out: Path) -> None:
    from mcp.server.fastmcp import FastMCP

    from trusty_browser.agent_mcp import add_tab_tools
    from trusty_browser.page_tools import add_page_tools

    connect = FastMCP("x")
    add_page_tools(connect, lambda *a, **k: {})
    workflow = FastMCP("y")
    add_tab_tools(workflow, lambda *a, **k: {})
    # Mirror run_codes' three session tools with their exact signatures.
    from trusty_browser import agent_mcp
    import inspect
    src = inspect.getsource(agent_mcp.run_codes)
    assert "def browser_connect(profile: str | None = None)" in src

    @connect.tool()
    def browser_profiles() -> dict:
        return {}

    @connect.tool()
    def browser_connect(profile: str | None = None) -> dict:
        return {}

    @connect.tool()
    def browser_disconnect() -> dict:
        return {}

    rows = []
    for tool, args in validation_cases():
        mgr = (workflow if tool in ("browser_claim", "browser_snapshot", "browser_click") else connect)._tool_manager
        md = mgr.get_tool(tool).fn_metadata
        row = {"tool": tool, "arguments": args}
        try:
            row["ok"] = md.arg_model.model_validate(md.pre_parse_json(args)).model_dump_one_level()
        except Exception as error:
            row["error"] = str(error)
        rows.append(row)
    (out / "mcp" / "validation.json").write_text(json.dumps(rows, indent=1, ensure_ascii=False) + "\n")


def mime_table(path: Path) -> None:
    mimetypes.init()
    db = mimetypes._db
    lines = [f"# Python {sys.version.split()[0]} mimetypes on this host (strict); generated by tests/fixtures/capture/capture_mcp.py"]
    lines += [f"suffix\t{k}\t{v}" for k, v in sorted(db.suffix_map.items())]
    lines += [f"encoding\t{k}\t{v}" for k, v in sorted(db.encodings_map.items())]
    lines += [f"type\t{k}\t{v}" for k, v in sorted(db.types_map[True].items())]
    path.write_text("\n".join(lines) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    out = parser.parse_args().out.resolve()
    exe = str(Path(sys.executable).parent / "trusty-browser-mcp")
    base = Path(tempfile.mkdtemp(dir="/tmp", prefix="tb-mcp5-"))
    state = base / "state"
    state.mkdir(mode=0o700)
    root = make_root(base)
    real = os.path.realpath(root)

    def norm(text: str) -> str:
        text = text.replace(real, "<REALROOT>").replace(str(root), "<ROOT>")
        return UUID4.sub("<uuid4>", text)

    try:
        broker = FakeBroker(state / "agent.sock")
        sessions = {
            "connect": run_session([exe, "--state", str(state), "--upload-root", str(root)],
                                   connect_steps(str(root)), broker, norm),
        }
        broker = FakeBroker(state / "agent.sock")
        sessions["noroot"] = run_session([exe, "--state", str(state)], noroot_steps(), broker, norm)
        broker.stop()
        cred = base / "cred.json"
        cred.write_text(json.dumps({"workflow": "fixture", "ticket": "plain-ticket"}))
        cred.chmod(0o600)
        broker = FakeBroker(state / "agent.sock")
        sessions["workflow"] = run_session([exe, "--state", str(state), "--credential", str(cred)],
                                           workflow_steps(), broker, norm)
        broker.stop()
    finally:
        (root / "locked").chmod(0o600)
        subprocess.run(["rm", "-rf", str(base)], check=True)
    for name, steps in sessions.items():
        (out / "mcp" / f"{name}.exchange.json").write_text(json.dumps(steps, indent=1, ensure_ascii=False) + "\n")
    validation(out)
    mime_table(out.parent.parent / "src" / "mimetypes.tsv")


if __name__ == "__main__":
    main()

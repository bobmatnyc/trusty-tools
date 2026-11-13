"""Expose one workflow to one web-QA agent over a persistent Unix socket."""

from __future__ import annotations

import argparse
import json
import os
import re
import socket
import stat
import struct
from pathlib import Path
from threading import Lock
from typing import Any, BinaryIO

from mcp.server.fastmcp import FastMCP

from trusty_browser import vault

HEADER = struct.Struct("=I")
MAX_FRAME = 1024 * 1024


def read_reply(stream: BinaryIO) -> dict[str, Any]:
    header = stream.read(HEADER.size)
    if len(header) != HEADER.size:
        raise ConnectionError("Trusty Browser closed the connection")
    (size,) = HEADER.unpack(header)
    if not 0 < size <= MAX_FRAME:
        raise ValueError("Invalid Trusty Browser frame size")
    payload = stream.read(size)
    if len(payload) != size:
        raise ConnectionError("Trusty Browser reply was interrupted")
    reply = json.loads(payload)
    if not isinstance(reply, dict) or type(reply.get("ok")) is not bool:
        raise ValueError("Invalid Trusty Browser reply")
    return reply


def run(state: Path, credential: Path) -> None:
    with credential.open(encoding="utf-8") as handle:
        attrs = os.fstat(handle.fileno())
        if (
            not stat.S_ISREG(attrs.st_mode)
            or attrs.st_uid != os.getuid()
            or attrs.st_mode & 0o077
        ):
            raise ValueError("Use an owner-only workflow credential file (0600)")
        grant = json.load(handle)
    if credential.parent.resolve() == (state / "workflows").resolve() or grant[
        "ticket"
    ].startswith(vault.PREFIX):
        grant["ticket"] = vault.unseal(
            state, "ticket:" + grant["workflow"], grant["ticket"]
        )

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wire:
        wire.settimeout(30)
        wire.connect(str(state / "agent.sock"))
        with wire.makefile("rb") as stream:
            lock = Lock()

            def call(op: str, **fields: str) -> dict[str, Any]:
                data = json.dumps({"op": op, **fields}, allow_nan=False).encode()
                if len(data) > MAX_FRAME:
                    raise ValueError("Trusty Browser request is too large")
                with lock:
                    try:
                        wire.sendall(HEADER.pack(len(data)) + data)
                        reply = read_reply(stream)
                    except (OSError, ValueError):
                        wire.close()
                        stream.close()
                        raise
                if not reply["ok"]:
                    raise ValueError(str(reply.get("error", "Request refused")))
                return reply

            call("open", workflow=grant["workflow"], ticket=grant["ticket"])
            server = FastMCP("trusty-browser")

            @server.tool()
            def browser_claim() -> dict[str, Any]:
                """Verify the configured Chrome and site identities, then claim this workflow's tab."""
                return call("claim")

            @server.tool()
            def browser_snapshot(lease: str, request_id: str) -> dict[str, Any]:
                """Read the claimed tab after checking both identities again."""
                return call("snapshot", lease=lease, request_id=request_id)

            @server.tool()
            def browser_click(
                lease: str, request_id: str, marker: str
            ) -> dict[str, Any]:
                """Submit the marker to the enrolled page. Reuse the request ID for a retry."""
                return call("click", lease=lease, request_id=request_id, marker=marker)

            @server.tool()
            def browser_release(lease: str) -> dict[str, Any]:
                """Release this workflow's claim."""
                return call("release", lease=lease)

            server.run(transport="stdio")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state", type=Path, default=Path.home() / ".trusty" / "browser"
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--workflow", help="Workflow name enrolled in the browser popup")
    group.add_argument(
        "--credential", type=Path, help="Explicit private workflow credential file"
    )
    args = parser.parse_args()
    credential = args.credential
    if args.workflow is not None:
        if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", args.workflow):
            parser.error("invalid workflow name")
        credential = args.state / "workflows" / (args.workflow + ".json")
    run(args.state, credential)


if __name__ == "__main__":
    main()

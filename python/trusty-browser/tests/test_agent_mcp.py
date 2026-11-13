"""Exercise the web-QA stdio tools through an actual Unix socket."""

import asyncio
import json
from pathlib import Path
import socket
import struct
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from trusty_browser import agent_mcp


def test_workflow_mcp_over_uds():
    with (
        tempfile.TemporaryDirectory(dir="/tmp") as name,
        socket.socket(socket.AF_UNIX) as listener,
    ):
        root = Path(name)
        credential = root / "grant.json"
        credential.write_text(
            json.dumps({"workflow": "test", "ticket": "fixture-ticket"})
        )
        credential.chmod(0o600)
        listener.bind(str(root / "agent.sock"))
        listener.listen()
        listener.settimeout(8)

        def peer():
            with listener.accept()[0] as conn, conn.makefile("rb") as stream:
                conn.settimeout(10)
                for op in ("open", "claim", "snapshot", "click", "release"):
                    (size,) = struct.unpack("=I", stream.read(4))
                    msg = json.loads(stream.read(size))
                    assert msg["op"] == op
                    if op == "open":
                        assert msg == {
                            "op": "open",
                            "workflow": "test",
                            "ticket": "fixture-ticket",
                        }
                    if op in ("snapshot", "click", "release"):
                        assert msg["lease"] == "one"
                    reply = {"ok": True}
                    if op == "claim":
                        reply["lease"] = "one"
                    data = json.dumps(reply).encode()
                    conn.sendall(struct.pack("=I", len(data)) + data)

        async def client():
            params = StdioServerParameters(
                command=sys.executable,
                args=[
                    str(Path(agent_mcp.__file__)),
                    "--state",
                    name,
                    "--credential",
                    str(credential),
                ],
            )
            async with (
                stdio_client(params) as (read, write),
                ClientSession(read, write) as session,
            ):
                await session.initialize()
                assert len((await session.list_tools()).tools) == 4
                claim = await session.call_tool("browser_claim", {})
                assert claim.structuredContent["lease"] == "one"
                for op, args in [
                    ("snapshot", {"request_id": "read"}),
                    ("click", {"request_id": "apply", "marker": "x"}),
                    ("release", {}),
                ]:
                    reply = await session.call_tool(
                        "browser_" + op, {"lease": "one", **args}
                    )
                    assert not reply.isError

        with ThreadPoolExecutor(1) as pool:
            result = pool.submit(peer)
            asyncio.run(client())
            result.result(timeout=10)

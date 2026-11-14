"""Exercise the web-QA stdio tools through an actual Unix socket."""

# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import asyncio
import contextlib
import json
from pathlib import Path
import socket
import struct
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from trusty_browser import agent_mcp, codes, service

# #11239: code mode runs against the real broker and fake extension of these tests.
from test_activation_codes import FakeBrowser, Rig, clock, short  # noqa: F401

CODE_TOOLS = {
    "browser_connect",
    "browser_disconnect",
    "browser_claim",
    "browser_snapshot",
    "browser_click",
    "browser_release",
}


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


class Agent:
    """A code-mode MCP server subprocess plus a broker the test starts and stops."""

    def __init__(self, state: Path) -> None:
        self.rig = Rig(state)
        self.rig.writers = []
        self.rig.alpha = None
        self.texts: list[str] = []
        self.home = state.parent / "cwd"
        self.home.mkdir()
        self.errlog = state.parent / "stderr.log"

    @contextlib.asynccontextmanager
    async def broker(self):
        rig = self.rig
        if rig.alpha is None:
            rig.alpha = rig.seed("alpha")
        async with service.Broker(rig.state) as broker:
            rig.broker = broker
            rig.a = await FakeBrowser(rig, rig.alpha).pair()
            try:
                yield broker
            finally:
                for writer in rig.writers + [rig.a.writer]:
                    writer.close()

    @contextlib.asynccontextmanager
    async def serve(self):
        params = StdioServerParameters(
            command=sys.executable,
            args=[str(Path(agent_mcp.__file__)), "--state", str(self.rig.state)],
            cwd=str(self.home),
        )
        with self.errlog.open("a") as errlog:
            async with (
                stdio_client(params, errlog=errlog) as (read, write),
                ClientSession(read, write) as session,
            ):
                await session.initialize()
                self.session = session
                yield self

    async def tool(self, name: str, **args):
        result = await self.session.call_tool(name, args)
        self.texts += [item.text for item in result.content]
        self.texts.append(json.dumps(result.structuredContent))
        return result

    async def ok(self, name: str, **args) -> dict:
        result = await self.tool(name, **args)
        assert not result.isError, result.content
        return result.structuredContent

    async def refused(self, name: str, **args) -> dict:
        """Return the JSON error frame of a tool error."""
        result = await self.tool(name, **args)
        assert result.isError
        prefix, _, frame = result.content[0].text.partition(": ")
        assert prefix == "Error executing tool " + name
        return json.loads(frame)

    async def mint(self, scope="profile", tab=None) -> str:
        return (await self.rig.mint(self.rig.alpha, scope, tab))["code"]

    async def settled(self) -> None:
        for _ in range(500):
            if not self.rig.broker.grants:
                return
            await asyncio.sleep(0.01)
        raise AssertionError("grant did not end")


def drive(short, body) -> Agent:
    agent = Agent(short)

    async def main():
        async with agent.serve():
            await body(agent)

    asyncio.run(main())
    return agent


def unavailable(frame: dict) -> bool:
    return frame["error"] == "ServiceUnavailable" and set(frame) == {
        "ok",
        "error",
        "message",
    }


def test_code_mode_starts_and_lists_tools_with_no_broker(short, clock):
    async def body(agent):
        tools = (await agent.session.list_tools()).tools
        assert {tool.name for tool in tools} == CODE_TOOLS
        connect = next(t for t in tools if t.name == "browser_connect")
        assert set(connect.inputSchema["properties"]) == {"code"}
        assert unavailable(
            await agent.refused("browser_connect", code=codes.new_code())
        )
        assert not short.exists()  # No credential read, no socket, no file.
        short.mkdir(mode=0o700)
        with socket.socket(socket.AF_UNIX) as stale:
            stale.bind(str(short / "agent.sock"))
        assert unavailable(
            await agent.refused("browser_connect", code=codes.new_code())
        )
        async with agent.broker():
            assert (await agent.ok("browser_connect", code=await agent.mint()))["ok"]

    drive(short, body)


def test_tools_before_connect_answer_not_connected(short, clock):
    async def body(agent):
        for name, args in [
            ("browser_claim", {}),
            ("browser_snapshot", {"lease": "l", "request_id": "r"}),
            ("browser_click", {"lease": "l", "request_id": "r", "marker": "m"}),
            ("browser_release", {"lease": "l"}),
        ]:
            frame = await agent.refused(name, **args)
            assert frame["error"] == "NotConnected" and frame["ok"] is False
        assert await agent.ok("browser_disconnect") == {
            "ok": True,
            "disconnected": False,
        }

    drive(short, body)


def test_connect_returns_grant_facts_and_tools_work_within_the_grant(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            minted = await agent.rig.mint(agent.rig.alpha, "tab", 7)
            reply = await agent.ok("browser_connect", code=minted["code"])
            assert reply == {
                "ok": True,
                "grant": minted["grant"],
                "scope": "tab",
                "profile": agent.rig.alpha["profile"],
                "tab": 7,
                "idle_timeout_s": int(codes.GRANT_IDLE),
            }
            assert broker.grants[minted["grant"]].state == "active"
            claim = await agent.ok("browser_claim")
            assert claim["tab"] == 7 and {"lease", "generation"} <= set(claim)
            lease = claim["lease"]
            read = await agent.ok("browser_snapshot", lease=lease, request_id="r")
            assert read["result"] == {"field": "page-secret-text"}
            clicked = await agent.ok(
                "browser_click", lease=lease, request_id="c", marker="m"
            )
            assert clicked["ok"] and clicked["tab"] == 7
            assert await agent.ok("browser_release", lease=lease) == {"ok": True}
            agent.rig.a.tab = 8
            assert await agent.refused("browser_claim") == {
                "ok": False,
                "error": "OutOfScope",
            }
            assert not broker.claims

    drive(short, body)


def test_second_connect_is_refused_and_spends_no_code(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            first = await agent.mint()
            await agent.ok("browser_connect", code=first)
            second = await agent.rig.mint(agent.rig.alpha)
            frame = await agent.refused("browser_connect", code=second["code"])
            assert frame["error"] == "AlreadyConnected"
            assert broker.grants[second["grant"]].state == "code"
            assert (await agent.ok("browser_claim"))["lease"]
            await agent.ok("browser_disconnect")
            reply = await agent.ok("browser_connect", code=second["code"])
            assert reply["grant"] == second["grant"]

    drive(short, body)


def test_code_refusals_reach_the_agent_and_leave_no_session(short, clock):
    async def body(agent):
        async with agent.broker():

            async def refused(code):
                frame = await agent.refused("browser_connect", code=code)
                missing = await agent.refused("browser_claim")
                assert missing["error"] == "NotConnected"
                return frame

            refusal = {"ok": False, "error": "CodeRefused"}
            assert await refused(codes.new_code()) == refusal
            used = await agent.mint()
            assert (await agent.rig.redeem(used))[0]["ok"]
            assert await refused(used) == refusal
            late = await agent.mint()
            clock[0] += codes.CODE_TTL
            assert await refused(late) == refusal
            for _ in range(codes.FAIL_LIMIT - 1):
                assert await refused(codes.new_code()) == refusal
            good = await agent.mint()
            assert await refused(good) == {
                "ok": False,
                "error": "RedeemLocked",
                "retry_after_s": 60,
            }
            clock[0] += 60
            assert (await agent.ok("browser_connect", code=good))["ok"]

    drive(short, body)


def test_scope_and_audit_refusals_reach_the_agent_and_leave_no_session(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            rig = agent.rig

            async def refused(code):
                frame = await agent.refused("browser_connect", code=code)
                missing = await agent.refused("browser_claim")
                assert missing["error"] == "NotConnected"
                return frame

            rig.a.tab = None
            closed = await agent.mint("tab", 7)
            assert await refused(closed) == {"ok": False, "error": "TabClosed"}
            rig.a.tab = 7
            held, limited = [await agent.mint("tab", 7) for _ in range(2)]
            assert (await rig.redeem(held))[0]["ok"]
            assert await refused(limited) == {
                "ok": False,
                "error": "GrantLimit",
                "limit": "tab",
            }
            await rig.operator(op="revoke_grants")
            real = broker.audit.write

            def broken(*_a, **_k):
                raise OSError("disk full")

            audited = await agent.mint()
            broker.audit.write = broken
            assert await refused(audited) == {"ok": False, "error": "AuditUnavailable"}
            broker.audit.write = real
            away = await agent.mint()
            await rig.a.leave()
            assert await refused(away) == {"ok": False, "error": "BrowserDisconnected"}
            rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
            assert (await agent.ok("browser_connect", code=away))["ok"]

    drive(short, body)


def test_grant_ended_drops_the_session(short, clock):
    async def body(agent):
        async with agent.broker():
            grant = (await agent.ok("browser_connect", code=await agent.mint()))[
                "grant"
            ]
            await agent.rig.operator(op="revoke_grant", grant=grant)
            assert await agent.refused("browser_claim") == {
                "ok": False,
                "error": "GrantEnded",
                "reason": "revoked",
            }
            assert (await agent.refused("browser_claim"))["error"] == "NotConnected"
            assert (await agent.ok("browser_connect", code=await agent.mint()))["ok"]

    drive(short, body)


def test_disconnect_and_shutdown_end_the_grant(short, clock):
    agent = Agent(short)

    # The server's exit (stdin closed) must end the grant it holds.
    async def scenario():
        async with agent.broker():
            async with agent.serve():
                await agent.ok("browser_connect", code=await agent.mint())
                assert await agent.ok("browser_disconnect") == {
                    "ok": True,
                    "disconnected": True,
                }
                await agent.settled()
                assert agent.rig.outcomes("grant_end") == ["closed"]
                missing = await agent.refused("browser_claim")
                assert missing["error"] == "NotConnected"
                await agent.ok("browser_connect", code=await agent.mint())
                assert agent.rig.broker.grants
            await agent.settled()
            assert agent.rig.outcomes("grant_end") == ["closed", "closed"]

    asyncio.run(scenario())


def test_broker_loss_mid_session_answers_unavailable_then_reconnects(short, clock):
    async def body(agent):
        async with agent.broker():
            await agent.ok("browser_connect", code=await agent.mint())
        assert unavailable(await agent.refused("browser_claim"))
        assert (await agent.refused("browser_claim"))["error"] == "NotConnected"
        async with agent.broker():
            await agent.ok("browser_connect", code=await agent.mint())
            assert (await agent.ok("browser_claim"))["lease"]
        # A dead session never blocks a new connect as AlreadyConnected.
        assert unavailable(
            await agent.refused("browser_connect", code=codes.new_code())
        )
        async with agent.broker():
            assert (await agent.ok("browser_connect", code=await agent.mint()))["ok"]

    drive(short, body)


def test_code_never_appears_in_results_logs_or_files(short, clock):
    used: list[str] = []

    async def body(agent):
        down = codes.new_code()
        used.append(down)
        await agent.refused("browser_connect", code=down)
        async with agent.broker():
            first, second = await agent.mint(), await agent.mint()
            used.extend([first, second])
            await agent.ok("browser_connect", code=first)
            await agent.refused("browser_connect", code=second)
            await agent.ok("browser_claim")
            await agent.ok("browser_disconnect")
            assert (await agent.refused("browser_connect", code=first))[
                "error"
            ] == "CodeRefused"

    agent = drive(short, body)
    needles = [c for code in used for c in (code, code[3:].replace("-", ""))]
    for text in agent.texts:
        assert not any(n.lower() in text.lower() for n in needles), text
    assert agent.errlog.read_text()  # The server did log; the code is not in it.
    for root in (short, agent.home, agent.errlog):
        for path in [root] if root.is_file() else root.rglob("*"):
            if path.is_file():
                data = path.read_bytes().lower()
                assert not any(n.lower().encode() in data for n in needles), path

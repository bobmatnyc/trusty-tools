"""Exercise the web-QA stdio tools through an actual Unix socket."""

# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import asyncio
import contextlib
import json
import os
import socket
import struct
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

# #11239: connect mode runs against the real broker and fake extension of these tests.
from test_activation_codes import FakeBrowser, Rig, clock, short  # noqa: F401

from trusty_browser import agent_mcp, service

CODE_TOOLS = {
    "browser_profiles",
    "browser_connect",
    "browser_disconnect",
    "browser_tabs",
    "browser_select",
    "browser_switch",
    "page_read",
    "page_find",
    "page_click",
    "page_hover",
    "page_type",
    "page_scroll",
    "page_press",
    "page_select",
    "page_drag",
    "page_navigate",
    "page_upload",
    "page_downloads",
    "page_back",
    "page_forward",
    "page_reload",
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
    """A connect-mode MCP server subprocess plus a broker the test starts and stops."""

    def __init__(self, state: Path) -> None:
        self.rig = Rig(state)
        self.rig.writers = []
        self.rig.alpha = None
        self.texts: list[str] = []
        self.home = state.parent / "cwd"
        self.home.mkdir()
        self.errlog = state.parent / "stderr.log"
        self.upload_root: Path | None = None

    @contextlib.asynccontextmanager
    async def broker(self, two=False):
        rig = self.rig
        if rig.alpha is None:
            rig.alpha = rig.seed("alpha")
        if two and getattr(rig, "beta", None) is None:
            rig.beta = rig.seed("beta")
        async with service.Broker(rig.state) as broker:
            rig.broker = broker
            rig.a = await FakeBrowser(rig, rig.alpha).pair()
            browsers = [rig.a]
            if two:
                rig.b = await FakeBrowser(rig, rig.beta).pair()
                browsers.append(rig.b)
            try:
                yield broker
            finally:
                for writer in rig.writers + [b.writer for b in browsers]:
                    writer.close()

    @contextlib.asynccontextmanager
    async def serve(self):
        params = StdioServerParameters(
            command=sys.executable,
            args=[str(Path(agent_mcp.__file__)), "--state", str(self.rig.state)]
            + (
                []
                if self.upload_root is None
                else ["--upload-root", str(self.upload_root)]
            ),
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

    async def connect(self, row=None) -> dict:
        """The user's popup Connect for ``row`` (alpha by default)."""
        made = await self.rig.grant(row or self.rig.alpha)
        assert made["ok"], made
        return made

    async def detached(self, grant: str) -> None:
        for _ in range(500):
            if not self.rig.broker.grants[grant].sessions:
                return
            await asyncio.sleep(0.01)
        raise AssertionError("session did not detach")


def drive(short, body, upload_root: Path | None = None) -> Agent:
    agent = Agent(short)
    agent.upload_root = upload_root

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


def test_connect_mode_starts_and_lists_tools_with_no_broker(short, clock):
    async def body(agent):
        tools = (await agent.session.list_tools()).tools
        assert {tool.name for tool in tools} == CODE_TOOLS
        connect = next(t for t in tools if t.name == "browser_connect")
        assert set(connect.inputSchema["properties"]) == {"profile"}
        assert not connect.inputSchema.get("required")
        assert "code" not in json.dumps(connect.inputSchema)
        assert "browser_profiles" in connect.description
        select = next(t for t in tools if t.name == "browser_select")
        assert select.inputSchema["properties"]["tab"]["type"] == "integer"
        assert unavailable(await agent.refused("browser_connect"))
        assert unavailable(await agent.refused("browser_profiles"))
        assert not short.exists()  # No credential read, no socket, no file.
        short.mkdir(mode=0o700)
        with socket.socket(socket.AF_UNIX) as stale:
            stale.bind(str(short / "agent.sock"))
        assert unavailable(await agent.refused("browser_connect"))
        async with agent.broker():
            await agent.connect()
            assert (await agent.ok("browser_connect"))["ok"]

    drive(short, body)


def test_tools_before_connect_answer_not_connected(short, clock):
    async def body(agent):
        for name, args in [
            ("browser_tabs", {}),
            ("browser_select", {"tab": 7}),
            ("browser_switch", {"lease": "l"}),
            ("page_read", {"lease": "l"}),
            ("page_click", {"lease": "l", "ref": "1.0", "request_id": "r"}),
            ("page_hover", {"lease": "l", "ref": "1.0", "request_id": "r"}),
            ("page_type", {"lease": "l", "ref": "1.0", "text": "", "request_id": "r"}),
            ("page_press", {"lease": "l", "key": "Enter", "request_id": "r"}),
            (
                "page_select",
                {"lease": "l", "ref": "1.0", "value": "", "request_id": "r"},
            ),
            (
                "page_drag",
                {"lease": "l", "ref": "1.0", "target": "1.1", "request_id": "r"},
            ),
            (
                "page_navigate",
                {"lease": "l", "url": "https://a.test", "request_id": "r"},
            ),
            ("browser_release", {"lease": "l"}),
        ]:
            frame = await agent.refused(name, **args)
            assert frame["error"] == "NotConnected" and frame["ok"] is False
        assert await agent.ok("browser_disconnect") == {
            "ok": True,
            "disconnected": False,
        }

    drive(short, body)


def test_connect_needs_no_code_and_attaches_to_the_connected_profile(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            # Nothing connected in the popup: a clear error, and no session.
            frame = await agent.refused("browser_connect")
            assert frame == {"ok": False, "error": "NotConnected"}
            assert (await agent.refused("browser_tabs"))["error"] == "NotConnected"
            made = await agent.connect()
            reply = await agent.ok("browser_connect")
            assert reply == {
                "ok": True,
                "grant": made["grant"],
                "scope": "profile",
                "profile": agent.rig.alpha["profile"],
                "email": agent.rig.alpha["email"],
            }
            assert len(broker.grants[made["grant"]].sessions) == 1
            claim = await agent.ok("browser_select", tab=7)
            assert claim["tab"] == 7 and claim["access"] is True
            lease = claim["lease"]
            read = await agent.ok("page_read", lease=lease)
            assert read["text"] == "page-secret-text"
            clicked = await agent.ok(
                "page_click", lease=lease, ref="1.0", request_id="c"
            )
            assert clicked == {"ok": True, "tab": 7, "document": "d7-1"}
            # #11293: a profile grant may bring its own leased tab to the front.
            assert await agent.ok("browser_switch", lease=lease) == {
                "ok": True,
                "tab": 7,
                "window": 1,
                "focused": True,
            }
            assert await agent.ok("browser_release", lease=lease) == {"ok": True}
            frame = await agent.refused("browser_connect")
            assert frame["error"] == "AlreadyConnected"
            assert (await agent.ok("browser_select", tab=9))["lease"]

    drive(short, body)


def test_connect_with_several_profiles_needs_an_explicit_choice(short, clock):
    async def body(agent):
        async with agent.broker(two=True):
            rig = agent.rig
            assert await agent.ok("browser_profiles") == {"ok": True, "profiles": []}
            alpha = (await agent.connect())["grant"]
            beta = (await agent.connect(rig.beta))["grant"]
            listed = (await agent.ok("browser_profiles"))["profiles"]
            assert {(p["profile"], p["grant"]) for p in listed} == {
                (rig.alpha["profile"], alpha),
                (rig.beta["profile"], beta),
            }
            frame = await agent.refused("browser_connect")
            assert frame["error"] == "ProfileChoiceRequired"
            assert sorted(p["grant"] for p in frame["profiles"]) == sorted([alpha, beta])
            assert (await agent.refused("browser_tabs"))["error"] == "NotConnected"
            reply = await agent.ok("browser_connect", profile=rig.beta["profile"])
            assert reply["grant"] == beta
            assert (await agent.ok("browser_tabs"))["ok"]
            assert rig.b.sent("page_tabs") and rig.a.sent("page_tabs") == []

    drive(short, body)


def test_attach_refusals_reach_the_agent_and_leave_no_session(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            rig = agent.rig

            async def refused(**args):
                frame = await agent.refused("browser_connect", **args)
                missing = await agent.refused("browser_tabs")
                assert missing["error"] == "NotConnected"
                return frame

            await agent.connect()
            real = broker.audit.write

            def broken(*_a, **_k):
                raise OSError("disk full")

            broker.audit.write = broken
            assert await refused() == {"ok": False, "error": "AuditUnavailable"}
            broker.audit.write = real
            assert (await refused(profile="nobody"))["error"] == "NotConnected"
            bad = await agent.refused("browser_connect", profile="")
            assert bad["error"] == "InvalidRequest"
            await rig.a.leave()
            rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
            assert await refused() == {"ok": False, "error": "NotConnected"}
            await agent.connect()
            assert (await agent.ok("browser_connect"))["ok"]

    drive(short, body)


def test_grant_ended_drops_the_session(short, clock):
    async def body(agent):
        async with agent.broker():
            await agent.connect()
            grant = (await agent.ok("browser_connect"))["grant"]
            await agent.rig.operator(op="revoke_grant", grant=grant)
            assert await agent.refused("browser_tabs") == {
                "ok": False,
                "error": "GrantEnded",
                "reason": "revoked",
            }
            after = await agent.refused("browser_tabs")
            assert after["error"] == "NotConnected" and after["reason"] == "revoked"
            await agent.connect()
            assert (await agent.ok("browser_connect"))["ok"]
            await agent.ok("browser_disconnect")
            assert "reason" not in await agent.refused("browser_tabs")

    drive(short, body)


def test_disconnect_and_shutdown_keep_the_profile_connected(short, clock):
    agent = Agent(short)

    # #11239 ruling Dpj: the user's Connect outlives any one agent.
    async def scenario():
        async with agent.broker():
            grant = (await agent.connect())["grant"]
            async with agent.serve():
                await agent.ok("browser_connect")
                assert await agent.ok("browser_disconnect") == {
                    "ok": True,
                    "disconnected": True,
                }
                await agent.detached(grant)
                missing = await agent.refused("browser_tabs")
                assert missing["error"] == "NotConnected"
                await agent.ok("browser_connect")
            await agent.detached(grant)
            assert grant in agent.rig.broker.grants
            assert agent.rig.outcomes("grant_end") == []
            assert agent.rig.outcomes("detach") == ["ok", "ok"]

    asyncio.run(scenario())


def test_broker_loss_mid_session_answers_unavailable_then_reconnects(short, clock):
    async def body(agent):
        async with agent.broker():
            await agent.connect()
            await agent.ok("browser_connect")
        assert unavailable(await agent.refused("browser_tabs"))
        assert (await agent.refused("browser_tabs"))["error"] == "NotConnected"
        async with agent.broker():
            await agent.connect()
            await agent.ok("browser_connect")
            assert (await agent.ok("browser_select", tab=7))["lease"]
        # A dead session never blocks a new connect as AlreadyConnected.
        assert unavailable(await agent.refused("browser_connect"))
        async with agent.broker():
            await agent.connect()
            assert (await agent.ok("browser_connect"))["ok"]

    drive(short, body)


def test_the_browser_key_never_appears_in_results_or_logs(short, clock):
    async def body(agent):
        async with agent.broker():
            await agent.connect()
            await agent.ok("browser_connect")
            await agent.ok("browser_tabs")
            await agent.ok("browser_profiles")
            await agent.ok("browser_disconnect")

    agent = drive(short, body)
    key = agent.rig.alpha["key"]
    assert not any(key in text for text in agent.texts)
    assert key not in agent.errlog.read_text()


def code_file(folder: Path, name: str, content: str, mode: int = 0o600) -> Path:
    """A real file with a real mode; the folder sits outside every scanned root."""
    folder.mkdir(mode=0o700, exist_ok=True)
    path = folder / name
    path.write_text(content)
    os.chmod(path, mode)
    return path


def test_end_to_end_page_session_through_the_mcp_server(short, clock):
    async def body(agent):
        async with agent.broker() as broker:
            rig = agent.rig
            await agent.connect()
            reply = await agent.ok("browser_connect")
            assert reply["scope"] == "profile"
            tabs = (await agent.ok("browser_tabs"))["tabs"]
            assert [t["tab"] for t in tabs] == [7, 8, 9]
            lease = (await agent.ok("browser_select", tab=7))["lease"]
            read = await agent.ok("page_read", lease=lease)
            assert read["elements"][0]["ref"] == "1.0" and read["document"] == "d7-1"
            typed = await agent.ok(
                "page_type", lease=lease, ref="1.0", text="hello", request_id="t"
            )
            assert typed == {"ok": True, "tab": 7, "document": "d7-1"}
            clicked = await agent.ok(
                "page_click", lease=lease, ref="1.0", request_id="c"
            )
            assert clicked["ok"]
            # #11291: an explicit scroll relays its view; the same request ID replays it.
            scrolled = await agent.ok(
                "page_scroll", lease=lease, to="bottom", request_id="s"
            )
            assert scrolled == {
                "ok": True,
                "tab": 7,
                "document": "d7-1",
                "scroll_y": 540,
                "scroll_height": 4000,
                "viewport_height": 600,
                "grew": True,
                "at_end": False,
            }
            again = await agent.ok(
                "page_scroll", lease=lease, to="bottom", request_id="s"
            )
            assert again == scrolled
            # #11286: ref and modifiers are optional; the broker sends null and [].
            pressed = await agent.ok(
                "page_press", lease=lease, key="Enter", request_id="p"
            )
            assert pressed == {"ok": True, "tab": 7, "document": "d7-1"}
            chord = await agent.ok(
                "page_press",
                lease=lease,
                ref="1.0",
                key="Tab",
                modifiers=["Shift"],
                request_id="p2",
            )
            assert chord["ok"]
            assert [
                (f["ref"], f["key"], f["modifiers"]) for f in rig.a.sent("page_press")
            ] == [(None, "Enter", []), ("1.0", "Tab", ["Shift"])]
            # #11285: a select reports a change, then a repeat reports no effect.
            for rid, changed in (("s1", True), ("s2", False)):
                chosen = await agent.ok(
                    "page_select", lease=lease, ref="1.0", value="ca", request_id=rid
                )
                assert chosen == {
                    "ok": True,
                    "tab": 7,
                    "document": "d7-1",
                    "changed": changed,
                    "value": "ca",
                }
            # #11287: hover runs on the lease like a click.
            hovered = await agent.ok(
                "page_hover", lease=lease, ref="1.0", request_id="h"
            )
            assert hovered == {"ok": True, "tab": 7, "document": "d7-1"}
            landed = await agent.ok(
                "page_navigate",
                lease=lease,
                url="https://qa.test/next",
                request_id="n",
            )
            assert landed["access"] is True and landed["settled"] is True
            back = await agent.ok("page_back", lease=lease, request_id="b")
            assert back["ok"] and back["origin"] == landed["origin"]
            assert (await agent.ok("page_forward", lease=lease, request_id="f"))["ok"]
            reloaded = await agent.ok("page_reload", lease=lease, request_id="r")
            assert reloaded["settled"] is True and reloaded["tab"] == 7
            assert rig.a.done == [
                ("page_type", "1.0", "hello"),
                ("page_click", "1.0", None),
                ("page_scroll", "bottom", None, 1000),
                ("page_press", None, None),
                ("page_press", "1.0", None),
                ("page_select", "1.0", "ca"),
                ("page_select", "1.0", "ca"),
                ("page_hover", "1.0", None),
            ]
            assert await agent.ok("browser_release", lease=lease) == {"ok": True}
            assert not broker.claims
            assert await agent.ok("browser_disconnect") == {
                "ok": True,
                "disconnected": True,
            }
            await agent.detached(reply["grant"])
            assert rig.outcomes("grant_end") == []

    drive(short, body)


def test_page_upload_reads_local_files_and_refuses_bad_paths(short, clock):
    folder = short.parent / "uploads"
    sample = code_file(folder, "upload-example.txt", "hello file\n")
    big = code_file(folder, "big.bin", "x" * (agent_mcp.UPLOAD_MAX + 1))
    outside = code_file(short.parent / "secret", "id_rsa", "key")
    (folder / "link").symlink_to(outside)

    async def body(agent):
        async with agent.broker():
            rig = agent.rig
            await agent.connect()
            await agent.ok("browser_connect")
            lease = (await agent.ok("browser_select", tab=7))["lease"]
            for path, reason in [
                (folder / "missing.txt", "does not exist"),
                (folder, "is not a regular file"),
                (big, f"is larger than {agent_mcp.UPLOAD_MAX} bytes"),
                (outside, "is outside the upload root"),
                (folder / "link", "is a symlink"),
                (f"{folder}/../secret/id_rsa", "contains a '..' component"),
            ]:
                frame = await agent.refused(
                    "page_upload",
                    lease=lease,
                    ref="1.0",
                    paths=[str(path)],
                    request_id="u",
                )
                assert frame == {
                    "ok": False,
                    "error": "FileRefused",
                    "message": f"{path}: {reason}",
                }
            empty = await agent.refused(
                "page_upload", lease=lease, ref="1.0", paths=[""], request_id="u"
            )
            assert empty["error"] == "InvalidRequest"
            assert rig.a.sent("page_upload") == []
            done = await agent.ok(
                "page_upload",
                lease=lease,
                ref="1.0",
                paths=[str(sample)],
                request_id="u",
            )
            assert done == {"ok": True, "tab": 7, "document": "d7-1"}
            assert rig.a.sent("page_upload")[0]["files"] == [
                {
                    "name": "upload-example.txt",
                    "type": "text/plain",
                    "data": "aGVsbG8gZmlsZQo=",
                }
            ]
            relative = await agent.ok(
                "page_upload",
                lease=lease,
                ref="1.0",
                paths=["upload-example.txt"],
                request_id="r",
            )
            assert relative["ok"] is True
            listed = await agent.ok("page_downloads", lease=lease)
            assert listed["downloads"][0]["path"].endswith("report.txt")

    drive(short, body, upload_root=folder)


def test_page_upload_is_refused_without_an_upload_root(short, clock):
    # PR #11305 review: no --upload-root means no file ever leaves the disk.
    sample = code_file(short.parent / "uploads", "a.txt", "a")

    async def body(agent):
        async with agent.broker():
            await agent.connect()
            await agent.ok("browser_connect")
            lease = (await agent.ok("browser_select", tab=7))["lease"]
            frame = await agent.refused(
                "page_upload",
                lease=lease,
                ref="1.0",
                paths=[str(sample)],
                request_id="u",
            )
            assert frame == {
                "ok": False,
                "error": "UploadsDisabled",
                "message": agent_mcp.UPLOADS_DISABLED,
            }
            assert agent.rig.a.sent("page_upload") == []

    drive(short, body)

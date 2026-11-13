"""Length-framed JSON and peer authentication for Trusty Browser's Unix sockets."""

from __future__ import annotations
import asyncio
from dataclasses import dataclass, field
import secrets
import sqlite3
import uuid
import hmac
import json
import os
import socket
import struct
import sys

Frame = dict[str, object]
HEADER = struct.Struct("=I")
MAX_FRAME = 1024 * 1024


class Refusal(ValueError):
    def __init__(self, code: str, **details: object) -> None:
        super().__init__(code)
        self.response: Frame = {"ok": False, "error": code, **details}


def string(message: Frame, name: str) -> str:
    value = message.get(name)
    if not isinstance(value, str) or not value or len(value) > 8192:
        raise Refusal("InvalidRequest")
    return value


async def read_frame(reader: asyncio.StreamReader) -> Frame:
    (size,) = HEADER.unpack(await reader.readexactly(HEADER.size))
    if not 0 < size <= MAX_FRAME:
        raise Refusal("InvalidFrame")
    frame = json.loads(await reader.readexactly(size))
    if not isinstance(frame, dict):
        raise Refusal("InvalidFrame")
    return frame


async def write_frame(writer: asyncio.StreamWriter, message: Frame) -> None:
    body = json.dumps(message, separators=(",", ":"), allow_nan=False).encode()
    if len(body) > MAX_FRAME:
        raise Refusal("InvalidFrame")
    writer.write(HEADER.pack(len(body)) + body)
    await asyncio.wait_for(writer.drain(), 5)


def verify_peer(writer: asyncio.StreamWriter) -> None:
    stream = writer.get_extra_info("socket")
    if stream is None or stream.family != socket.AF_UNIX:
        raise Refusal("Unauthorized")
    if sys.platform == "darwin":
        raw = stream.getsockopt(0, socket.LOCAL_PEERCRED, 128)
        version, uid = struct.unpack_from("=II", raw)
        if version:
            raise Refusal("Unauthorized")
    elif sys.platform.startswith("linux"):
        raw = stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
        _, uid, _ = struct.unpack("=iII", raw)
    else:
        raise Refusal("UnsupportedPeerCredentials")
    if uid != os.getuid():
        raise Refusal("Unauthorized")


def pairing_proof(key: str, role: str, hello: Frame, nonce: str) -> str:
    transcript = json.dumps(
        [
            "trusty-browser-native-v1",
            role,
            "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc",
            hello["profile"],
            hello["email"],
            hello["incarnation"],
            hello["nonce"],
            nonce,
        ],
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode()
    return hmac.digest(key.encode("ascii"), transcript, "sha256").hex()


@dataclass(eq=False)
class Peer:
    writer: asyncio.StreamWriter
    row: sqlite3.Row
    incarnation: str
    generation: str = field(default_factory=lambda: str(uuid.uuid4()))
    pending: dict[str, asyncio.Future[Frame]] = field(default_factory=dict)

    async def command(self, op: str, **args: object) -> Frame:
        ident = secrets.token_urlsafe(18)
        reply = asyncio.get_running_loop().create_future()
        self.pending[ident] = reply
        expectations = {k: str(self.row[k]) for k in ("email", "url", "account")}
        try:
            await write_frame(
                self.writer, {"id": ident, "op": op, **expectations, **args}
            )
            observed = await asyncio.wait_for(reply, 8)
        except (ConnectionError, TimeoutError, OSError) as exc:
            self.writer.close()
            raise Refusal(
                "OutcomeUnknown" if op == "click" else "BrowserDisconnected"
            ) from exc
        finally:
            self.pending.pop(ident, None)
        if observed.get("ok") is not True:
            code = observed.get("error")
            raise Refusal(code if isinstance(code, str) else "BrowserRefused")
        if any(observed.get(k) != v for k, v in expectations.items()):
            raise Refusal("IdentityMismatch")
        tab = observed.get("tab")
        if type(tab) is not int or tab < 0 or ("tab" in args and tab != args["tab"]):
            raise Refusal("TabMismatch")
        return {
            k: observed.get(k) for k in ("email", "url", "account", "tab", "result")
        }


@dataclass(eq=False)
class Claim:
    peer: Peer
    tab: int
    owner: str
    token: str
    deadline: float
    busy: bool = False


@dataclass
class Session:
    row: sqlite3.Row
    owner: str = field(default_factory=lambda: secrets.token_urlsafe(12))
    journal: dict[str, tuple[Frame, Frame]] = field(default_factory=dict)

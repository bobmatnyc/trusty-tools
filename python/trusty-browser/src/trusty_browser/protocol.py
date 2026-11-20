"""Length-framed JSON and peer authentication for Trusty Browser's Unix sockets."""

from __future__ import annotations

import asyncio
import hmac
import json
import os
import secrets
import socket
import sqlite3
import struct
import sys
import uuid
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from trusty_browser.codes import Grant

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

    async def page(self, op: str, grant: str, **args: object) -> object:
        """Send one page command (email expectation only) and return its ``result``.

        Why: a paired profile has no site route, and a page refusal carries only
        the fields the contract names (#11239). What: a lost reply is
        ``OutcomeUnknown`` for a mutating op, else ``BrowserDisconnected``, and
        so is a worker's own ``OutcomeUnknown`` or ``BrowserDisconnected``; an
        unknown error name becomes ``BrowserRefused``.
        Test: `test_lost_reply_on_click_type_navigate_is_outcome_unknown_and_not_retried`,
        `test_worker_outcome_unknown_is_journaled_and_a_retry_sends_nothing`,
        `test_document_changed_is_relayed_without_extra_fields_and_not_journaled`.
        """
        ident = secrets.token_urlsafe(18)
        reply = asyncio.get_running_loop().create_future()
        self.pending[ident] = reply
        email = str(self.row["email"])
        frame = {"id": ident, "op": op, "email": email, "grant": grant, **args}
        try:
            await write_frame(self.writer, frame)
            observed = await asyncio.wait_for(reply, PAGE_WAIT)
        except (ConnectionError, TimeoutError, OSError) as exc:
            self.writer.close()
            raise Refusal(
                "OutcomeUnknown" if op in MUTATING else "BrowserDisconnected"
            ) from exc
        finally:
            self.pending.pop(ident, None)
        if observed.get("ok") is not True:
            code = observed.get("error")
            if code not in PAGE_ERRORS:
                raise Refusal("BrowserRefused")
            if code in UNFINISHED:
                # #11239: a click the worker could not finish may have run; a read is retry-safe.
                raise Refusal(
                    "OutcomeUnknown" if op in MUTATING else "BrowserDisconnected"
                )
            extra = {k: observed[k] for k in REFUSAL_FIELDS if k in observed}
            raise Refusal(str(code), **extra)
        if observed.get("email") != email:
            raise Refusal("IdentityMismatch")
        if "tab" in args and observed.get("tab") != args["tab"]:
            raise Refusal("TabMismatch")
        return observed.get("result")


PAGE_WAIT = 8.0  # the worker answers within 7 s
# #11291: a scroll can load content, so it is journaled like a click; #11286:
# page_press and #11285: page_select too.
# #11292: history and reload change the page, so they are journaled like navigate.
HISTORY = {"page_back", "page_forward", "page_reload"}
MUTATING = {
    *HISTORY,
    "page_click",
    "page_hover",
    "page_type",
    "page_press",
    "page_select",
    "page_scroll",
    "page_navigate",
    "page_upload",
    "page_drag",
}
# #11352: TextDropped also carries typed, accepted and entered.
REFUSAL_FIELDS = ("tab", "origin", "access", "document")
REFUSAL_FIELDS += ("typed", "accepted", "entered")
# Worker names for a command it could not finish; relayed without fields.
UNFINISHED = {"OutcomeUnknown", "BrowserDisconnected"}
PAGE_ERRORS = UNFINISHED | {
    "InvalidOperation",
    "InvalidRequest",
    "ProfileMismatch",
    "UnknownGrant",
    "OutOfScope",
    "TabNotFound",
    "PermissionNeeded",
    "DocumentChanged",
    "NavigationPending",  # #11354: page_read before a navigation settled
    "StaleElement",
    "NotEditable",
    "TextDropped",  # #11352: a field refused some of the typed text
    "NotSelectable",
    "NoSuchOption",
    "Obscured",
    "DebuggerBusy",
    "NoHistory",
    "NotDraggable",
    "InvalidUrl",
}


@dataclass(eq=False)
class Claim:
    peer: Peer
    tab: int
    owner: str
    token: str
    deadline: float
    busy: bool = False
    # #11239: code-session leases show no indicator and track the last document.
    indicator: bool = True
    document: str | None = None
    origin: str | None = None


@dataclass
class Session:
    row: sqlite3.Row
    owner: str = field(default_factory=lambda: secrets.token_urlsafe(12))
    journal: dict[str, tuple[Frame, Frame]] = field(default_factory=dict)
    # #11239: set when the session was opened by an activation code.
    grant: Grant | None = None

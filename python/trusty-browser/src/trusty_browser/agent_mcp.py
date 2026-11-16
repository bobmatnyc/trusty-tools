"""Expose Trusty Browser to one agent over stdio MCP, by workflow ticket or a connected profile.

Connect mode contract (#11239): docs/activation-codes.md, "MCP tools".
"""

from __future__ import annotations

import argparse
import base64
import errno
import json
import mimetypes
import os
import re
import socket
import stat
import struct
from collections.abc import Callable
from pathlib import Path
from threading import Lock
from typing import Any, BinaryIO

from mcp.server.fastmcp import FastMCP

from trusty_browser import vault
from trusty_browser.page_tools import add_page_tools

HEADER = struct.Struct("=I")
MAX_FRAME = 1024 * 1024
TIMEOUT = 30.0
UNAVAILABLE = (
    "The Trusty Browser service is not reachable on agent.sock; "
    "start it, then call browser_connect again"
)
NOT_CONNECTED = "No session; call browser_connect first"
ALREADY_CONNECTED = "A session is already held; call browser_disconnect first"


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


def add_tab_tools(server: FastMCP, call: Callable[..., dict[str, Any]]) -> None:
    """Register the four lease tools; both modes share their names and shapes."""

    @server.tool()
    def browser_claim() -> dict[str, Any]:
        """Verify the configured Chrome and site identities, then claim this workflow's tab."""
        return call("claim")

    @server.tool()
    def browser_snapshot(lease: str, request_id: str) -> dict[str, Any]:
        """Read the claimed tab after checking both identities again."""
        return call("snapshot", lease=lease, request_id=request_id)

    @server.tool()
    def browser_click(lease: str, request_id: str, marker: str) -> dict[str, Any]:
        """Submit the marker to the enrolled page. Reuse the request ID for a retry."""
        return call("click", lease=lease, request_id=request_id, marker=marker)

    @server.tool()
    def browser_release(lease: str) -> dict[str, Any]:
        """Release this workflow's claim."""
        return call("release", lease=lease)


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
        wire.settimeout(TIMEOUT)
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
            add_tab_tools(server, call)
            server.run(transport="stdio")


class Refused(Exception):
    """A tool error whose text is one compact JSON error frame."""

    def __init__(self, frame: dict[str, Any]) -> None:
        super().__init__(json.dumps(frame, separators=(",", ":"), allow_nan=False))


def local(error: str, message: str) -> Refused:
    return Refused({"ok": False, "error": error, "message": message})


class CodeSession:
    """At most one attached ``agent.sock`` connection, opened only by ``connect``.

    Why: connect mode must start, list tools and recover with the broker down (#11239).
    What: every socket failure drops the connection and answers
    ``ServiceUnavailable``; a refused attach or ``GrantEnded`` drops it too.
    Test: `test_code_mode_starts_and_lists_tools_with_no_broker`,
    `test_broker_loss_mid_session_answers_unavailable_then_reconnects`.
    """

    def __init__(self, path: Path) -> None:
        self.path = path
        self.lock = Lock()
        self.wire: socket.socket | None = None
        self.stream: BinaryIO | None = None
        # #11239: the last GrantEnded reason, so NotConnected says why.
        self.ended: str | None = None

    def drop(self) -> None:
        if self.stream is not None:
            self.stream.close()
        if self.wire is not None:
            self.wire.close()
        self.wire = self.stream = None

    def alive(self) -> bool:
        """Peek without blocking: a closed or failed socket is dead."""
        assert self.wire is not None
        self.wire.setblocking(False)
        try:
            return self.wire.recv(1, socket.MSG_PEEK) != b""
        except BlockingIOError:
            return True
        except OSError:
            return False
        finally:
            self.wire.settimeout(TIMEOUT)

    def exchange(self, frame: dict[str, Any]) -> dict[str, Any]:
        assert self.wire is not None and self.stream is not None
        data = json.dumps(frame, allow_nan=False).encode()
        if len(data) > MAX_FRAME:
            raise local("InvalidRequest", "The request is too large")
        try:
            self.wire.sendall(HEADER.pack(len(data)) + data)
            return read_reply(self.stream)
        except (OSError, ValueError):
            self.drop()
            raise local("ServiceUnavailable", UNAVAILABLE) from None

    def open(self) -> None:
        wire = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        wire.settimeout(TIMEOUT)
        try:
            wire.connect(str(self.path))
        except OSError:
            wire.close()
            raise local("ServiceUnavailable", UNAVAILABLE) from None
        self.wire, self.stream = wire, wire.makefile("rb")

    def profiles(self) -> dict[str, Any]:
        """List the connected profiles on a one-shot connection; the session is untouched."""
        with self.lock:
            held = self.wire, self.stream
            self.wire = self.stream = None
            try:
                self.open()
                reply = self.exchange({"op": "profiles"})
            finally:
                self.drop()
                self.wire, self.stream = held
        if not reply["ok"]:
            raise Refused(reply)
        return reply

    def connect(self, profile: str | None) -> dict[str, Any]:
        """Attach to a profile the user connected in the popup; no code (#11239).

        Test: `test_connect_needs_no_code_and_attaches_to_the_connected_profile`,
        `test_connect_with_several_profiles_needs_an_explicit_choice`.
        """
        with self.lock:
            if self.wire is not None and self.alive():
                raise local("AlreadyConnected", ALREADY_CONNECTED)
            self.drop()
            self.ended = None  # a new attempt never reports the old grant's end
            self.open()
            hello: dict[str, Any] = {"op": "attach"}
            if profile is not None:
                hello["profile"] = profile
            reply = self.exchange(hello)
            if not reply["ok"]:
                self.drop()  # The broker closes a refused attach; so do we.
                raise Refused(reply)
            return reply

    def call(self, op: str, **fields: object) -> dict[str, Any]:
        with self.lock:
            if self.wire is None:
                frame: dict[str, Any] = {
                    "ok": False,
                    "error": "NotConnected",
                    "message": NOT_CONNECTED,
                }
                if self.ended is not None:
                    frame["reason"] = self.ended
                raise Refused(frame)
            reply = self.exchange({"op": op, **fields})
            if not reply["ok"]:
                if reply.get("error") == "GrantEnded":
                    self.drop()  # The broker closes the socket after this frame.
                    reason = reply.get("reason")
                    self.ended = reason if isinstance(reason, str) else "unknown"
                raise Refused(reply)
            return reply

    def disconnect(self) -> dict[str, Any]:
        with self.lock:
            held = self.wire is not None
            self.drop()
            self.ended = None
        return {"ok": True, "disconnected": held}


UPLOAD_MAX = 512 * 1024


UPLOADS_DISABLED = (
    "Uploads are off: start the MCP server with --upload-root DIR to allow"
    " files under DIR"
)


def upload_root(raw: str) -> Path:
    """Resolve ``--upload-root`` to the real path of an existing directory."""
    root = Path(os.path.realpath(raw))
    if not root.is_dir():
        raise ValueError(f"--upload-root {raw} is not a directory")
    return root


def _inside(root: Path, raw: str) -> list[str]:
    """The path components of ``raw`` below ``root``; raises ``ValueError`` outside it."""
    parts = Path(raw).parts
    if ".." in parts:
        raise ValueError("contains a '..' component")
    if not os.path.isabs(raw):
        rel = Path(*parts) if parts else Path()
    elif os.path.realpath(raw) == str(root):
        rel = Path()
    else:
        # The caller may name the root through an alias (/tmp vs /private/tmp),
        # so resolve only the parent; the final component is never followed.
        parent = Path(os.path.realpath(os.path.dirname(raw)))
        if parent != root and root not in parent.parents:
            raise ValueError("is outside the upload root")
        rel = parent.relative_to(root) / os.path.basename(raw)
    comps = [c for c in rel.parts if c not in ("", ".")]
    if not comps:
        raise ValueError("is not a regular file")
    return comps


def upload_file(
    raw: object, root: Path | None, budget: int = UPLOAD_MAX
) -> dict[str, str]:
    """One ``page_upload`` file entry read from a file under the upload root (#11289).

    Why: the browser cannot read the agent's disk, and a hostile page must not
    steer the agent into sending ~/.ssh or .env (PR #11305 review).
    What: with no root every upload is ``UploadsDisabled``. A path relative to
    ``root``, or absolute and inside it, is opened one component at a time from
    a ``root`` directory fd with ``O_NOFOLLOW``, so no symlink anywhere below
    the root is followed and the file cannot escape it. A ``..`` component, a
    path outside the root, a symlink, and anything but a readable regular file
    of at most ``UPLOAD_MAX`` bytes is ``FileRefused`` naming path and reason.
    The entry is ``{name, type, data}`` (basename, guessed MIME type, base64).
    Test: `test_page_upload_reads_local_files_and_refuses_bad_paths`,
    tests/test_upload_root.py `test_symlink_escape_is_refused`.
    """
    if root is None:
        raise local("UploadsDisabled", UPLOADS_DISABLED)
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise local("InvalidRequest", "paths must be non-empty file path strings")

    def refuse(reason: str) -> Refused:
        return local("FileRefused", f"{raw}: {reason}")

    try:
        comps = _inside(root, raw)
    except ValueError as error:
        raise refuse(str(error)) from None
    # PR #11305 delta review: every fd, the leaf's included, is closed in the
    # one finally, whatever raises after its open (fstat, read, a refusal).
    fds: list[int] = []
    try:
        fds.append(os.open(root, os.O_RDONLY | os.O_DIRECTORY))
        for comp in comps[:-1]:
            flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
            fds.append(os.open(comp, flags, dir_fd=fds[-1]))
        # O_NONBLOCK: opening a FIFO must not hang the tool.
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        fds.append(os.open(comps[-1], flags, dir_fd=fds[-1]))
        if not stat.S_ISREG(os.fstat(fds[-1]).st_mode):
            raise refuse("is not a regular file")
        chunks: list[bytes] = []
        got = 0
        while got <= budget:
            chunk = os.read(fds[-1], budget + 1 - got)
            if not chunk:
                break
            chunks.append(chunk)
            got += len(chunk)
        data = b"".join(chunks)
    except FileNotFoundError:
        raise refuse("does not exist") from None
    except NotADirectoryError:
        raise refuse("is a symlink or not under a directory") from None
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise refuse("is a symlink") from None
        raise refuse("cannot be read") from None
    finally:
        for opened in fds:
            os.close(opened)
    if len(data) > budget:
        if budget < UPLOAD_MAX:
            raise refuse(f"puts the upload over {UPLOAD_MAX} bytes in total")
        raise refuse(f"is larger than {UPLOAD_MAX} bytes")
    name = comps[-1]
    kind = mimetypes.guess_type(name)[0] or "application/octet-stream"
    return {"name": name, "type": kind, "data": base64.b64encode(data).decode()}


UPLOAD_FILES = 10


def upload_files(paths: object, root: Path | None) -> list[dict[str, str]]:
    """Every ``page_upload`` entry, refusing too many paths before any read.

    Why: a caller can send 1000 paths; none may be opened (PR #11305 delta review).
    What: no root is ``UploadsDisabled``; anything but a list of 1 to
    ``UPLOAD_FILES`` paths is ``InvalidRequest``. Each file reads at most the
    bytes left of ``UPLOAD_MAX``, so the total is bounded while reading.
    Test: tests/test_upload_root.py `test_too_many_paths_are_refused_before_any_read`.
    """
    if root is None:
        raise local("UploadsDisabled", UPLOADS_DISABLED)
    if not isinstance(paths, list) or not 1 <= len(paths) <= UPLOAD_FILES:
        raise local("InvalidRequest", f"paths must list 1 to {UPLOAD_FILES} files")
    entries: list[dict[str, str]] = []
    left = UPLOAD_MAX
    for raw in paths:
        entry = upload_file(raw, root, left)
        left -= len(base64.b64decode(entry["data"]))
        entries.append(entry)
    return entries


def run_codes(state: Path, root: Path | None = None) -> None:
    """Serve connect mode: no credential, no socket until ``browser_connect``.

    ``root`` is the only directory ``page_upload`` may read; ``None`` refuses uploads.
    """
    session = CodeSession(state / "agent.sock")
    server = FastMCP("trusty-browser")

    @server.tool()
    def browser_profiles() -> dict[str, Any]:
        """List the Chrome profiles the user connected with the popup's Connect button."""
        return session.profiles()

    @server.tool()
    def browser_connect(profile: str | None = None) -> dict[str, Any]:
        """Attach to a Chrome profile the user connected in the Trusty Browser popup.

        No code is needed. With one connected profile, omit ``profile``. With
        several, call browser_profiles and pass the chosen ``profile`` id.
        """
        if profile is not None and (not isinstance(profile, str) or not profile):
            raise local("InvalidRequest", "profile must be a non-empty string")
        return session.connect(profile)

    @server.tool()
    def browser_disconnect() -> dict[str, Any]:
        """Close this agent's session; the profile stays connected until the user disconnects it."""
        return session.disconnect()

    add_page_tools(server, session.call, root)
    try:
        server.run(transport="stdio")
    finally:
        session.disconnect()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state", type=Path, default=Path.home() / ".trusty" / "browser"
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--workflow", help="Workflow ID shown under Activation details")
    group.add_argument(
        "--credential", type=Path, help="Explicit private workflow credential file"
    )
    parser.add_argument(
        "--upload-root",
        help="Directory page_upload may read files from; without it uploads are refused",
    )
    args = parser.parse_args()
    root = None
    if args.upload_root is not None:
        try:
            root = upload_root(args.upload_root)
        except ValueError as error:
            parser.error(str(error))
    credential = args.credential
    if args.workflow is not None:
        if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", args.workflow):
            parser.error("invalid workflow name")
        credential = args.state / "workflows" / (args.workflow + ".json")
    if credential is None:
        run_codes(args.state, root)
    else:
        run(args.state, credential)


if __name__ == "__main__":
    main()

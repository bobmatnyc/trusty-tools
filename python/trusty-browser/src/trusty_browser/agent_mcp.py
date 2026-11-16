"""Expose Trusty Browser to one agent over stdio MCP, by workflow ticket or activation code.

Code mode contract (#11239): docs/activation-codes.md, "MCP tools".
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
from typing import Annotated, Any, BinaryIO

from mcp.server.fastmcp import FastMCP
from pydantic import Field, WithJsonSchema

from trusty_browser import vault

HEADER = struct.Struct("=I")
MAX_FRAME = 1024 * 1024
TIMEOUT = 30.0
UNAVAILABLE = (
    "The Trusty Browser service is not reachable on agent.sock; "
    "start it, then call browser_connect again"
)
NOT_CONNECTED = "No activation code session; call browser_connect with a code first"
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
    """A tool error whose text is one compact JSON error frame, never a code."""

    def __init__(self, frame: dict[str, Any]) -> None:
        super().__init__(json.dumps(frame, separators=(",", ":"), allow_nan=False))


def local(error: str, message: str) -> Refused:
    return Refused({"ok": False, "error": error, "message": message})


class CodeSession:
    """At most one redeemed ``agent.sock`` connection, opened only by ``connect``.

    Why: code mode must start, list tools and recover with the broker down (#11239).
    What: every socket failure drops the connection and answers
    ``ServiceUnavailable``; a refused redeem or ``GrantEnded`` drops it too.
    Test: `test_code_mode_starts_and_lists_tools_with_no_broker`,
    `test_broker_loss_mid_session_answers_unavailable_then_reconnects`.
    """

    def __init__(self, path: Path) -> None:
        self.path = path
        self.lock = Lock()
        self.wire: socket.socket | None = None
        self.stream: BinaryIO | None = None

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

    def connect(self, code: str) -> dict[str, Any]:
        with self.lock:
            if self.wire is not None and self.alive():
                raise local("AlreadyConnected", ALREADY_CONNECTED)
            self.drop()
            wire = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            wire.settimeout(TIMEOUT)
            try:
                wire.connect(str(self.path))
            except OSError:
                wire.close()
                raise local("ServiceUnavailable", UNAVAILABLE) from None
            self.wire, self.stream = wire, wire.makefile("rb")
            reply = self.exchange({"op": "redeem", "code": code})
            if not reply["ok"]:
                self.drop()  # The broker closes a refused redeem; so do we.
                raise Refused(reply)
            return reply

    def call(self, op: str, **fields: object) -> dict[str, Any]:
        with self.lock:
            if self.wire is None:
                raise local("NotConnected", NOT_CONNECTED)
            reply = self.exchange({"op": op, **fields})
            if not reply["ok"]:
                if reply.get("error") == "GrantEnded":
                    self.drop()  # The broker closes the socket after this frame.
                raise Refused(reply)
            return reply

    def disconnect(self) -> dict[str, Any]:
        with self.lock:
            held = self.wire is not None
            self.drop()
        return {"ok": True, "disconnected": held}


class _Absent:
    """Marks a ``browser_connect`` argument the caller left out (unlike ``null``)."""


ABSENT: Any = _Absent()
CODE_FILE_MAX = 1024
ONE_INPUT = "Give exactly one of code or code_file"


def _optional(description: str) -> Any:
    # #11239: unvalidated, so a non-string value is never quoted back; a
    # default_factory keeps the default out of the schema and tells absent from null.
    return Annotated[
        Any,
        WithJsonSchema({"type": "string", "description": description}),
        Field(default_factory=lambda: ABSENT),
    ]


CodeArg = _optional(
    "The activation code itself. It lands in your transcript; prefer code_file."
)
CodeFileArg = _optional(
    "Path to an owner-only file (mode 0600 or 0400, not a symlink) holding the"
    " activation code. Preferred: the code never enters your transcript."
)


def code_from_file(raw: object) -> str:
    """Read an activation code from an owner-only file without ever echoing it.

    Why: a code is a secret; a path keeps it out of the agent's transcript (#11239).
    What: refuses a symlink (``lstat`` and ``O_NOFOLLOW``), anything but a regular
    file owned by this user with no group or other bits, and a missing,
    unreadable, oversize, non-UTF-8 or blank file. Strips surrounding whitespace.
    A refusal is ``CodeFileRefused`` naming the path and the reason only.
    Test: `test_code_file_refusals_name_the_path_never_the_code`,
    `test_code_file_redeems_through_the_same_redeem_as_code`.
    """
    if not isinstance(raw, str) or not raw:
        raise local("InvalidRequest", "code_file must be a non-empty path string")

    def refuse(reason: str) -> Refused:
        return local("CodeFileRefused", f"code_file {raw}: {reason}")

    try:
        if stat.S_ISLNK(os.lstat(raw).st_mode):
            raise refuse("is a symlink")
        fd = os.open(raw, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        raise refuse("does not exist") from None
    except OSError as error:
        reason = "is a symlink" if error.errno == errno.ELOOP else "cannot be read"
        raise refuse(reason) from None
    with os.fdopen(fd, "rb") as handle:
        attrs = os.fstat(handle.fileno())
        if not stat.S_ISREG(attrs.st_mode):
            raise refuse("is not a regular file")
        if attrs.st_uid != os.getuid():
            raise refuse("is not owned by the current user")
        if attrs.st_mode & 0o077:
            raise refuse("must be owner-only (mode 0600 or 0400)")
        try:
            data = handle.read(CODE_FILE_MAX + 1)
        except OSError:
            raise refuse("cannot be read") from None
    if len(data) > CODE_FILE_MAX:
        raise refuse(f"is larger than {CODE_FILE_MAX} bytes")
    try:
        code = data.decode("utf-8").strip()
    except UnicodeDecodeError:
        raise refuse("is not UTF-8 text") from None
    if not code:
        raise refuse("is empty")
    return code


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


def add_page_tools(
    server: FastMCP, call: Callable[..., dict[str, Any]], root: Path | None = None
) -> None:
    """Register code mode's page tools; contract "MCP tools", "Code mode".

    Test: `test_end_to_end_page_session_through_the_mcp_server`.
    """

    @server.tool()
    def browser_tabs() -> dict[str, Any]:
        """List the tabs this grant may address, with page access, origin and title."""
        return call("tabs")

    @server.tool()
    def browser_select(tab: int) -> dict[str, Any]:
        """Lease a tab by id for this session (does not focus it); returns the lease."""
        return call("select", tab=tab)

    @server.tool()
    def page_read(lease: str) -> dict[str, Any]:
        """Read the leased tab's page: text and elements with refs for click and type."""
        return call("page_read", lease=lease)

    @server.tool()
    def page_click(lease: str, ref: str, request_id: str) -> dict[str, Any]:
        """Click the element a page_read returned as ``ref``. Reuse the request ID to retry."""
        return call("page_click", lease=lease, ref=ref, request_id=request_id)

    @server.tool()
    def page_type(lease: str, ref: str, text: str, request_id: str) -> dict[str, Any]:
        """Replace the text of an editable element named by ``ref``."""
        return call("page_type", lease=lease, ref=ref, text=text, request_id=request_id)

    @server.tool()
    def page_scroll(
        lease: str,
        request_id: str,
        to: str = "down",
        ref: str | None = None,
        wait_ms: int = 1000,
    ) -> dict[str, Any]:
        """Scroll the leased tab, then wait up to ``wait_ms`` for new content.

        Why: offscreen and infinite-scroll content loads only on scroll (#11291).
        What: ``to`` is down or up (90% of the viewport), top, bottom, or
        element with ``ref`` from the latest page_read. The result carries
        scroll_y, scroll_height, viewport_height, grew (content was added) and
        at_end. For a feed, repeat ``to="bottom"`` until grew is false, then
        page_read again: refs from an earlier read answer StaleElement, as does
        a ref whose element the page removed. Reuse the request ID to retry.
        Test: `test_end_to_end_page_session_through_the_mcp_server`.
        """
        return call(
            "page_scroll",
            lease=lease,
            to=to,
            ref=ref,
            wait_ms=wait_ms,
            request_id=request_id,
        )

    @server.tool()
    def page_navigate(lease: str, url: str, request_id: str) -> dict[str, Any]:
        """Navigate the leased tab to an https URL (or http://127.0.0.1)."""
        return call("page_navigate", lease=lease, url=url, request_id=request_id)

    @server.tool()
    def page_upload(
        lease: str, ref: str, paths: list[str], request_id: str
    ) -> dict[str, Any]:
        """Set local files on the <input type=file> named by ``ref``; the page sees a user's pick.

        ``paths`` name files under the server's --upload-root (relative to it, or
        absolute inside it), at most 10 and 512 KiB in total. Without
        --upload-root, or for a symlink or a path outside the root, it refuses.
        """
        return call(
            "page_upload",
            lease=lease,
            ref=ref,
            files=upload_files(paths, root),
            request_id=request_id,
        )

    @server.tool()
    def page_downloads(lease: str) -> dict[str, Any]:
        """List the newest downloads started from the leased tab's origin, with path and state."""
        return call("page_downloads", lease=lease)

    @server.tool()
    def browser_release(lease: str) -> dict[str, Any]:
        """Release a lease taken with browser_select."""
        return call("release", lease=lease)


def run_codes(state: Path, root: Path | None = None) -> None:
    """Serve code mode: no credential, no socket until ``browser_connect``.

    ``root`` is the only directory ``page_upload`` may read; ``None`` refuses uploads.
    """
    session = CodeSession(state / "agent.sock")
    server = FastMCP("trusty-browser")

    @server.tool()
    def browser_connect(code: CodeArg, code_file: CodeFileArg) -> dict[str, Any]:
        """Redeem a one-use activation code (tb-...) from the Trusty Browser popup.

        Give exactly one of code_file or code. Prefer code_file, the path of the
        owner-only file the owner saved the code to, so the code stays secret.
        """
        if (code is ABSENT) == (code_file is ABSENT):
            raise local("InvalidRequest", ONE_INPUT)
        if code_file is not ABSENT:
            code = code_from_file(code_file)
        elif not isinstance(code, str):
            raise local("InvalidRequest", "code must be a string")
        return session.connect(code)

    @server.tool()
    def browser_disconnect() -> dict[str, Any]:
        """Close the session; the broker ends its grant."""
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

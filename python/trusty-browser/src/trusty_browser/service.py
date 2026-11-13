"""Route workflow-scoped browser operations over private, length-framed Unix sockets."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import fcntl
import os
import secrets
import sqlite3
import stat
import sys
import time
from pathlib import Path
from typing import BinaryIO
from trusty_browser.enrollment import (
    database,
    digest,
    finish_removals,
    inspect_conflict,
    browser_key,
    register,
    remove,
    replace,
    status,
)
from trusty_browser.mac_peer import verify_chrome_host

from trusty_browser.protocol import (
    Peer,
    Claim,
    Session,
    Frame,
    Refusal,
    pairing_proof,
    read_frame,
    string,
    verify_peer,
    write_frame,
)

TTL = 60.0


class Broker:
    def __init__(self, state: Path) -> None:
        self.state = state.expanduser().absolute()
        self.sessions: dict[asyncio.StreamWriter, Session] = {}
        self.peers: dict[str, Peer] = {}
        self.claims: dict[tuple[str, int], Claim] = {}
        self.servers: list[asyncio.AbstractServer] = []
        self.writers: set[asyncio.StreamWriter] = set()
        self.paths: list[Path] = []
        self.lock: BinaryIO | None = None
        self.db: sqlite3.Connection | None = None

    async def __aenter__(self) -> Broker:
        self.db = database(self.state)
        try:
            self.lock = (self.state / "service.lock").open("ab")
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finish_removals(self.db, self.state)
            for name, handler in (
                ("service.sock", self.browser),
                ("agent.sock", self.agent),
                ("enrollment.sock", self.bootstrap),
                ("operator.sock", self.operator),
            ):
                path = self.state / name
                if path.exists():
                    if not stat.S_ISSOCK(path.lstat().st_mode):
                        raise ValueError("socket path is occupied")
                    path.unlink()
                server = await asyncio.start_unix_server(handler, path)
                self.servers.append(server)
                self.paths.append(path)
                path.chmod(0o600)
        except BaseException:
            await self.close()
            raise
        return self

    async def close(self) -> None:
        for server in self.servers:
            server.close()
        for writer in tuple(self.writers):
            writer.close()
        for server in self.servers:
            await server.wait_closed()
        for path in self.paths:
            path.unlink(missing_ok=True)
        self.paths.clear()
        if self.db is not None:
            self.db.close()
            self.db = None
        if self.lock is not None:
            self.lock.close()
            self.lock = None

    async def __aexit__(self, *_: object) -> None:
        await self.close()

    def route(self, request: Frame, browser: bool) -> sqlite3.Row:
        assert self.db is not None
        field = "profile" if browser else "workflow"
        row = self.db.execute(
            f"SELECT * FROM routes WHERE {field} = ?", (string(request, field),)
        ).fetchone()
        if not isinstance(row, sqlite3.Row) or (
            not browser
            and not secrets.compare_digest(
                row["ticket"], digest(string(request, "ticket"))
            )
        ):
            raise Refusal("Unauthorized")
        return row

    async def pair(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> tuple[sqlite3.Row, str]:
        hello = await read_frame(reader)
        fields = {"type", "profile", "email", "incarnation", "nonce"}
        if set(hello) != fields or hello["type"] != "hello":
            raise Refusal("InvalidRequest")
        row = self.route(hello, True)
        if row["email"] != string(hello, "email").casefold():
            raise Refusal("ProfileMismatch")
        client_nonce = string(hello, "nonce")
        if len(client_nonce) != 64 or any(
            c not in "0123456789abcdef" for c in client_nonce
        ):
            raise Refusal("InvalidNonce")
        incarnation = string(hello, "incarnation")
        nonce = secrets.token_hex(32)
        key = browser_key(self.state, row)
        await write_frame(
            writer,
            {
                "type": "challenge",
                "nonce": nonce,
                "proof": pairing_proof(key, "broker", hello, nonce),
            },
        )
        response = await read_frame(reader)
        expected = pairing_proof(key, "extension", hello, nonce)
        if response.get("type") != "proof" or not secrets.compare_digest(
            expected.encode(), string(response, "proof").encode()
        ):
            raise Refusal("Unauthorized")
        return row, incarnation

    def forget(self, peer: Peer) -> None:
        if self.peers.get(peer.row["profile"]) is peer:
            del self.peers[peer.row["profile"]]
        for key, claim in list(self.claims.items()):
            if claim.peer is peer:
                del self.claims[key]
        for future in peer.pending.values():
            if not future.done():
                future.set_exception(ConnectionResetError())

    async def bootstrap(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self.writers.add(writer)
        try:
            verify_peer(writer)
            try:
                await asyncio.wait_for(
                    asyncio.to_thread(
                        verify_chrome_host, writer.get_extra_info("socket"), self.state
                    ),
                    7,
                )
            except (PermissionError, OSError, TimeoutError):
                raise Refusal("Unauthorized") from None
            # Read the enrollment key only after OS authentication.
            request = await asyncio.wait_for(read_frame(reader), 5)
            assert self.db is not None
            response = await self.enrollment_request(request)
            await write_frame(writer, response)
        except (ValueError, OSError, sqlite3.Error, TimeoutError) as exc:
            safe = {
                "InvalidEnrollment",
                "EnrollmentConflict",
                "EnrollmentRevoked",
                "ReplacementStale",
                "CredentialConflict",
                "WorkflowCredentialOccupied",
                "Unauthorized",
            }
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(
                    writer,
                    {
                        "ok": False,
                        "error": str(exc) if str(exc) in safe else "EnrollmentFailed",
                    },
                )
        except EOFError:
            pass
        finally:
            self.writers.discard(writer)
            writer.close()

    async def enrollment_request(self, request: Frame) -> Frame:
        assert self.db is not None
        op = string(request, "op")
        workflow = string(request, "workflow")
        if op == "status":
            return status(self.db, self.state, request)
        if op == "unenroll":
            await self.revoke(workflow, request)
            return {"ok": True, "workflow": workflow}
        if op == "replace":
            retired = replace(self.db, self.state, request)
            if retired is not None:
                await self.detach(retired)
            request = {k: v for k, v in request.items() if k != "previous_version"}
            request["op"] = "enroll"
        try:
            return register(self.db, self.state, request)
        except ValueError as exc:
            if op == "enroll" and str(exc) == "EnrollmentConflict":
                prior = inspect_conflict(self.db, workflow)
                if prior is not None:
                    return {
                        "ok": False,
                        "error": "EnrollmentConflict",
                        "previous": prior,
                    }
            raise

    async def revoke(self, workflow: str, request: Frame | None = None) -> None:
        assert self.db is not None
        profile = remove(self.db, self.state, workflow, request)
        if profile is not None:
            await self.detach(profile)
        finish_removals(self.db, self.state)

    async def detach(self, profile: str) -> None:
        for w, s in list(self.sessions.items()):
            if s.row["profile"] == profile:
                w.close()
        peer = self.peers.get(profile)
        if peer is not None:
            # Drop the generation synchronously before waiting on any indicator.
            old = [c for c in self.claims.values() if c.peer is peer]
            self.forget(peer)
            try:
                await asyncio.gather(*(self.clear(c) for c in old))
            finally:
                peer.writer.close()

    async def operator(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        try:
            verify_peer(writer)
            req = await asyncio.wait_for(read_frame(reader), 5)
            if set(req) != {"op", "workflow"} or req["op"] != "revoke":
                raise Refusal("InvalidRequest")
            await self.revoke(string(req, "workflow"))
            await write_frame(writer, {"ok": True})
        except (ValueError, OSError, EOFError, TimeoutError):
            with contextlib.suppress(OSError):
                await write_frame(writer, {"ok": False, "error": "RevokeFailed"})
        finally:
            writer.close()

    async def browser(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self.writers.add(writer)
        peer: Peer | None = None
        try:
            verify_peer(writer)
            row, incarnation = await asyncio.wait_for(self.pair(reader, writer), 5)
            # Revocation may occur while the browser is computing its proof.
            current = self.route({"profile": row["profile"]}, True)
            if (
                current["browser_key"] != row["browser_key"]
                or current["ticket"] != row["ticket"]
            ):
                raise Refusal("Unauthorized")
            name = row["profile"]
            if name in self.peers:
                raise Refusal("DuplicateProfile")
            peer = Peer(writer, row, incarnation)
            self.peers[name] = peer
            await write_frame(writer, {"ok": True, "generation": peer.generation})
            while True:
                reply = await read_frame(reader)
                pending = peer.pending.get(string(reply, "id"))
                if pending is None or pending.done():
                    raise Refusal("UnexpectedReply")
                pending.set_result(reply)
        except Refusal as exc:
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(writer, exc.response)
        except (EOFError, OSError, ValueError, TimeoutError):
            pass
        finally:
            if peer is not None:
                self.forget(peer)
            self.writers.discard(writer)
            writer.close()

    async def agent(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self.writers.add(writer)
        session: Session | None = None
        try:
            verify_peer(writer)
            hello = await asyncio.wait_for(read_frame(reader), 5)
            if set(hello) != {"op", "workflow", "ticket"} or hello.get("op") != "open":
                raise Refusal("InvalidRequest")
            session = Session(self.route(hello, False))
            self.sessions[writer] = session
            await write_frame(writer, {"ok": True})
            while True:
                request = await read_frame(reader)
                try:
                    response = await self.dispatch(session, request)
                except Refusal as exc:
                    response = exc.response
                await write_frame(writer, response)
        except Refusal as exc:
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(writer, exc.response)
        except (EOFError, OSError, ValueError, TimeoutError):
            pass
        finally:
            self.sessions.pop(writer, None)
            if session is not None:
                for key, claim in list(self.claims.items()):
                    if claim.owner == session.owner:
                        del self.claims[key]
                        await self.clear(claim)
            self.writers.discard(writer)
            writer.close()

    async def indicator(self, claim: Claim, show: bool) -> None:
        await claim.peer.command(
            "lease_show" if show else "lease_clear",
            tab=claim.tab,
            lease=claim.token,
            workflow=claim.peer.row["workflow"],
            ttl_ms=int(TTL * 1000),
        )

    async def clear(self, claim: Claim) -> None:
        with contextlib.suppress(Refusal):
            await self.indicator(claim, False)

    def active(self, peer: Peer, tab: int) -> Claim | None:
        key = (peer.generation, tab)
        claim = self.claims.get(key)
        if claim is not None and not claim.busy and claim.deadline <= time.monotonic():
            del self.claims[key]
            return None
        return claim

    async def dispatch(self, session: Session, request: Frame) -> Frame:
        op = string(request, "op")
        allowed = {
            "claim": {"op"},
            "release": {"op", "lease"},
            "click": {"op", "lease", "request_id", "marker"},
            "snapshot": {"op", "lease", "request_id"},
        }
        if op not in allowed or set(request) != allowed[op]:
            raise Refusal("InvalidRequest")
        profile = str(session.row["profile"])
        assert self.db is not None
        active = self.db.execute(
            "SELECT ticket FROM routes WHERE profile=?", (profile,)
        ).fetchone()
        if active is None or active["ticket"] != session.row["ticket"]:
            raise Refusal("Unauthorized")
        peer = self.peers.get(profile)
        if peer is None:
            raise Refusal("BrowserDisconnected" if op == "claim" else "StaleLease")
        if op == "claim":
            obs = await peer.command("inspect")
            tab = obs["tab"]
            assert isinstance(tab, int)
            if self.peers.get(profile) is not peer:
                raise Refusal("StaleLease")
            claim = self.active(peer, tab)
            if claim is not None and claim.owner != session.owner:
                raise Refusal(
                    "TabClaimed",
                    holder=claim.owner,
                    expires_at=time.time()
                    + max(0.0, claim.deadline - time.monotonic()),
                )
            if claim is None:
                claim = Claim(
                    peer,
                    tab,
                    session.owner,
                    secrets.token_urlsafe(24),
                    time.monotonic() + TTL,
                )
                self.claims[(peer.generation, tab)] = claim
                try:
                    await self.indicator(claim, True)
                    if self.active(peer, tab) is not claim:
                        raise Refusal("StaleLease")
                except Refusal:
                    if self.claims.get((peer.generation, tab)) is claim:
                        del self.claims[(peer.generation, tab)]
                    await self.clear(claim)
                    raise
            return {
                "ok": True,
                "lease": claim.token,
                "generation": peer.generation,
                "incarnation": peer.incarnation,
                **obs,
            }
        token = string(request, "lease")
        claim = next(
            (
                c
                for c in self.claims.values()
                if c.owner == session.owner and c.token == token
            ),
            None,
        )
        if (
            claim is None
            or claim.peer is not peer
            or self.active(peer, claim.tab) is not claim
        ):
            raise Refusal("StaleLease")
        if op == "release":
            del self.claims[(peer.generation, claim.tab)]
            await self.clear(claim)
            return {"ok": True}
        if op not in {"click", "snapshot"}:
            raise Refusal("InvalidRequest")
        rid = string(request, "request_id")
        previous = session.journal.get(rid)
        if previous is not None:
            if request != previous[0]:
                raise Refusal("Conflict")
            # A cached acknowledgement may contain authenticated page contents. Re-attest
            # this tab before releasing it, even when the original action already ran.
            await peer.command("snapshot", tab=claim.tab)
            if (
                self.peers.get(profile) is not peer
                or self.active(peer, claim.tab) is not claim
            ):
                raise Refusal("StaleLease")
            return previous[1]
        if len(session.journal) >= 4096:
            raise Refusal("SessionFull")
        args: Frame = {"tab": claim.tab}
        if op == "click":
            args["marker"] = string(request, "marker")
        session.journal[rid] = (request, {"ok": False, "error": "OutcomeUnknown"})
        claim.busy = True
        try:
            # Refresh the visible deadline before dispatch so an in-flight action can
            # never outlive the existing indicator near the end of a lease.
            await self.indicator(claim, True)
            if self.claims.get((peer.generation, claim.tab)) is not claim:
                raise Refusal("StaleLease")
            claim.deadline = time.monotonic() + TTL
            obs = await peer.command(op, **args)
            if self.peers.get(profile) is not peer:
                raise Refusal("OutcomeUnknown" if op == "click" else "StaleLease")
        except Refusal as exc:
            self.claims.pop((peer.generation, claim.tab), None)
            await self.clear(claim)
            session.journal[rid] = (request, exc.response)
            raise
        finally:
            claim.busy = False
        result: Frame = {"ok": True, **obs}
        session.journal[rid] = (request, result)
        return result


async def operator_revoke(state: Path, workflow: str) -> None:
    try:
        if not (state / "operator.sock").exists():
            raise FileNotFoundError
        reader, writer = await asyncio.open_unix_connection(state / "operator.sock")
    except (FileNotFoundError, ConnectionRefusedError):
        # The same singleton lock covers offline registry changes and service startup.
        with (
            contextlib.closing(database(state)) as db,
            (state / "service.lock").open("ab") as lock,
        ):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            remove(db, state, workflow)
            finish_removals(db, state)
        return
    try:
        await write_frame(writer, {"op": "revoke", "workflow": workflow})
        reply = await asyncio.wait_for(read_frame(reader), 15)
        if reply.get("ok") is not True:
            raise ValueError("RevokeFailed")
    finally:
        writer.close()


async def serve(state: Path) -> None:
    async with Broker(state):
        await asyncio.Event().wait()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--state", type=Path, default=Path.home() / ".trusty" / "browser"
    )
    verbs = parser.add_subparsers(dest="command", required=True)
    verbs.add_parser("serve")
    verbs.add_parser("revoke").add_argument("workflow")
    args = parser.parse_args()
    try:
        if args.command == "revoke":
            asyncio.run(operator_revoke(args.state, args.workflow))
        else:
            asyncio.run(serve(args.state))
    except KeyboardInterrupt:
        pass
    except (ValueError, OSError, sqlite3.Error) as exc:
        sys.exit(str(exc))


if __name__ == "__main__":
    main()

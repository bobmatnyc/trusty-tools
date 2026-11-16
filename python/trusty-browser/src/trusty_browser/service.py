"""Route workflow-scoped browser operations over private, length-framed Unix sockets."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import fcntl
import secrets
import sqlite3
import stat
import sys
import time  # noqa: F401 -- tests/test_service.py patches service.time
from pathlib import Path
from typing import BinaryIO, Self

from trusty_browser import assets, codes, pairing
from trusty_browser.audit import email_domain
from trusty_browser.enrollment import (
    browser_key,
    database,
    digest,
    finish_removals,
    remove,
)
from trusty_browser.grants import OPERATOR_OPS, POPUP_OPS, RELAYED
from trusty_browser.mac_peer import HostRefused, verify_chrome_host
from trusty_browser.pages import Pages
from trusty_browser.protocol import (
    Claim,
    Frame,
    Peer,
    Refusal,
    Session,
    pairing_proof,
    read_frame,
    string,
    verify_peer,
    write_frame,
)


# #11239: Pages (code-session ops) layers over Grants, which layers over Leases.
class Broker(Pages):
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
        self.init_grants()

    async def __aenter__(self) -> Self:
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
        self.halt()
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
        if browser:  # #11239: a paired profile pairs its native port too.
            row = pairing.profile_row(self.db, string(request, "profile"))
        else:
            row = self.db.execute(
                "SELECT * FROM routes WHERE workflow = ?",
                (string(request, "workflow"),),
            ).fetchone()
        if not isinstance(row, sqlite3.Row):
            raise self.refused("service" if browser else "agent", "route_unknown", request)
        if not browser and not secrets.compare_digest(
            row["ticket"], digest(string(request, "ticket"))
        ):
            raise self.refused("agent", "ticket_mismatch", request)
        return row

    def refused(self, site: str, reason: str, request: Frame | None = None) -> Refusal:
        """Log a refusal (ruling Dqw 2026-11-16) and return its ``Unauthorized`` reply.

        Test: `test_each_refusal_site_writes_one_domain_only_audit_line`.
        """
        request = request or {}
        profile = pairing.profile_id(request.get("profile"))
        domain = email_domain(request.get("email"))
        self.audit.refuse(site, reason, profile=profile, email_domain=domain)
        return Refusal("Unauthorized", reason=reason)

    def peer_ok(self, writer: asyncio.StreamWriter, site: str) -> None:
        """``verify_peer``, logging a refusal as ``peer_uid``."""
        try:
            verify_peer(writer)
        except Refusal as exc:
            self.audit.refuse(site, "peer_uid")
            exc.response["reason"] = "peer_uid"
            raise

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
            raise self.refused("service", "proof_mismatch", hello)
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
        self.peer_gone(peer)  # #11239: grants wait RECONNECT_GRACE for the worker.

    async def bootstrap(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self.writers.add(writer)
        try:
            self.peer_ok(writer, "enrollment")
            try:
                await asyncio.wait_for(
                    asyncio.to_thread(
                        verify_chrome_host, writer.get_extra_info("socket"), self.state
                    ),
                    7,
                )
            except (PermissionError, OSError, TimeoutError) as exc:
                reason = (
                    exc.code
                    if isinstance(exc, HostRefused)
                    else "host_timeout"
                    if isinstance(exc, TimeoutError)
                    else "host_not_native_host"
                )
                raise self.refused("enrollment", reason) from None
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
            reply: Frame = {
                "ok": False,
                "error": str(exc) if str(exc) in safe else "EnrollmentFailed",
            }
            if isinstance(exc, Refusal) and str(exc) in RELAYED:  # #11239
                reply = exc.response
            elif isinstance(exc, Refusal) and "reason" in exc.response:
                reply["reason"] = exc.response["reason"]  # ruling Dqw: the popup shows it
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(writer, reply)
        except EOFError:
            pass
        finally:
            self.writers.discard(writer)
            writer.close()

    async def enrollment_request(self, request: Frame) -> Frame:
        assert self.db is not None
        """The extension's ops on enrollment.sock: pair, unpair, connect, list and revoke.

        #11239: the workflow enroll ops (resolve, status, enroll, replace,
        unenroll) and code mint are gone; each answers ``InvalidRequest``.
        Test: `test_removed_enroll_and_code_ops_are_refused`.
        """
        op = string(request, "op")
        if op in ("pair", "unpair"):
            return await self.pairing_request(request)
        if op in POPUP_OPS:
            return await self.popup_grants(request)
        raise Refusal("InvalidRequest")

    async def pairing_request(self, request: Frame) -> Frame:
        """The extension's ``pair`` and ``unpair``; contract "pair".

        Test: `test_pair_is_idempotent_and_seals_exactly_one_key`,
        `test_unpair_ends_grants_and_a_removed_key_cannot_mint`.
        """
        assert self.db is not None
        op, profile = request["op"], request.get("profile")
        try:
            if op == "pair":
                reply, replaced = pairing.pair_profile(self.db, self.state, request)
                if replaced is not None:  # ruling Dqw 2026-11-16: newest pairing wins
                    await self.detach(replaced)
                    self.audit.best_effort(
                        "pair_replaced", profile=reply["profile"], replaced=replaced,
                        outcome="ok",
                    )  # fmt: skip
            else:
                fields = pairing.triple(request)
                if fields is None or set(request) != {"op", *pairing.CREDENTIAL}:
                    raise Refusal("InvalidEnrollment")
                if not pairing.unpair_profile(self.db, self.state, request, fields[0]):
                    raise self.refused("unpair", "unpair_bad_credential", request)
                await self.detach(fields[0])
                reply = {"ok": True, "profile": fields[0]}
        except ValueError as exc:
            self.audit.best_effort(
                str(op), profile=pairing.profile_id(profile), outcome=str(exc)
            )
            raise
        self.audit.best_effort(str(op), profile=reply["profile"], outcome="ok")
        return reply

    async def pairing_operator(self, request: Frame) -> Frame:
        """Operator ``pairings`` and credential-free ``unpair``; contract "Operator ops"."""
        assert self.db is not None
        if set(request) == {"op"} and request["op"] == "pairings":
            return {"ok": True, "pairings": pairing.pairings(self.db)}
        profile = pairing.profile_id(request.get("profile"))
        if set(request) != {"op", "profile"} or profile is None:
            raise Refusal("InvalidRequest")
        if not pairing.unpair_profile(self.db, self.state, None, profile):
            return {"ok": False, "error": "UnknownProfile"}
        await self.detach(profile)
        self.audit.best_effort("unpair", profile=profile, outcome="ok")
        return {"ok": True, "profile": profile}

    async def revoke(self, workflow: str, request: Frame | None = None) -> None:
        assert self.db is not None
        profile = remove(self.db, self.state, workflow, request)
        if profile is not None:
            await self.detach(profile)
        finish_removals(self.db, self.state)

    async def detach(self, profile: str) -> None:
        await self.end_profile(profile)  # #11239
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
            self.peer_ok(writer, "operator")
            req = await asyncio.wait_for(read_frame(reader), 5)
            if req.get("op") in OPERATOR_OPS:  # #11239
                await write_frame(writer, await self.grant_operator(req))
                return
            if req.get("op") in ("pairings", "unpair"):
                await write_frame(writer, await self.pairing_operator(req))
                return
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
            self.peer_ok(writer, "service")
            row, incarnation = await asyncio.wait_for(self.pair(reader, writer), 5)
            # Revocation may occur while the browser is computing its proof.
            current = self.route({"profile": row["profile"]}, True)
            if (
                current["browser_key"] != row["browser_key"]
                or current["ticket"] != row["ticket"]
            ):
                raise self.refused("service", "key_rotated", dict(row))
            name = row["profile"]
            if name in self.peers:
                raise Refusal("DuplicateProfile")
            peer = Peer(writer, row, incarnation)
            self.peers[name] = peer
            self.peer_back(peer)  # #11239: rebind grants across a worker restart.
            # #11239: ``live`` lets the worker prune grants whose end it missed.
            ack = {"ok": True, "generation": peer.generation, "live": self.live(name)}
            # #11363: the worker reloads itself when it runs other assets than these.
            stamp = assets.published(self.state)
            if stamp is not None:
                ack["assets"] = stamp
            await write_frame(writer, ack)
            while True:
                reply = await read_frame(reader)
                pending = peer.pending.get(string(reply, "id"))
                if pending is None or pending.done():
                    raise Refusal("UnexpectedReply")
                pending.set_result(reply)
        except Refusal as exc:
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(writer, relayed(exc))
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
            self.peer_ok(writer, "agent")
            hello = await asyncio.wait_for(read_frame(reader), 5)
            if hello.get("op") == "profiles":  # #11239: list, then close
                if set(hello) != {"op"}:
                    raise Refusal("InvalidRequest")
                await self.expire(codes.clock())
                await write_frame(writer, {"ok": True, "profiles": self.profiles()})
                return
            if hello.get("op") != "attach":
                # #11239: the workflow ticket `open`, like redeem, is removed; a leftover
                # routes row must not open a grant-free, unaudited session.
                raise Refusal("InvalidRequest")
            session = await self.attach(hello)  # no code; Connect made the grant
            ack = self.attached(session)
            self.sessions[writer] = session
            await write_frame(writer, ack)
            while True:
                request = await read_frame(reader)
                try:
                    response = await self.dispatch(session, request)
                except Refusal as exc:
                    response = exc.response
                await write_frame(writer, response)
                if response.get("error") == "GrantEnded":  # #11239
                    break
        except Refusal as exc:
            with contextlib.suppress(ConnectionError, TimeoutError):
                await write_frame(writer, relayed(exc))
        except (EOFError, OSError, ValueError, TimeoutError):
            pass
        finally:
            self.sessions.pop(writer, None)
            if session is not None:
                await self.close_session(session)  # #11239
                for key, claim in list(self.claims.items()):
                    if claim.owner == session.owner:
                        del self.claims[key]
                        await self.clear(claim)
            self.writers.discard(writer)
            writer.close()


def relayed(exc: Refusal) -> Frame:
    """A refusal for service.sock or agent.sock: reason codes go to the popup only."""
    return {k: v for k, v in exc.response.items() if k != "reason"}


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

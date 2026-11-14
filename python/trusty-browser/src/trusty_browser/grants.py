"""Mint, redeem, scope and revoke one-use activation codes for Broker; contract in docs/activation-codes.md."""

from __future__ import annotations

import asyncio
import math
import secrets
import time
from pathlib import Path

from trusty_browser import codes
from trusty_browser.audit import Audit
from trusty_browser.codes import CODE_TTL, GRANT_IDLE, Grant, Lockout
from trusty_browser.enrollment import FIELDS, original_fields, registration_id
from trusty_browser.leases import Leases
from trusty_browser.protocol import Claim, Frame, Peer, Refusal, Session, string

MINT_ERRORS = {
    "InvalidMint",
    "NotEnrolled",
    "BrowserDisconnected",
    "GrantLimit",
    "AuditUnavailable",
}
OPERATOR_OPS = {"grants", "revoke_grant", "revoke_grants"}


def gone(grant: Grant) -> Refusal:
    return Refusal("GrantEnded", reason=grant.reason or "revoked")


class Grants(Leases):
    """Activation-code methods mixed into ``service.Broker``; tests in tests/test_activation_codes.py."""

    state: Path
    sessions: dict[asyncio.StreamWriter, Session]

    def init_grants(self) -> None:
        self.grants: dict[str, Grant] = {}
        self.codes: dict[str, Grant] = {}
        self.spent: dict[str, tuple[str, float]] = {}
        self.lockout = Lockout()
        self.audit = Audit(self.state)

    def purge(self, now: float) -> None:
        for digest, grant in list(self.codes.items()):
            if grant.state == "code" and grant.deadline <= now:
                del self.codes[digest]
                self.grants.pop(grant.ident, None)
                self.spent[digest] = ("expired", grant.deadline + CODE_TTL)
        for digest, (_, forget) in list(self.spent.items()):
            if forget <= now:
                del self.spent[digest]

    def check_caps(
        self, generation: str | None, tab: int | None, me: Grant | None
    ) -> None:
        live = [
            g
            for g in self.grants.values()
            if g is not me and g.state in ("active", "pending")
        ]
        if (
            tab is not None
            and sum(
                g.scope == "tab" and g.tab == tab and g.generation == generation
                for g in live
            )
            >= codes.MAX_GRANTS_PER_TAB
        ):
            raise Refusal("GrantLimit", limit="tab")
        if len(live) >= codes.MAX_GRANTS:
            raise Refusal("GrantLimit", limit="total")

    def record(self, event: str, grant: Grant | None, **fields: object) -> None:
        """Fail-closed audit line for mint, redeem and admitted operations."""
        try:
            self.audit.write(event, **(grant.fields() if grant else {}), **fields)
        except OSError:
            raise Refusal("AuditUnavailable") from None

    def mint(self, request: Frame) -> Frame:
        scope, tab = request.get("scope"), request.get("tab")
        expected = {"op", "scope", *FIELDS} | ({"tab"} if scope == "tab" else set())
        if (
            set(request) != expected
            or scope not in ("profile", "tab")
            or scope == "tab"
            and (type(tab) is not int or tab < 0)
            or not all(isinstance(request[k], str) and request[k] for k in FIELDS)
        ):
            raise Refusal("InvalidMint")
        assert self.db is not None
        row = self.db.execute(
            "SELECT * FROM routes WHERE workflow=? AND profile=?",
            (request["workflow"], request["profile"]),
        ).fetchone()
        if row is None or not secrets.compare_digest(
            registration_id(original_fields(row, self.state)), registration_id(request)
        ):
            raise Refusal("NotEnrolled")
        peer = self.peers.get(str(row["profile"]))
        if scope == "tab" and peer is None:
            raise Refusal("BrowserDisconnected")
        generation = peer.generation if scope == "tab" and peer else None
        self.purge(codes.clock())
        self.check_caps(generation, tab if scope == "tab" else None, None)
        code = codes.new_code()
        digest = codes.code_digest(code)
        assert digest is not None
        grant = Grant(
            "g-" + secrets.token_hex(6),
            str(scope),
            row,
            tab if scope == "tab" else None,
            generation,
            digest,
            codes.clock() + CODE_TTL,
        )
        self.record("mint", grant, outcome="ok")
        self.codes[digest] = grant
        self.grants[grant.ident] = grant
        reply: Frame = {"ok": True, "code": code, **grant.fields()}
        del reply["origin"]
        if grant.tab is None:
            del reply["tab"]
        return {**reply, "expires_at": time.time() + CODE_TTL}

    def refuse(self, kind: str, grant: Grant | None, now: float) -> Refusal:
        self.audit.best_effort(
            "redeem", **(grant.fields() if grant else {}), outcome=kind
        )
        length = self.lockout.fail(now)
        if length is not None:
            self.audit.best_effort("lock", outcome=f"{int(length)}s")
        return Refusal("CodeRefused")

    async def redeem(self, hello: Frame) -> Session:
        if set(hello) != {"op", "code"}:
            raise Refusal("InvalidRequest")
        code, now = string(hello, "code"), codes.clock()
        if self.lockout.remaining(now) > 0:
            self.audit.best_effort("redeem", outcome="locked")
            raise Refusal(
                "RedeemLocked", retry_after_s=math.ceil(self.lockout.remaining(now))
            )
        self.purge(now)
        digest = codes.code_digest(code)
        grant = self.codes.get(digest) if digest else None
        if grant is None or grant.state != "code":
            if grant is not None:
                kind = "contended"
            else:
                kind = self.spent[digest][0] if digest in self.spent else "wrong"
            raise self.refuse(kind, grant, now)
        grant.state = "pending"
        try:
            peer = await self.bind(grant)
            if grant.state != "pending":
                raise self.refuse("revoked", grant, now)
            self.check_caps(grant.generation, grant.tab, grant)
            self.record("redeem", grant, outcome="ok")
        except Refusal as exc:
            if grant.state == "pending" and str(exc) == "TabClosed":
                self.consume(grant)
                self.grants.pop(grant.ident, None)
                grant.state, grant.reason = "ended", "tab_closed"
            elif grant.state == "pending":
                grant.state = "code"
            if str(exc) not in {"CodeRefused", "AuditUnavailable"}:
                outcome = {
                    "GrantLimit": "limit",
                    "TabClosed": "tab_closed",
                    "BrowserDisconnected": "browser_disconnected",
                }
                self.audit.best_effort(
                    "redeem", **grant.fields(), outcome=outcome.get(str(exc), str(exc))
                )
            raise
        except BaseException:
            if grant.state == "pending":
                grant.state = "code"
            raise
        self.consume(grant)
        grant.state, grant.generation = "active", peer.generation
        grant.session = Session(grant.row, grant=grant)
        grant.last_used = codes.clock()
        self.lockout.succeed()
        return grant.session

    def redeemed(self, session: Session) -> Frame:
        grant = session.grant
        assert grant is not None
        reply: Frame = {"ok": True, **grant.fields(), "idle_timeout_s": int(GRANT_IDLE)}
        del reply["origin"]
        if grant.tab is None:
            del reply["tab"]
        return reply

    def consume(self, grant: Grant) -> None:
        self.codes.pop(grant.digest, None)
        self.spent[grant.digest] = ("replay", grant.deadline + CODE_TTL)

    async def bind(self, grant: Grant) -> Peer:
        """Find the live peer for a redeem; a tab grant must still see its tab."""
        peer = self.peers.get(str(grant.row["profile"]))
        if grant.scope == "profile":
            if peer is None:
                raise Refusal("BrowserDisconnected")
            return peer
        if peer is None or peer.generation != grant.generation:
            raise Refusal("TabClosed")
        try:
            obs = await peer.command("inspect")
        except Refusal as exc:
            if str(exc) == "TabNotFound":
                raise Refusal("TabClosed") from None
            raise
        if (
            obs["tab"] != grant.tab
            or self.peers.get(str(grant.row["profile"])) is not peer
        ):
            raise Refusal("TabClosed")
        return peer

    def retire(self, grant: Grant, reason: str) -> list[Claim]:
        """End a grant synchronously; return its claims for indicator clearing."""
        if grant.state == "ended":
            return []
        was = grant.state
        grant.state, grant.reason = "ended", reason
        self.grants.pop(grant.ident, None)
        if was in ("code", "pending"):
            self.codes.pop(grant.digest, None)
            self.spent[grant.digest] = ("revoked", grant.deadline + CODE_TTL)
        self.audit.best_effort("grant_end", **grant.fields(), outcome=reason)
        if grant.session is None:
            return []
        owner = grant.session.owner
        old = [(k, c) for k, c in self.claims.items() if c.owner == owner]
        for key, _ in old:
            del self.claims[key]
        return [c for _, c in old]

    async def end(self, grant: Grant, reason: str) -> None:
        await asyncio.gather(*(self.clear(c) for c in self.retire(grant, reason)))

    def peer_gone(self, peer: Peer) -> None:
        for grant in list(self.grants.values()):
            if grant.state == "active" and grant.generation == peer.generation:
                self.retire(grant, "browser_gone")

    async def end_profile(self, profile: str) -> None:
        for grant in list(self.grants.values()):
            if grant.row["profile"] == profile:
                await self.end(grant, "unenrolled")

    async def close_session(self, session: Session) -> None:
        if session.grant is not None and session.grant.state == "active":
            await self.end(session.grant, "closed")

    async def check_live(self, grant: Grant) -> None:
        assert self.db is not None
        profile = str(grant.row["profile"])
        peer = self.peers.get(profile)
        route = self.db.execute(
            "SELECT ticket FROM routes WHERE profile=?", (profile,)
        ).fetchone()
        if grant.state != "active":
            return
        if codes.clock() - grant.last_used >= GRANT_IDLE:
            await self.end(grant, "idle")
        elif peer is None or peer.generation != grant.generation:
            await self.end(grant, "browser_gone")
        elif route is None or route["ticket"] != grant.row["ticket"]:
            await self.end(grant, "unenrolled")

    def admit_tab(self, session: Session, peer: Peer, tab: int) -> None:
        grant = session.grant
        if grant is None:
            return
        if (
            peer.generation != grant.generation
            or grant.scope == "tab"
            and tab != grant.tab
        ):
            raise Refusal("OutOfScope")

    async def dispatch(self, session: Session, request: Frame) -> Frame:
        grant = session.grant
        if grant is None:
            return await super().dispatch(session, request)
        await self.check_live(grant)
        if grant.state != "active":
            raise gone(grant)
        op = request.get("op")
        name = op if op in ("claim", "release", "click", "snapshot") else "invalid"
        self.record("operation", grant, op=name, outcome="admitted")
        grant.last_used = codes.clock()
        try:
            result = await super().dispatch(session, request)
        except Refusal as exc:
            if grant.scope == "tab" and str(exc) == "TabNotFound":
                await self.end(grant, "tab_closed")
            self.audit.best_effort(
                "operation", **grant.fields(), op=name, outcome=str(exc)
            )
            if grant.state != "active" and str(exc) != "OutcomeUnknown":
                raise gone(grant) from None
            raise
        if grant.state != "active":
            self.audit.best_effort(
                "operation", **grant.fields(), op=name, outcome="grant_ended"
            )
            if name != "click":
                raise gone(grant)
            result = {"ok": False, "error": "OutcomeUnknown"}
            session.journal[str(request["request_id"])] = (request, result)
            return result
        grant.last_used = codes.clock()
        try:
            self.audit.write("operation", **grant.fields(), op=name, outcome="ok")
        except OSError:
            await self.end(grant, "audit")
        return result

    async def grant_operator(self, request: Frame) -> Frame:
        op = request.get("op")
        self.purge(codes.clock())
        if op == "grants" and set(request) == {"op"}:
            now = codes.clock()
            return {"ok": True, "grants": [g.entry(now) for g in self.grants.values()]}
        if op == "revoke_grants" and set(request) == {"op"}:
            targets = list(self.grants.values())
        elif op == "revoke_grant" and set(request) == {"op", "grant"}:
            grant = self.grants.get(string(request, "grant"))
            if grant is None:
                return {"ok": False, "error": "UnknownGrant"}
            targets = [grant]
        else:
            raise Refusal("InvalidRequest")
        for grant in targets:
            self.audit.best_effort("revoke", **grant.fields(), outcome="ok")
            await self.end(grant, "revoked")
        return {"ok": True, "revoked": len(targets)}

"""Tab leases for Broker: claim, release, and leased browser actions with indicators."""

# #11185: split from service.py for the 500-line cap

from __future__ import annotations

import contextlib
import secrets
import sqlite3
import time

from trusty_browser.protocol import Claim, Frame, Peer, Refusal, Session, string

TTL = 60.0


class Leases:
    """Lease methods mixed into ``service.Broker``; tests live in tests/test_service.py."""

    db: sqlite3.Connection | None
    peers: dict[str, Peer]
    claims: dict[tuple[str, int], Claim]

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

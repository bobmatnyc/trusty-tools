"""Mint, redeem, scope and revoke one-use activation codes for Broker; contract in docs/activation-codes.md."""

from __future__ import annotations

import asyncio
import contextlib
import math
import secrets
import sqlite3
import sys
import time
from pathlib import Path

from trusty_browser import codes, pairing
from trusty_browser.audit import Audit
from trusty_browser.codes import CODE_TTL, GRANT_IDLE, Grant, Lockout
from trusty_browser.enrollment import FIELDS, original_fields, registration_id
from trusty_browser.leases import Leases
from trusty_browser.protocol import Claim, Frame, Peer, Refusal, Session, string

# Refusals relayed whole on enrollment.sock (#11239); anything else is EnrollmentFailed.
RELAYED = {
    "InvalidMint",
    "InvalidRequest",
    "NotEnrolled",
    "BrowserDisconnected",
    "GrantLimit",
    "AuditUnavailable",
    "UnknownGrant",
}
OPERATOR_OPS = {"grants", "revoke_grant", "revoke_grants"}
POPUP_OPS = OPERATOR_OPS


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
        self.tasks: set[asyncio.Future[None]] = set()
        self.sweeper: asyncio.TimerHandle | None = None
        self.halted = False

    def purge(self, now: float) -> None:
        for digest, grant in list(self.codes.items()):
            if grant.state == "code" and grant.deadline <= now:
                del self.codes[digest]
                self.grants.pop(grant.ident, None)
                self.spent[digest] = ("expired", grant.deadline + CODE_TTL)
                grant.state, grant.reason = "ended", "expired"
                self.notify(grant, "expired")
        for digest, (_, forget) in list(self.spent.items()):
            if forget <= now:
                del self.spent[digest]

    async def expire(self, now: float) -> None:
        """Purge stale codes and end every grant idle for ``GRANT_IDLE`` or longer.

        Why: an idle grant otherwise holds its tab slot and stays listed until
        its own agent calls again (#11239). What: runs before every cap check
        and grant listing.
        Test: `test_idle_profile_grants_leave_the_grant_list`,
        `test_idle_tab_grant_releases_the_tab_cap_even_without_audit`.
        """
        self.purge(now)
        for grant in list(self.grants.values()):
            if grant.state == "active" and now - grant.last_used >= GRANT_IDLE:
                await self.end(grant, "idle")

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

    def record(self, event: str, grant: Grant | None, **fields: object) -> None:
        """Fail-closed audit line for mint, redeem and admitted operations."""
        try:
            self.audit.write(event, **(grant.fields() if grant else {}), **fields)
        except OSError:
            raise Refusal("AuditUnavailable") from None

    def minter(self, request: Frame) -> sqlite3.Row:
        """The row a mint frame proves: the credential triple, or phase 1's six fields."""
        assert self.db is not None
        if "workflow" not in request:
            fields = pairing.triple(request)
            if fields is None:
                raise Refusal("InvalidMint")
            row = pairing.credential_row(self.db, self.state, fields)
        else:
            if not all(isinstance(request[k], str) and request[k] for k in FIELDS):
                raise Refusal("InvalidMint")
            row = self.db.execute(
                "SELECT * FROM routes WHERE workflow=? AND profile=?",
                (request["workflow"], request["profile"]),
            ).fetchone()
            if row is not None and not secrets.compare_digest(
                registration_id(original_fields(row, self.state)),
                registration_id(request),
            ):
                row = None
        if row is None:
            raise Refusal("NotEnrolled")
        return row

    async def mint(self, request: Frame) -> Frame:
        """Mint one code for the credential's profile; contract "mint".

        Test: `test_mint_with_a_pairing_credential_succeeds_and_a_wrong_key_does_not`,
        `test_mint_is_refused_for_an_unenrolled_profile`.
        """
        scope, tab = request.get("scope"), request.get("tab")
        names = FIELDS if "workflow" in request else pairing.CREDENTIAL
        expected = {"op", "scope", *names} | ({"tab"} if scope == "tab" else set())
        if (
            set(request) != expected
            or scope not in ("profile", "tab")
            or scope == "tab"
            and (type(tab) is not int or tab < 0)
        ):
            raise Refusal("InvalidMint")
        row = self.minter(request)
        peer = self.peers.get(str(row["profile"]))
        if scope == "tab" and peer is None:
            raise Refusal("BrowserDisconnected")
        generation = peer.generation if scope == "tab" and peer else None
        await self.expire(codes.clock())
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
        self.arm()
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
        await self.expire(now)
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
                self.notify(grant, "tab_closed")
            elif grant.state == "pending":
                grant.state = "code"
            self.arm()  # a sweep that fired while pending skipped this code
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
            self.arm()
            raise
        self.consume(grant)
        grant.state, grant.generation = "active", peer.generation
        grant.session = Session(grant.row, grant=grant)
        grant.last_used = codes.clock()
        self.lockout.succeed()
        self.arm()
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
            # #11239: probe the granted tab by its id, never the active tab.
            await peer.page("page_tab", grant.ident, tab=grant.tab)
        except Refusal as exc:
            if str(exc) in ("TabNotFound", "UnknownGrant"):
                raise Refusal("TabClosed") from None
            raise
        if self.peers.get(str(grant.row["profile"])) is not peer:
            raise Refusal("TabClosed")
        return peer

    def live(self, profile: str) -> list[str]:
        """Ids of the profile's unredeemed codes and live grants."""
        return [g.ident for g in self.grants.values() if g.row["profile"] == profile]

    def notify(self, grant: Grant, reason: str) -> None:
        """Tell the profile's worker, best effort and without waiting, that a grant ended.

        Why: the worker drops its row, and all-https access with the last one
        (#11239, "All-https access"). What: ``last`` is true when the profile
        holds no other code or grant.
        Test: `test_grant_end_reaches_the_worker_on_every_end_route_with_last`.
        """
        profile = str(grant.row["profile"])
        peer = self.peers.get(profile)
        if peer is None:
            return
        last = not self.live(profile)

        async def send() -> None:
            with contextlib.suppress(Refusal):
                await peer.page("grant_end", grant.ident, reason=reason, last=last)

        task = asyncio.ensure_future(send())
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    def arm(self) -> None:
        """Keep one loop callback at the earliest code or idle deadline (no thread).

        Why: idle and code expiry must reach the worker without an agent call
        (#11239, "All-https access"). What: called after mint, redeem (either
        outcome) and every sweep; a no-op once ``halt`` ran.
        Test: `test_a_refused_redeem_rearms_the_code_deadline`,
        `test_the_deadline_callback_never_runs_after_close`.
        """
        if self.sweeper is not None:
            self.sweeper.cancel()
            self.sweeper = None
        if self.halted:
            return
        due = [
            g.deadline if g.state == "code" else g.last_used + GRANT_IDLE
            for g in self.grants.values()
            if g.state in ("code", "active")
        ]
        if not due:
            return
        delay = max(0.0, min(due) - codes.clock())
        loop = asyncio.get_running_loop()
        self.sweeper = loop.call_later(delay, self.sweep)

    def sweep(self) -> None:
        """The deadline callback: expire what is due in a task, then re-arm.

        An exception is logged and the timer still re-arms; every due item
        leaves ``self.grants`` before anything can raise, so it cannot spin.
        Test: `test_a_sweep_that_raises_still_rearms_the_deadline`.
        """
        if self.halted:
            return

        async def run() -> None:
            try:
                await self.expire(codes.clock())
            except Exception as exc:  # the timer must survive any failure
                sys.stderr.write(f"trusty-browser: grant sweep failed: {exc!r}\n")
            finally:
                self.arm()

        task = asyncio.ensure_future(run())
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

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
        self.notify(grant, reason)
        if grant.session is None:
            return []
        owner = grant.session.owner
        old = [(k, c) for k, c in self.claims.items() if c.owner == owner]
        for key, _ in old:
            del self.claims[key]
        return [c for _, c in old]

    async def end(self, grant: Grant, reason: str) -> None:
        """End a grant, then clear its indicators; a failed clear never stops a caller's loop.

        Test: `test_a_failing_indicator_clear_cannot_stop_the_idle_sweep`.
        """
        claims = self.retire(grant, reason)
        results = await asyncio.gather(
            *(self.clear(c) for c in claims), return_exceptions=True
        )
        for result in results:
            if isinstance(result, Exception):
                sys.stderr.write(
                    f"trusty-browser: indicator clear failed: {result!r}\n"
                )
            elif isinstance(result, BaseException):
                raise result

    def halt(self) -> None:
        """Stop the deadline callback and its tasks for good; ``Broker.close`` calls it."""
        self.halted = True
        if self.sweeper is not None:
            self.sweeper.cancel()
            self.sweeper = None
        for task in tuple(self.tasks):
            task.cancel()
        self.tasks.clear()

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
        # #11239: the enrollment or pairing that minted the grant must still stand.
        route = pairing.profile_row(self.db, profile)
        if grant.state != "active":
            return
        if codes.clock() - grant.last_used >= GRANT_IDLE:
            await self.end(grant, "idle")
        elif peer is None or peer.generation != grant.generation:
            await self.end(grant, "browser_gone")
        elif route is None or any(
            route[k] != grant.row[k] for k in ("ticket", "browser_key")
        ):
            await self.end(grant, "unenrolled")

    async def grant_operator(self, request: Frame, profile: str | None = None) -> Frame:
        """List or revoke codes and grants; ``profile`` narrows to one profile (popup).

        Test: `test_revoke_one_and_all_refuse_the_next_call`,
        `test_popup_list_and_revoke_see_only_the_credential_profile`.
        """
        op = request.get("op")
        await self.expire(codes.clock())
        mine = [
            g
            for g in self.grants.values()
            if profile is None or g.row["profile"] == profile
        ]
        if op == "grants" and set(request) == {"op"}:
            now = codes.clock()
            return {"ok": True, "grants": [g.entry(now) for g in mine]}
        if op == "revoke_grants" and set(request) == {"op"}:
            targets = mine
        elif op == "revoke_grant" and set(request) == {"op", "grant"}:
            grant = self.grants.get(string(request, "grant"))
            if grant is None or grant not in mine:
                return {"ok": False, "error": "UnknownGrant"}
            targets = [grant]
        else:
            raise Refusal("InvalidRequest")
        for grant in targets:
            self.audit.best_effort("revoke", **grant.fields(), outcome="ok")
            await self.end(grant, "revoked")
        return {"ok": True, "revoked": len(targets)}

    async def popup_grants(self, request: Frame) -> Frame:
        """The popup's list and revoke on enrollment.sock, inside one credential's profile."""
        assert self.db is not None
        fields = pairing.triple(request)
        if fields is None or request.get("op") not in POPUP_OPS:
            raise Refusal("InvalidRequest")
        row = pairing.credential_row(self.db, self.state, fields)
        if row is None:
            raise Refusal("NotEnrolled")
        rest = {k: v for k, v in request.items() if k not in pairing.CREDENTIAL}
        return await self.grant_operator(rest, str(row["profile"]))

"""Connect, attach, list and revoke standing profile grants for Broker; contract in docs/activation-codes.md."""

from __future__ import annotations

import asyncio
import contextlib
import secrets
import sqlite3
import sys
from pathlib import Path

from trusty_browser import codes, pairing
from trusty_browser.audit import Audit
from trusty_browser.codes import RECONNECT_GRACE, Grant
from trusty_browser.leases import Leases
from trusty_browser.protocol import Claim, Frame, Peer, Refusal, Session, string

# Refusals relayed whole on enrollment.sock (#11239); anything else is EnrollmentFailed.
RELAYED = {
    "InvalidRequest",
    "NotEnrolled",
    "BrowserDisconnected",
    "AuditUnavailable",
    "UnknownGrant",
}
OPERATOR_OPS = {"grants", "revoke_grant", "revoke_grants"}
# #11239: Connect is the popup's only way to create a grant; Disconnect is revoke_grants.
POPUP_OPS = OPERATOR_OPS | {"connect"}


def gone(grant: Grant) -> Refusal:
    return Refusal("GrantEnded", reason=grant.reason or "revoked")


class Grants(Leases):
    """Grant methods mixed into ``service.Broker``; tests in tests/test_activation_codes.py."""

    state: Path
    sessions: dict[asyncio.StreamWriter, Session]

    def init_grants(self) -> None:
        self.grants: dict[str, Grant] = {}
        self.audit = Audit(self.state)
        self.tasks: set[asyncio.Future[None]] = set()
        self.sweeper: asyncio.TimerHandle | None = None
        self.halted = False

    async def expire(self, now: float) -> None:
        """End every grant whose reconnect grace ran out.

        Why: a grant lasts until its browser closes (#11239 owner ruling), so
        idleness never ends one; a worker that does not return within
        ``RECONNECT_GRACE`` means the browser is gone. What: runs before every
        grant listing and attach, and from the deadline sweep.
        Test: `test_idle_profile_grants_stay_listed_past_the_old_timeout`,
        `test_a_grant_whose_worker_never_returns_ends_after_the_grace`.
        """
        for grant in list(self.grants.values()):
            if (
                grant.state == "active"
                and grant.grace_until is not None
                and grant.grace_until <= now
            ):
                await self.end(grant, "browser_gone")

    def record(self, event: str, grant: Grant | None, **fields: object) -> None:
        """Fail-closed audit line for connect, attach and admitted operations."""
        try:
            self.audit.write(event, **(grant.fields() if grant else {}), **fields)
        except OSError:
            raise Refusal("AuditUnavailable") from None

    def profile_grant(self, profile: str) -> Grant | None:
        for grant in self.grants.values():
            if grant.state == "active" and grant.row["profile"] == profile:
                return grant
        return None

    async def connect(self, row: sqlite3.Row) -> Frame:
        """Create the profile's standing grant; contract "connect".

        Why: one Connect click grants the whole profile to every same-user
        local agent (#11239 owner rulings 03:37Z, Dpj). What: needs the
        profile's paired browser, reuses a live grant, and writes the
        fail-closed ``connect`` audit line before the grant exists.
        Test: `test_connect_with_no_code_creates_a_standing_profile_grant`,
        `test_connect_whose_audit_fails_creates_no_grant`.
        """
        profile = str(row["profile"])
        peer = self.peers.get(profile)
        if peer is None:
            raise Refusal("BrowserDisconnected")
        await self.expire(codes.clock())
        grant = self.profile_grant(profile)
        if grant is None:
            grant = Grant(
                "g-" + secrets.token_hex(6),
                "profile",
                row,
                None,
                peer.generation,
                incarnation=peer.incarnation,
                last_used=codes.clock(),
            )
            self.record("connect", grant, outcome="ok")
            self.grants[grant.ident] = grant
        return {"ok": True, "grant": grant.ident, "scope": "profile", "profile": profile}

    def connected(self) -> list[Grant]:
        return [g for g in self.grants.values() if g.state == "active"]

    def profiles(self) -> list[dict[str, object]]:
        """The connected profiles an agent may attach to, for ``browser_profiles``."""
        return [
            {"profile": str(g.row["profile"]), "email": str(g.row["email"]), "grant": g.ident}
            for g in self.connected()
        ]

    async def attach(self, hello: Frame) -> Session:
        """Attach an agent session to a connected profile's grant; contract "attach".

        Why: the agent needs no code; choosing among several connected
        profiles must be explicit (#11239). What: ``profile`` names one;
        without it exactly one connected profile is taken, none answers
        ``NotConnected`` and several answer ``ProfileChoiceRequired`` with the
        list. Writes the fail-closed ``attach`` audit line.
        Test: `test_attach_needs_no_code_and_several_profiles_need_a_choice`.
        """
        if set(hello) - {"op", "profile"}:
            raise Refusal("InvalidRequest")
        await self.expire(codes.clock())
        live = self.connected()
        if "profile" in hello:
            wanted = string(hello, "profile")
            live = [g for g in live if g.row["profile"] == wanted]
        if not live:
            raise Refusal("NotConnected")
        if len(live) > 1:
            raise Refusal("ProfileChoiceRequired", profiles=self.profiles())
        grant = live[0]
        self.record("attach", grant, outcome="ok")
        session = Session(grant.row, grant=grant)
        grant.sessions.append(session)
        return session

    def attached(self, session: Session) -> Frame:
        grant = session.grant
        assert grant is not None
        return {
            "ok": True,
            "grant": grant.ident,
            "scope": grant.scope,
            "profile": str(grant.row["profile"]),
            "email": str(grant.row["email"]),
        }

    def live(self, profile: str) -> list[str]:
        """Ids of the profile's live grants."""
        return [g.ident for g in self.grants.values() if g.row["profile"] == profile]

    def notify(self, grant: Grant, reason: str) -> None:
        """Tell the profile's worker, best effort and without waiting, that a grant ended.

        Why: the worker drops its row, and all-https access with the last one
        (#11239, "All-https access"). What: ``last`` is true when the profile
        holds no other grant.
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
        """Keep one loop callback at the earliest reconnect-grace deadline (no thread).

        Why: a lost browser must reach the worker without an agent call
        (#11239, "All-https access"). What: called after a peer loss or return
        and every sweep; an active grant with a connected browser has no
        deadline; a no-op once ``halt`` ran.
        Test: `test_a_grant_whose_worker_never_returns_ends_after_the_grace`,
        `test_the_deadline_callback_never_runs_after_close`.
        """
        if self.sweeper is not None:
            self.sweeper.cancel()
            self.sweeper = None
        if self.halted:
            return
        due = [
            g.grace_until
            for g in self.grants.values()
            if g.state == "active" and g.grace_until is not None
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
        grant.state, grant.reason = "ended", reason
        self.grants.pop(grant.ident, None)
        # #11239: the grant ends before any audit write, so a failed line never keeps it.
        self.audit.best_effort("grant_end", **grant.fields(), outcome=reason)
        self.notify(grant, reason)
        owners = {s.owner for s in grant.sessions}
        old = [(k, c) for k, c in self.claims.items() if c.owner in owners]
        for key, _ in old:
            del self.claims[key]
        return [c for _, c in old]

    def retire_soon(self, grant: Grant, reason: str) -> None:
        """Retire from synchronous code and clear its claims in a task, so no stale lease outlives it.

        Test: `test_a_browser_restart_ends_the_grant_at_reconnect`.
        """
        claims = self.retire(grant, reason)
        if not claims:
            return

        async def run() -> None:
            await asyncio.gather(*(self.clear(c) for c in claims), return_exceptions=True)

        task = asyncio.ensure_future(run())
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def end(self, grant: Grant, reason: str) -> None:
        """End a grant, then clear its indicators; a failed clear never stops a caller's loop.

        Test: `test_a_failing_indicator_clear_cannot_stop_a_revoke_all`.
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
        """Start the reconnect grace for the lost peer's active grants.

        Why: Chrome stops an idle MV3 service worker, which drops service.sock
        while the browser stays open; the grant must outlive that (#11239
        owner ruling). What: the grant stays active with ``grace_until`` set;
        ``peer_back`` rebinds it or the sweep ends it ``browser_gone``. A grant
        with no incarnation ends at once (fail closed).
        Test: `test_a_grant_survives_a_worker_reconnect_within_the_grace`,
        `test_a_grant_without_an_incarnation_does_not_persist`.
        """
        until = codes.clock() + RECONNECT_GRACE
        for grant in list(self.grants.values()):
            if grant.state != "active" or grant.generation != peer.generation:
                continue
            if grant.incarnation:
                grant.grace_until = until
            else:  # #11239: no incarnation to match on return, so no grace.
                self.retire_soon(grant, "browser_gone")
        self.arm()

    def peer_back(self, peer: Peer) -> None:
        """Rebind the profile's grants to a new peer of the same browser incarnation.

        Why: ``incarnation`` lives in ``chrome.storage.session``, which survives
        a service-worker restart and is cleared when the browser closes. What:
        same incarnation moves active grants to the new generation and audits
        ``grant_reuse``; a new or missing incarnation, or an audit failure,
        ends them ``browser_gone``, so after a browser restart the popup
        offers Connect again (#11239 ruling Dpj). Runs before the hello
        acknowledgement, so its ``live`` list is current.
        Test: `test_a_grant_survives_a_worker_reconnect_within_the_grace`,
        `test_a_browser_restart_ends_the_grant_at_reconnect`,
        `test_a_reconnect_after_the_grace_ends_the_grant_without_a_sweep`.
        """
        profile = str(peer.row["profile"])
        for grant in list(self.grants.values()):
            if (
                grant.row["profile"] != profile
                or grant.state != "active"
                or grant.generation in (None, peer.generation)
            ):
                continue
            if (  # #11239: a lapsed grace ends the grant even before the sweep runs
                grant.state == "active"
                and grant.grace_until is not None
                and grant.grace_until <= codes.clock()
            ):
                self.retire_soon(grant, "browser_gone")
                continue
            # pair() refuses an empty incarnation (protocol.string); this guard
            # keeps an unset one from ever matching.
            if grant.incarnation and grant.incarnation == peer.incarnation:
                try:  # fail closed: a rebind the audit cannot record ends the grant
                    self.audit.write("grant_reuse", **grant.fields(), outcome="ok")
                except OSError:
                    self.retire_soon(grant, "browser_gone")
                    continue
                grant.generation, grant.grace_until = peer.generation, None
            else:
                self.retire_soon(grant, "browser_gone")
        self.arm()

    async def end_profile(self, profile: str) -> None:
        for grant in list(self.grants.values()):
            if grant.row["profile"] == profile:
                await self.end(grant, "unenrolled")

    async def close_session(self, session: Session) -> None:
        """Detach one agent; the profile grant stays for the next agent (#11239 ruling Dpj)."""
        grant = session.grant
        if grant is not None and session in grant.sessions:
            grant.sessions.remove(session)
            if grant.state == "active":
                self.audit.best_effort("detach", **grant.fields(), outcome="ok")

    async def check_live(self, grant: Grant) -> None:
        assert self.db is not None
        profile = str(grant.row["profile"])
        peer = self.peers.get(profile)
        # #11239: the enrollment or pairing that minted the grant must still stand.
        route = pairing.profile_row(self.db, profile)
        if grant.state != "active":
            return
        if peer is None or peer.generation != grant.generation:
            # #11239: inside the reconnect grace the grant waits for its worker.
            if grant.grace_until is not None and codes.clock() < grant.grace_until:
                raise Refusal("BrowserDisconnected")
            await self.end(grant, "browser_gone")
        elif route is None or any(
            route[k] != grant.row[k] for k in ("ticket", "browser_key")
        ):
            await self.end(grant, "unenrolled")

    async def grant_operator(self, request: Frame, profile: str | None = None) -> Frame:
        """List or revoke grants; ``profile`` narrows to one profile (popup).

        Why: Disconnect must end access at once and the full audit log must
        record it (#11239 ruling 03:52Z). What: a revoke ends each grant
        first, then writes the best-effort ``revoke`` line, so a failed audit
        write never keeps a grant alive.
        Test: `test_revoke_one_and_all_refuse_the_next_call`,
        `test_disconnect_whose_audit_fails_still_revokes_at_once`.
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
            await self.end(grant, "revoked")
            self.audit.best_effort("revoke", **grant.fields(), outcome="ok")
        return {"ok": True, "revoked": len(targets)}

    async def popup_grants(self, request: Frame) -> Frame:
        """The popup's connect, list and revoke on enrollment.sock, inside one credential's profile."""
        assert self.db is not None
        fields = pairing.triple(request)
        if fields is None or request.get("op") not in POPUP_OPS:
            raise Refusal("InvalidRequest")
        row = pairing.credential_row(self.db, self.state, fields)
        if row is None:
            raise Refusal("NotEnrolled")
        rest = {k: v for k, v in request.items() if k not in pairing.CREDENTIAL}
        if rest.get("op") == "connect":
            if set(rest) != {"op"}:
                raise Refusal("InvalidRequest")
            return await self.connect(row)
        return await self.grant_operator(rest, str(row["profile"]))

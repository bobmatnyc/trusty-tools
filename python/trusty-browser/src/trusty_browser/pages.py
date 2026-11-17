"""Code-session page operations for Broker; contract in docs/activation-codes.md.

The contract sections are "Operations on a code session", "Tab incarnation and
documentId" and "Per-operation checks, audit and grant fate". Tests live in
tests/test_page_ops.py.
"""

from __future__ import annotations

import hashlib
import secrets
import time
from urllib.parse import urlsplit

from trusty_browser import codes
from trusty_browser.codes import Grant
from trusty_browser.grants import Grants, gone
from trusty_browser.leases import TTL
from trusty_browser.page_fields import MODIFIERS, key_allowed, valid_files
from trusty_browser.page_query import KEEP_LEASE, clicked, controls, optional
from trusty_browser.protocol import (
    HISTORY,
    MUTATING,
    Claim,
    Frame,
    Peer,
    Refusal,
    Session,
    string,
)

OPS = {
    "tabs": {"op"},
    "select": {"op", "tab"},
    "page_read": {"op", "lease"},
    "page_click": {"op", "lease", "request_id", "ref"},
    "page_hover": {"op", "lease", "request_id", "ref"},
    "page_type": {"op", "lease", "request_id", "ref", "text"},
    "page_scroll": {"op", "lease", "request_id", "to", "ref", "wait_ms"},
    "page_press": {"op", "lease", "request_id", "ref", "key", "modifiers"},
    "page_select": {"op", "lease", "request_id", "ref", "value"},
    "page_drag": {"op", "lease", "request_id", "ref", "target"},
    "page_navigate": {"op", "lease", "request_id", "url"},
    "page_upload": {"op", "lease", "request_id", "ref", "files"},
    "page_downloads": {"op", "lease"},
    "page_find": {"op", "lease"},  # #11357: terms are optional fields
    "page_back": {"op", "lease", "request_id"},
    "page_forward": {"op", "lease", "request_id"},
    "page_reload": {"op", "lease", "request_id"},
    "release": {"op", "lease"},
    # #11293: focus the leased tab; profile grants only.
    "switch": {"op", "lease"},
}
TAB_KEYS = ("tab", "window", "index", "active", "access", "origin", "title")


def remembered(request: Frame) -> Frame:
    """The journal's copy of a request: file data becomes its SHA-256 (#11289).

    Why: 4096 journaled uploads of 512 KiB would hold gigabytes; a digest still
    tells a true retry from a ``Conflict``.
    """
    files = request.get("files")
    if not isinstance(files, list):
        return request
    digests = [
        {**f, "data": hashlib.sha256(str(f.get("data")).encode()).hexdigest()}
        if isinstance(f, dict)
        else f
        for f in files
    ]
    return {**request, "files": digests}


def floor_host(url: object) -> str | None:
    """The URL's host when it passes the URL floor, else ``None``."""
    if not isinstance(url, str) or not 0 < len(url) <= 8192:
        return None
    try:
        parts = urlsplit(url)
        parts.port  # noqa: B018 -- raises ValueError on a malformed port
    except ValueError:
        return None
    host = parts.hostname
    if parts.username is not None or parts.password is not None or not host:
        return None
    if parts.scheme == "https" or parts.scheme == "http" and host == "127.0.0.1":
        return host
    return None


def host(origin: object) -> str | None:
    return urlsplit(origin).hostname if isinstance(origin, str) else None


def shaped(value: object, types: dict[str, tuple[type, ...]]) -> Frame:
    """``value`` as a dict holding exactly ``types``' keys, else ``BrowserRefused``."""
    if not isinstance(value, dict) or set(value) != set(types):
        raise Refusal("BrowserRefused")
    for key, allowed in types.items():
        item = value[key]
        if type(item) not in allowed:
            raise Refusal("BrowserRefused")
    return value


OPTIONAL = (str, type(None))
PROBE = {"access": (bool,), "document": OPTIONAL, "origin": OPTIONAL, "title": OPTIONAL}
READ = {
    "document": (str,),
    "origin": (str,),
    "url": (str,),
    "title": (str,),
    "text": (str,),
    "elements": (list,),
    "truncated": (bool,),
}
FOUND = {
    "document": (str,),
    "origin": (str,),
    "matches": (list,),
    "truncated": (bool,),
}
DONE = {"document": (str,)}
# #11352: a page_type or page_press whose text a maxlength cut still succeeds, with its counts.
CUT = {**DONE, "truncated": (bool,), "typed": (int,), "accepted": (int,)}
SCROLL_TO = ("down", "up", "top", "bottom", "element")
SCROLLED = {
    **DONE,
    "scroll_y": (int,),
    "scroll_height": (int,),
    "viewport_height": (int,),
    "grew": (bool,),
    "at_end": (bool,),
}
# #11285: page_select says whether the value moved and what it is now.
SELECTED = {"document": (str,), "changed": (bool,), "value": (str,)}
SWITCHED = {"window": (int,), "focused": (bool,)}
DRAGGED = {"document": (str,), "dropped": (bool,)}  # #11288
LANDED = {
    "access": (bool,),
    "document": OPTIONAL,
    "origin": OPTIONAL,
    "settled": (bool,),
}
LISTED = {"origin": (str,), "downloads": (list,)}
DOWNLOAD = {
    "id": (int,),
    "path": (str,),
    "url": (str,),
    "mime": (str,),
    "state": (str,),
    "bytes": (int, float),
    "total": (int, float),
    "error": OPTIONAL,
    "started": (str,),
}


def validate(request: Frame) -> str:
    """Exact fields and types per op; returns the op name. Contract "Field types"."""
    op = request.get("op")
    if not isinstance(op, str) or op not in OPS:
        raise Refusal("InvalidRequest")
    if not OPS[op] <= set(request) <= OPS[op] | optional(op):
        raise Refusal("InvalidRequest")
    controls(request)  # #11356
    for name in ("lease", "request_id", "ref", "target"):
        # #11291: page_scroll names no ref unless it scrolls to an element.
        # #11286: page_press takes a null ref for the focused element.
        nullable = op == "page_scroll" or (op == "page_press" and request.get(name) is None)
        if name in request and not (name == "ref" and nullable):
            string(request, name)
    if op == "page_press":
        mods = request["modifiers"]
        if not key_allowed(request["key"]) or not (
            isinstance(mods, list)
            and all(isinstance(m, str) and m in MODIFIERS for m in mods)
            and len(set(mods)) == len(mods)
        ):
            raise Refusal("InvalidRequest")
    if op == "page_scroll":
        to, ref, wait = request["to"], request["ref"], request["wait_ms"]
        if (
            to not in SCROLL_TO
            or (to == "element") != isinstance(ref, str)
            or not (ref is None or isinstance(ref, str) and ref)
            or type(wait) is not int
            or not 0 <= wait <= 5000
        ):
            raise Refusal("InvalidRequest")
    if "tab" in request:
        tab = request["tab"]
        if type(tab) is not int or tab < 0:
            raise Refusal("InvalidRequest")
    for name in ("text", "value"):
        if name in request:
            item = request[name]
            if not isinstance(item, str) or len(item) > 8192:
                raise Refusal("InvalidRequest")
    if "files" in request and not valid_files(request["files"]):
        raise Refusal("InvalidRequest")
    if "url" in request:
        url = request["url"]
        if not isinstance(url, str) or not 0 < len(url) <= 8192:
            raise Refusal("InvalidRequest")
        if floor_host(url) is None:
            raise Refusal("InvalidUrl")
    return op


class Pages(Grants):
    """Code-session dispatch mixed into ``service.Broker``; workflow sessions pass through."""

    def line(self, grant: Grant, **fields: object) -> dict[str, object]:
        """Audit fields of an operation line: the grant's, with ``tab`` and ``origin`` given."""
        return {**grant.fields(), "tab": None, "origin": None, **fields}

    def drop(self, claim: Claim) -> None:
        key = (claim.peer.generation, claim.tab)
        if self.claims.get(key) is claim:
            del self.claims[key]

    async def dispatch(self, session: Session, request: Frame) -> Frame:
        """Run one code-session op in the contract's check order.

        Why: a code session must stay inside its grant whatever the worker
        does, and no unrecorded result may leave the broker (#11239). What:
        liveness, the fail-closed admitted line, frame and lease validation,
        the broker scope check, then the browser command; a failed result line
        ends the grant with ``audit``.
        Test: `test_tab_grant_select_of_another_tab_is_out_of_scope_and_sends_nothing`,
        `test_result_line_failure_ends_the_grant_and_releases_no_result`.
        """
        grant = session.grant
        if grant is None:
            return await super().dispatch(session, request)
        await self.check_live(grant)
        if grant.state != "active":
            raise gone(grant)
        op = request.get("op")
        name = op if isinstance(op, str) and op in OPS else "invalid"
        target = floor_host(request.get("url")) if name == "page_navigate" else None
        admitted = self.line(grant, op=name, outcome="admitted", origin=target)
        try:
            self.audit.write("operation", **admitted)
        except OSError:
            raise Refusal("AuditUnavailable") from None
        grant.last_used = codes.clock()
        try:
            reply, tab, origin = await self.run(
                session, grant, validate(request), request
            )
        except Refusal as exc:
            seen = exc.response.get("tab")
            self.audit.best_effort(
                "operation",
                **self.line(
                    grant,
                    op=name,
                    outcome=str(exc),
                    tab=seen if type(seen) is int else None,
                ),
            )
            if grant.state != "active" and str(exc) != "OutcomeUnknown":
                raise gone(grant) from None
            raise
        unknown: Frame = {"ok": False, "error": "OutcomeUnknown"}
        if grant.state != "active":
            self.audit.best_effort(
                "operation", **self.line(grant, op=name, outcome="grant_ended", tab=tab)
            )
            if name not in MUTATING:
                raise gone(grant)
            session.journal[str(request["request_id"])] = (remembered(request), unknown)
            return unknown
        grant.last_used = codes.clock()
        outcome = reply.get("error", "ok") if reply.get("ok") is False else "ok"
        try:
            self.audit.write(
                "operation",
                **self.line(grant, op=name, outcome=outcome, tab=tab, origin=origin),
            )
        except OSError:
            # The op ran but is unrecorded: end the grant and release no result.
            await self.end(grant, "audit")
            if name not in MUTATING:
                raise gone(grant) from None
            session.journal[str(request["request_id"])] = (remembered(request), unknown)
            return unknown
        return reply

    def peer_of(self, grant: Grant) -> Peer:
        peer = self.peers.get(str(grant.row["profile"]))
        if peer is None or peer.generation != grant.generation:
            raise Refusal("BrowserDisconnected")
        return peer

    async def send(
        self, grant: Grant, claim: Claim | None, peer: Peer, op: str, **args: object
    ) -> object:
        """One page command; applies the contract's grant and lease fate on refusal."""
        if claim is not None:
            claim.busy = True
        try:
            return await peer.page(op, grant.ident, **args)
        except Refusal as exc:
            code = str(exc)
            if code in KEEP_LEASE:
                raise
            if claim is not None:
                self.drop(claim)
            if code == "TabNotFound" and grant.scope == "tab":
                await self.end(grant, "tab_closed")
            elif (
                code == "UnknownGrant"
                or (
                    code in ("BrowserDisconnected", "OutcomeUnknown")
                    and self.peers.get(str(grant.row["profile"])) is not peer
                    # #11239: a grant in its reconnect grace, or already rebound, lives on.
                    and grant.grace_until is None
                    and grant.generation == peer.generation
                )
            ):
                await self.end(grant, "browser_gone")
            raise
        finally:
            if claim is not None:
                claim.busy = False

    def leased(self, session: Session, grant: Grant, request: Frame) -> Claim:
        """The session's live lease named by the frame, inside the grant's scope."""
        token = string(request, "lease")
        claim = next(
            (
                c
                for c in self.claims.values()
                if c.owner == session.owner and c.token == token
            ),
            None,
        )
        if claim is None or self.active(claim.peer, claim.tab) is not claim:
            raise Refusal("StaleLease")
        if claim.peer is not self.peers.get(str(grant.row["profile"])):
            self.drop(claim)
            raise Refusal("StaleLease")
        if grant.scope == "tab" and claim.tab != grant.tab:
            self.drop(claim)
            raise Refusal("OutOfScope", tab=claim.tab)
        return claim

    async def run(
        self, session: Session, grant: Grant, op: str, request: Frame
    ) -> tuple[Frame, int | None, str | None]:
        """Run a validated op; returns the reply, the operated tab and the result host."""
        if op == "tabs":
            result = await self.send(grant, None, self.peer_of(grant), "page_tabs")
            entries = result.get("tabs") if isinstance(result, dict) else None
            if not isinstance(entries, list):
                raise Refusal("BrowserRefused")
            tabs = [
                {k: e.get(k) for k in TAB_KEYS}
                for e in entries
                if isinstance(e, dict)
                and type(e.get("tab")) is int
                and (grant.scope != "tab" or e["tab"] == grant.tab)
            ]
            return {"ok": True, "tabs": tabs}, None, None
        if op == "select":
            tab = request["tab"]
            assert isinstance(tab, int)
            return await self.select(session, grant, tab)
        if op in MUTATING:
            # #11239: a journaled request answers before lease validation, so an
            # OutcomeUnknown that dropped the lease still never runs twice.
            previous = session.journal.get(string(request, "request_id"))
            if previous is not None:
                if remembered(request) != previous[0]:
                    raise Refusal("Conflict")
                seen = previous[1].get("tab")
                return previous[1], seen if type(seen) is int else None, None
        claim = self.leased(session, grant, request)
        if op == "release":
            self.drop(claim)
            return {"ok": True}, claim.tab, None
        tab = claim.tab
        if op == "switch":
            return await self.switch(grant, claim)
        if op in ("page_read", "page_find"):
            result = shaped(
                await self.send(
                    grant,
                    claim,
                    claim.peer,
                    op,
                    tab=tab,
                    document=claim.document,
                    **controls(request),
                ),
                READ if op == "page_read" else FOUND,
            )
            changed = result["document"] != claim.document
            claim.document, claim.origin = (
                str(result["document"]),
                str(result["origin"]),
            )
            claim.deadline = time.monotonic() + TTL
            reply = {"ok": True, "tab": tab, **result, "document_changed": changed}
            return reply, tab, host(result["origin"])
        if op == "page_downloads":
            raw = await self.send(grant, claim, claim.peer, op, tab=tab)
            listed = shaped(raw, LISTED)
            found = [shaped(d, DOWNLOAD) for d in listed["downloads"][:20]]
            claim.deadline = time.monotonic() + TTL
            reply = {"ok": True, "tab": tab, "origin": listed["origin"]}
            reply["downloads"] = found
            return reply, tab, host(listed["origin"])
        rid = string(request, "request_id")
        if len(session.journal) >= 4096:
            raise Refusal("SessionFull")
        if op == "page_navigate":
            args: Frame = {"url": request["url"]}
        elif op in HISTORY:
            args = {}
        else:
            args = {"document": claim.document, "ref": request["ref"]}
            if op == "page_click":  # #11294: the dialog policy
                args.update(controls(request))
            if op == "page_type":
                args["text"] = request["text"]
            if op == "page_scroll":
                args.update(to=request["to"], wait_ms=request["wait_ms"])
            if op == "page_upload":
                args["files"] = request["files"]
            if op == "page_press":  # #11286
                args.update(key=request["key"], modifiers=request["modifiers"])
            if op == "page_select":
                args["value"] = request["value"]
            if op == "page_drag":
                args["target"] = request["target"]
        try:
            raw = await self.send(grant, claim, claim.peer, op, tab=tab, **args)
        except Refusal as exc:
            # #11352: TextDropped reports typing that ran, so a retry must not type again.
            if str(exc) in ("OutcomeUnknown", "TextDropped"):
                session.journal[rid] = (remembered(request), exc.response)
            raise
        if op == "page_navigate" or op in HISTORY:
            result = shaped(raw, LANDED)
            claim.document = result["document"]  # type: ignore[assignment]
            claim.origin = result["origin"] if result["access"] else None  # type: ignore[assignment]
            where = host(claim.origin)
        else:
            shape = {"page_scroll": SCROLLED, "page_select": SELECTED, "page_drag": DRAGGED}
            shape = shape.get(op, DONE)
            if op in ("page_type", "page_press") and isinstance(raw, dict) and "truncated" in raw:
                shape = CUT
            result = clicked(raw) if op == "page_click" else shaped(raw, shape)
            where = host(claim.origin)
        claim.deadline = time.monotonic() + TTL
        reply = {"ok": True, "tab": tab, **result}
        session.journal[rid] = (remembered(request), reply)
        return reply, tab, where

    async def switch(
        self, grant: Grant, claim: Claim
    ) -> tuple[Frame, int | None, str | None]:
        """Make the leased tab the active tab of its window and focus that window.

        Why: an agent on a profile grant must reach a tab a link opened, and the
        human must see which tab it drives (#11293). What: needs this session's
        own lease, so a tab another session holds stays out of reach; a tab
        grant answers ``OutOfScope`` because focus changes browser-wide state.
        Idempotent, so it is not journaled.
        Test: `test_profile_grant_switch_focuses_the_leased_tab`,
        `test_switch_needs_this_sessions_own_lease`.
        """
        if grant.scope == "tab":
            raise Refusal("OutOfScope", tab=claim.tab)
        result = shaped(
            await self.send(grant, claim, claim.peer, "page_switch", tab=claim.tab),
            SWITCHED,
        )
        claim.deadline = time.monotonic() + TTL
        reply = {"ok": True, "tab": claim.tab, **result}
        return reply, claim.tab, host(claim.origin)

    async def select(
        self, session: Session, grant: Grant, tab: int
    ) -> tuple[Frame, int | None, str | None]:
        """Take or return this session's lease on a tab; one controller per tab."""
        if grant.scope == "tab" and tab != grant.tab:
            raise Refusal("OutOfScope", tab=tab)
        peer = self.peer_of(grant)
        claim = self.active(peer, tab)
        if claim is not None and claim.owner != session.owner:
            raise Refusal(
                "TabClaimed",
                holder=claim.owner,
                expires_at=time.time() + max(0.0, claim.deadline - time.monotonic()),
            )
        created = claim is None
        if claim is None:
            token = secrets.token_urlsafe(24)
            deadline = time.monotonic() + TTL
            claim = Claim(peer, tab, session.owner, token, deadline, indicator=False)
            self.claims[(peer.generation, tab)] = claim
        try:
            seen = shaped(
                await self.send(grant, claim, peer, "page_tab", tab=tab), PROBE
            )
            if self.claims.get((peer.generation, tab)) is not claim:
                raise Refusal("StaleLease")
        except Refusal:
            if created:
                self.drop(claim)
            raise
        claim.document = seen["document"]  # type: ignore[assignment]
        claim.deadline = time.monotonic() + TTL
        claim.origin = seen["origin"] if seen["access"] else None  # type: ignore[assignment]
        reply = {"ok": True, "lease": claim.token, "tab": tab, **seen}
        return reply, tab, host(seen["origin"]) if seen["access"] else None

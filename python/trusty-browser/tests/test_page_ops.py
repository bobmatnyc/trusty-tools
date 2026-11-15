"""Connected-profile page operations (#11239) through the real broker and a fake worker."""

# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import asyncio

import pytest
from test_activation_codes import (  # noqa: F401
    DOWNLOAD,
    ORIGIN,
    PAGE_TEXT,
    TITLE,
    FakeBrowser,
    clock,
    gone,
    run,
    session,
    settle,
    short,
)

from trusty_browser.page_query import FIND_DEFAULTS, READ_DEFAULTS
from trusty_browser import codes, protocol

CLOSED = (EOFError, asyncio.IncompleteReadError, ConnectionError)


def test_profile_grant_switch_focuses_the_leased_tab(short, clock):
    # #11293: the link-opened tab 8 is reachable and brought to the front.
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 8}))["lease"]
        assert await call({"op": "switch", "lease": lease}) == {
            "ok": True,
            "tab": 8,
            "window": 1,
            "focused": True,
        }
        assert rig.a.sent("page_switch")[-1]["tab"] == 8
        assert rig.a.tabs[8]["active"] and not rig.a.tabs[7]["active"]
        listed = {t["tab"]: t["active"] for t in (await call({"op": "tabs"}))["tabs"]}
        assert listed[8] is True and listed[7] is False

    run(short, body)


def test_switch_needs_this_sessions_own_lease(short, clock):
    async def body(rig):
        _, first = await session(rig)
        lease = (await first({"op": "select", "tab": 8}))["lease"]
        _, second = await session(rig)
        assert (await second({"op": "select", "tab": 8}))["error"] == "TabClaimed"
        assert await second({"op": "switch", "lease": lease}) == {
            "ok": False,
            "error": "StaleLease",
        }
        assert not rig.a.sent("page_switch")
        assert (await first({"op": "switch", "lease": lease}))["ok"]

    run(short, body)


def test_profile_grant_never_reaches_another_profile(short, clock):
    async def body(rig):
        _, travel = await session(rig, rig.beta)
        beta_lease = (await travel({"op": "select", "tab": 7}))["lease"]
        seen = list(rig.b.frames)
        _, call = await session(rig)
        assert (await call({"op": "select", "tab": 7, "profile": rig.beta["profile"]}))[
            "error"
        ] == "InvalidRequest"
        assert (await call({"op": "page_read", "lease": beta_lease}))["error"] == (
            "StaleLease"
        )
        assert (await call({"op": "tabs"}))["ok"]
        assert rig.b.frames == seen
        assert all(f["email"] == rig.alpha["email"] for f in rig.a.sent("page_tabs"))

    run(short, body)


def test_tabs_on_a_profile_grant_lists_every_tab_in_shape(short, clock):
    async def body(rig):
        _, whole = await session(rig)
        listed = (await whole({"op": "tabs"}))["tabs"]
        assert [e["tab"] for e in listed] == [7, 8, 9]
        assert all("extra" not in e for e in listed)
        assert listed[1]["origin"] is None and listed[1]["title"] is None

    run(short, body)


def test_stale_generation_lease_answers_grant_ended_and_sends_nothing_new(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
        assert ack["grant"] not in rig.a.ack["live"]
        ended = await gone(call, {"op": "page_read", "lease": lease})
        assert ended == {"ok": False, "error": "GrantEnded", "reason": "browser_gone"}
        assert rig.a.sent("page_read", "page_tab") == []

    run(short, body)


def test_document_changed_is_relayed_without_extra_fields_and_not_journaled(
    short, clock
):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.tabs[7]["document"] = "d7-2"
        frames = {
            "page_click": {
                "op": "page_click",
                "lease": lease,
                "request_id": "c",
                "ref": "1.0",
            },
            "page_type": {
                "op": "page_type",
                "lease": lease,
                "request_id": "t",
                "ref": "1.0",
                "text": "hi",
            },
        }
        for op, frame in frames.items():
            assert await call(frame) == {
                "ok": False,
                "error": "DocumentChanged",
                "tab": 7,
                "access": True,
                "document": "d7-2",
            }
            assert rig.a.sent(op)[-1]["document"] == "d7-1"
        assert rig.broker.grants and len(rig.broker.claims) == 1
        assert (
            not rig.a.done
            and not rig.broker.grants[next(iter(rig.broker.grants))].sessions[0].journal
        )
        read = await call({"op": "page_read", "lease": lease})
        assert read["document"] == "d7-2" and read["document_changed"] is True
        again = await call({"op": "page_read", "lease": lease})
        assert again["document_changed"] is False
        for op, frame in frames.items():
            assert (await call(frame)) == {"ok": True, "tab": 7, "document": "d7-2"}
            assert rig.a.sent(op)[-1]["document"] == "d7-2"
        assert [d[0] for d in rig.a.done] == ["page_click", "page_type"]

    run(short, body)


def test_navigation_pending_is_relayed_and_keeps_the_lease(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse["page_read"] = {"error": "NavigationPending", "tab": 7}
        read = {"op": "page_read", "lease": lease}
        assert await call(read) == {"ok": False, "error": "NavigationPending", "tab": 7}
        assert len(rig.broker.claims) == 1
        del rig.a.refuse["page_read"]
        rig.a.tabs[7]["document"] = "d7-2"
        landed = await call(read)
        assert landed["document"] == "d7-2" and landed["document_changed"] is True

    run(short, body)


def test_permission_needed_is_relayed_and_keeps_grant_and_lease(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        selected = await call({"op": "select", "tab": 8})
        assert selected == {
            "ok": True,
            "lease": selected["lease"],
            "tab": 8,
            "access": False,
            "document": None,
            "origin": None,
            "title": None,
        }
        lease = selected["lease"]
        assert await call({"op": "page_read", "lease": lease}) == {
            "ok": False,
            "error": "PermissionNeeded",
            "tab": 8,
            "origin": None,
        }
        assert rig.broker.grants[ack["grant"]].state == "active"
        landed = await call(
            {
                "op": "page_navigate",
                "lease": lease,
                "request_id": "n",
                "url": ORIGIN + "/",
            }
        )
        assert landed["access"] is True and landed["settled"] is True
        assert (await call({"op": "page_read", "lease": lease}))["text"] == PAGE_TEXT

    run(short, body)


def test_select_claim_is_exclusive_between_sessions_and_refusal_takes_nothing(
    short, clock
):
    async def body(rig):
        _, first = await session(rig)
        _, second = await session(rig)
        held = await first({"op": "select", "tab": 7})
        refused = await second({"op": "select", "tab": 7})
        assert refused["error"] == "TabClaimed"
        assert set(refused) == {"ok", "error", "holder", "expires_at"}
        assert len(rig.broker.claims) == 1
        assert (await first({"op": "select", "tab": 7}))["lease"] == held["lease"]
        other = await second({"op": "select", "tab": 9})
        assert other["ok"] and len(rig.broker.claims) == 2
        assert (await first({"op": "release", "lease": held["lease"]})) == {"ok": True}
        taken = await second({"op": "select", "tab": 7})
        assert taken["ok"] and taken["lease"] != held["lease"]
        assert (await first({"op": "page_read", "lease": held["lease"]}))["error"] == (
            "StaleLease"
        )

    run(short, body)


def test_page_operations_relay_the_contract_shapes(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        read = await call({"op": "page_read", "lease": lease})
        assert set(read) == {
            "ok",
            "tab",
            "document",
            "document_changed",
            "origin",
            "url",
            "title",
            "text",
            "elements",
            "truncated",
        }
        assert read["document_changed"] is False and read["elements"][0]["ref"] == "1.0"
        typed = {
            "op": "page_type",
            "lease": lease,
            "request_id": "t",
            "ref": "1.0",
            "text": "",
        }
        assert await call(typed) == {"ok": True, "tab": 7, "document": "d7-1"}
        assert await call(typed) == {"ok": True, "tab": 7, "document": "d7-1"}
        assert len(rig.a.sent("page_type")) == 1  # the journal answered the repeat
        assert (await call({**typed, "text": "other"}))["error"] == "Conflict"
        away = await call(
            {
                "op": "page_navigate",
                "lease": lease,
                "request_id": "n",
                "url": "https://elsewhere.test/x",
            }
        )
        assert away == {
            "ok": True,
            "tab": 7,
            "access": False,
            "document": None,
            "origin": None,
            "settled": True,
        }
        assert (await call({"op": "page_read", "lease": lease}))["error"] == (
            "PermissionNeeded"
        )
        for frame in rig.a.frames:
            if frame["op"].startswith("page_"):
                assert {"url", "account"}.isdisjoint(frame) or frame["op"] == (
                    "page_navigate"
                )
                assert frame["grant"] == ack["grant"] and "account" not in frame

    run(short, body)


@pytest.mark.parametrize("route", ["operator", "popup", "unknown_grant"])
def test_ended_grant_answers_grant_ended_and_sends_grant_end(short, clock, route):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = {"op": "page_read", "lease": lease}
        reason = {"operator": "revoked", "popup": "revoked"}
        if route == "operator":
            await rig.operator(op="revoke_grant", grant=ack["grant"])
        elif route == "popup":
            await rig.popup(rig.alpha, "revoke_grant", grant=ack["grant"])
        else:
            rig.a.refuse["page_read"] = {"error": "UnknownGrant"}
            reason[route] = "browser_gone"
        ended = await gone(call, frame)
        assert ended == {"ok": False, "error": "GrantEnded", "reason": reason[route]}
        await settle(rig)
        sent = rig.a.sent("grant_end")
        assert [(f["grant"], f["reason"], f["last"]) for f in sent] == [
            (ack["grant"], reason[route], True)
        ]
        assert not rig.broker.claims

    run(short, body)


def test_grant_end_reaches_the_worker_on_every_end_route_with_last(short, clock):
    async def body(rig):
        ack, _one = await session(rig)
        _, two = await session(rig)
        rig.writers[0].close()  # an agent leaving ends nothing
        await settle(rig)
        assert rig.a.sent("grant_end") == []
        await rig.operator(op="revoke_grant", grant=ack["grant"])
        await settle(rig)
        last = rig.a.sent("grant_end")[-1]
        assert (last["grant"], last["reason"], last["last"]) == (
            ack["grant"],
            "revoked",
            True,
        )
        assert rig.broker.sweeper is None and not rig.broker.grants
        kept = await rig.grant(rig.alpha)
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair()
        assert rig.a.ack["live"] == [kept["grant"]]
        assert (await gone(two))["reason"] == "revoked"

    run(short, body)


def test_malformed_requests_answer_invalid_and_send_nothing(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        sent = len(rig.a.frames)
        base = {
            "op": "page_type",
            "lease": lease,
            "request_id": "t",
            "ref": "1.0",
            "text": "x",
        }
        # #11291: page_scroll's own fields.
        scroll = {
            "op": "page_scroll",
            "lease": lease,
            "request_id": "s",
            "to": "down",
            "ref": None,
            "wait_ms": 0,
        }
        invalid = [
            {"op": "tabs", "extra": 1},
            {"op": "select"},
            {"op": "select", "tab": "7"},
            {"op": "select", "tab": -1},
            {"op": "select", "tab": True},
            {**base, "text": 5},
            {**base, "text": "x" * 8193},
            {**base, "ref": ""},
            {k: v for k, v in base.items() if k != "ref"},
            {"op": "claim"},
            {"op": "snapshot", "lease": lease, "request_id": "s"},
            {"op": "click", "lease": lease, "request_id": "c", "marker": "m"},
            {"op": "page_navigate", "lease": lease, "request_id": "n", "url": 5},
            {"op": "unknown"},
            *(
                {**scroll, **bad}
                for bad in (
                    {"to": "sideways"},
                    {"to": "element"},
                    {"ref": "1.0"},
                    {"to": "element", "ref": ""},
                    {"wait_ms": -1},
                    {"wait_ms": 5001},
                    {"wait_ms": 1.5},
                    {"wait_ms": True},
                )
            ),
            {k: v for k, v in scroll.items() if k != "wait_ms"},
        ]
        for frame in invalid:
            assert (await call(frame)) == {"ok": False, "error": "InvalidRequest"}, (
                frame
            )
        for url in (
            "javascript:alert(1)",
            "http://example.test/",
            "https://user:pw@qa.test/",
            "file:///etc/passwd",
            "data:text/html,x",
            "https://qa.test:99999/",
        ):
            frame = {
                "op": "page_navigate",
                "lease": lease,
                "request_id": "n",
                "url": url,
            }
            assert (await call(frame)) == {"ok": False, "error": "InvalidUrl"}, url
        assert len(rig.a.frames) == sent and len(rig.broker.claims) == 1
        local = {"op": "page_navigate", "lease": lease, "request_id": "l"}
        assert (await call({**local, "url": "http://127.0.0.1:8080/"}))["ok"]

    run(short, body)


@pytest.mark.parametrize(
    "op", ["page_click", "page_type", "page_press", "page_navigate"]
)
def test_lost_reply_on_click_type_navigate_is_outcome_unknown_and_not_retried(
    short, clock, monkeypatch, op
):
    monkeypatch.setattr(protocol, "PAGE_WAIT", 0.2)

    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = {
            "page_click": {"op": op, "lease": lease, "request_id": "r", "ref": "1.0"},
            "page_type": {
                "op": op,
                "lease": lease,
                "request_id": "r",
                "ref": "1.0",
                "text": "x",
            },
            "page_press": mutating_frame(op, lease),
            "page_navigate": {
                "op": op,
                "lease": lease,
                "request_id": "r",
                "url": ORIGIN,
            },
        }[op]
        rig.a.hold[op] = asyncio.Event()  # the worker never answers
        assert await call(frame) == {"ok": False, "error": "OutcomeUnknown"}
        assert len(rig.a.sent(op)) == 1
        assert not rig.broker.claims
        await rig.a.task  # the broker dropped the silent browser
        # #11239: the grant waits out the reconnect grace, then ends.
        assert await call(frame) == {"ok": False, "error": "BrowserDisconnected"}
        clock[0] += codes.RECONNECT_GRACE
        ended = await gone(call, frame)
        assert ended == {"ok": False, "error": "GrantEnded", "reason": "browser_gone"}
        assert len(rig.a.sent(op)) == 1

    run(short, body)


def test_lost_reply_on_read_is_browser_disconnected(short, clock, monkeypatch):
    monkeypatch.setattr(protocol, "PAGE_WAIT", 0.2)

    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.hold["page_read"] = asyncio.Event()
        assert await call({"op": "page_read", "lease": lease}) == {
            "ok": False,
            "error": "BrowserDisconnected",
        }

    run(short, body)


def test_result_line_failure_ends_the_grant_and_releases_no_result(
    short, clock, monkeypatch
):
    async def body(rig):
        real = rig.broker.audit.write

        def results_fail(event, **fields):
            if event == "operation" and fields.get("outcome") == "ok":
                raise OSError("disk full")
            return real(event, **fields)

        _, reader = await session(rig)
        lease = (await reader({"op": "select", "tab": 7}))["lease"]
        monkeypatch.setattr(rig.broker.audit, "write", results_fail)
        ended = await gone(reader, {"op": "page_read", "lease": lease})
        assert ended == {"ok": False, "error": "GrantEnded", "reason": "audit"}
        assert PAGE_TEXT not in str(ended)
        monkeypatch.setattr(rig.broker.audit, "write", real)
        _, clicker = await session(rig)
        lease = (await clicker({"op": "select", "tab": 7}))["lease"]
        monkeypatch.setattr(rig.broker.audit, "write", results_fail)
        frame = {"op": "page_click", "lease": lease, "request_id": "c", "ref": "1.0"}
        assert await clicker(frame) == {"ok": False, "error": "OutcomeUnknown"}
        assert [d[0] for d in rig.a.done] == ["page_click"]  # it did run
        monkeypatch.setattr(rig.broker.audit, "write", real)
        assert (await gone(clicker, frame))["reason"] == "audit"
        await settle(rig)
        assert [f["reason"] for f in rig.a.sent("grant_end")] == ["audit", "audit"]
        assert not rig.broker.grants and not rig.broker.claims

    run(short, body)


def test_admitted_line_failure_sends_nothing_and_keeps_the_lease(
    short, clock, monkeypatch
):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        real = rig.broker.audit.write

        def broken(*_a, **_k):
            raise OSError("disk full")

        monkeypatch.setattr(rig.broker.audit, "write", broken)
        frame = {"op": "page_click", "lease": lease, "request_id": "c", "ref": "1.0"}
        assert await call(frame) == {"ok": False, "error": "AuditUnavailable"}
        monkeypatch.setattr(rig.broker.audit, "write", real)
        assert rig.a.sent("page_click") == [] and len(rig.broker.claims) == 1
        assert not rig.broker.grants[ack["grant"]].sessions[0].journal
        assert (await call(frame))["ok"]

    run(short, body)


@pytest.mark.parametrize(
    "refusal, lease_kept",
    [
        ({"error": "StaleElement", "tab": 7}, True),
        ({"error": "NotEditable", "tab": 7}, True),
        ({"error": "OutOfScope", "tab": 7}, False),
        ({"error": "TabNotFound", "tab": 7}, False),
        ({"error": "ProfileMismatch"}, False),
        ({"error": "SomethingNew", "tab": 7}, False),
    ],
)
def test_worker_refusals_apply_the_contract_fate(short, clock, refusal, lease_kept):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse["page_type"] = {**refusal, "text": PAGE_TEXT}
        frame = {
            "op": "page_type",
            "lease": lease,
            "request_id": "t",
            "ref": "1.0",
            "text": "x",
        }
        reply = await call(frame)
        expected = {k: v for k, v in refusal.items()}
        if refusal["error"] == "SomethingNew":
            expected = {"error": "BrowserRefused"}
        assert reply == {"ok": False, **expected}
        assert (len(rig.broker.claims) == 1) is lease_kept
        assert rig.broker.grants[ack["grant"]].state == "active"
        assert not rig.broker.grants[ack["grant"]].sessions[0].journal

    run(short, body)


def test_page_read_sends_its_controls_and_refuses_bad_ones(short, clock):
    # #11356: absent controls take their defaults; a bad one sends nothing.
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        assert (await call({"op": "page_read", "lease": lease}))["ok"]
        scoped = {"scope_ref": "4.1", "max_chars": 2000, "include_text": True}
        assert (await call({"op": "page_read", "lease": lease, **scoped}))["ok"]
        sent = [{k: f[k] for k in READ_DEFAULTS} for f in rig.a.sent("page_read")]
        assert sent == [READ_DEFAULTS, {**READ_DEFAULTS, **scoped}]
        for bad in ({"max_chars": 999}, {"max_chars": True}, {"scope_ref": ""},
                    {"viewport_only": "yes"}, {"include_text": 1}, {"limit": 3}):  # fmt: skip
            reply = await call({"op": "page_read", "lease": lease, **bad})
            assert reply == {"ok": False, "error": "InvalidRequest"}, bad
        assert len(rig.a.sent("page_read")) == 2

    run(short, body)


def test_page_find_relays_matches_and_refuses_bad_terms(short, clock):
    # #11357: absent terms take their defaults; a find needs role, name or text.
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        found = await call({"op": "page_find", "lease": lease, "name": "Delete"})
        assert found["ok"] and found["matches"][0]["in"]["role"] == "row"
        assert found["document_changed"] is False and found["truncated"] is False
        sent = rig.a.sent("page_find")[0]
        assert {k: sent[k] for k in FIND_DEFAULTS} == {
            **FIND_DEFAULTS,
            "name": "Delete",
        }
        for bad in ({}, {"name": ""}, {"name": "x", "limit": 0}, {"name": "x", "limit": 51},
                    {"role": 3}, {"name": "x", "within_ref": ""}, {"name": "x", "max_chars": 2000}):  # fmt: skip
            reply = await call({"op": "page_find", "lease": lease, **bad})
            assert reply == {"ok": False, "error": "InvalidRequest"}, bad
        assert len(rig.a.sent("page_find")) == 1

    run(short, body)


def test_maxlength_cut_is_a_truncated_success_and_journaled(short, clock):
    # #11352: a maxlength cut answers ok with truncated, typed and accepted; a retry types nothing.
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        cut = {"document": "d7-1", "truncated": True, "typed": 6, "accepted": 3}
        rig.a.refuse["page_type"] = cut
        frame = {**mutating_frame("page_type", lease, "m"), "ref": "1.0", "text": "abcdef"}
        assert await call(frame) == {"ok": True, "tab": 7, **cut}
        del rig.a.refuse["page_type"]
        assert await call(frame) == {"ok": True, "tab": 7, **cut}
        assert len(rig.a.sent("page_type")) == 1
        rig.a.refuse["page_type"] = {**cut, "typed": "6"}
        bad = {**frame, "request_id": "n"}
        assert await call(bad) == {"ok": False, "error": "BrowserRefused"}

    run(short, body)


def test_text_dropped_is_relayed_with_its_counts_and_journaled(short, clock):
    # #11352: the typing ran, so the refusal keeps the lease and a retry types nothing.
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        dropped = {"error": "TextDropped", "tab": 7, "typed": 3, "accepted": 2,
                   "entered": "ab"}  # fmt: skip
        rig.a.refuse["page_type"] = dropped
        frame = {
            "op": "page_type",
            "lease": lease,
            "request_id": "t",
            "ref": "1.0",
            "text": "axb",
        }
        assert await call(frame) == {"ok": False, **dropped}
        assert len(rig.broker.claims) == 1
        del rig.a.refuse["page_type"]
        assert await call(frame) == {"ok": False, **dropped}
        assert len(rig.a.sent("page_type")) == 1

    run(short, body)


@pytest.mark.parametrize("error", ["NotSelectable", "NoSuchOption", "StaleElement"])
def test_page_select_refusals_keep_the_lease_and_are_not_journaled(short, clock, error):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse["page_select"] = {"error": error, "tab": 7}
        reply = await call(mutating_frame("page_select", lease))
        assert reply == {"ok": False, "error": error, "tab": 7}
        assert len(rig.broker.claims) == 1
        assert not rig.broker.grants[ack["grant"]].sessions[0].journal
        del rig.a.refuse["page_select"]
        assert (await call(mutating_frame("page_select", lease)))["changed"] is True

    run(short, body)


def test_page_select_reply_without_changed_is_browser_refused(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse["page_select"] = {"document": "d7-1"}
        reply = await call(mutating_frame("page_select", lease))
        assert reply == {"ok": False, "error": "BrowserRefused"}
        bad = {**mutating_frame("page_select", lease, "v"), "value": 5}
        assert (await call(bad))["error"] == "InvalidRequest"

    run(short, body)


def test_drag_relays_both_refs_reports_dropped_and_replays_from_the_journal(
    short, clock
):
    # #11288: page_drag is a journaled mutating op whose reply carries `dropped`.
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = mutating_frame("page_drag", lease, "d")
        done = {"ok": True, "tab": 7, "document": "d7-1", "dropped": True}
        assert await call(frame) == done
        assert await call(frame) == done  # the retry answers from the journal
        sent = rig.a.sent("page_drag")
        assert len(sent) == 1 and (sent[0]["ref"], sent[0]["target"]) == ("1.0", "1.1")
        refused = {**frame, "request_id": "e", "target": "1.9"}
        assert (await call(refused))["dropped"] is False
        assert await call({**frame, "request_id": "f", "target": 5}) == {
            "ok": False,
            "error": "InvalidRequest",
        }
        rig.a.refuse["page_drag"] = {"error": "NotDraggable", "tab": 7}
        assert await call({**frame, "request_id": "g"}) == {
            "ok": False,
            "error": "NotDraggable",
            "tab": 7,
        }
        assert len(rig.broker.claims) == 1  # NotDraggable keeps the lease

    run(short, body)


def mutating_frame(op, lease, rid="r"):
    base = {"op": op, "lease": lease, "request_id": rid}
    return {
        "page_click": {**base, "ref": "1.0"},
        "page_hover": {**base, "ref": "1.0"},
        "page_type": {**base, "ref": "1.0", "text": "x"},
        "page_press": {**base, "ref": None, "key": "Enter", "modifiers": ["Shift"]},
        "page_select": {**base, "ref": "1.0", "value": "x"},
        "page_drag": {**base, "ref": "1.0", "target": "1.1"},
        "page_navigate": {**base, "url": ORIGIN},
        "page_upload": {**base, "ref": "1.0", "files": [FILE]},
        "page_back": base,
        "page_forward": base,
        "page_reload": base,
    }[op]


def test_page_press_validates_key_and_modifiers_before_sending(short, clock):
    """#11286: a bad key or modifier list is refused by the broker; a good one reaches the worker."""

    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = mutating_frame("page_press", lease)
        bad = [
            {"key": "Return"},
            {"key": ""},
            {"key": "ab"},
            {"key": "\n"},
            {"key": "\u00a0"},
            {"key": 13},
            {"modifiers": ["Ctrl"]},
            {"modifiers": ["Shift", "Shift"]},
            {"modifiers": "Shift"},
            {"ref": ""},
            {"ref": 1},
        ]
        for change in bad:
            reply = await call({**frame, **change, "request_id": repr(change)})
            assert reply == {"ok": False, "error": "InvalidRequest"}, change
        missing = {k: v for k, v in frame.items() if k != "modifiers"}
        assert (await call(missing))["error"] == "InvalidRequest"
        assert rig.a.sent("page_press") == []
        for rid, change in enumerate(
            [
                {},
                {"ref": "1.0", "key": "a", "modifiers": ["Control", "Meta"]},
                {"key": "é"},
            ]
        ):
            sent = {**frame, **change, "request_id": str(rid)}
            assert await call(sent) == {"ok": True, "tab": 7, "document": "d7-1"}
            worker = rig.a.sent("page_press")[-1]
            assert {k: worker[k] for k in ("ref", "key", "modifiers", "document")} == {
                "ref": sent["ref"],
                "key": sent["key"],
                "modifiers": sent["modifiers"],
                "document": "d7-1",
            }
        assert len(rig.a.sent("page_press")) == 3

    run(short, body)


FILE = {"name": "a.txt", "type": "text/plain", "data": "aGVsbG8="}


def test_back_forward_and_reload_walk_the_tab_history_within_a_profile_grant(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = lambda op, rid: mutating_frame(op, lease, rid)  # noqa: E731
        assert await call(frame("page_back", "b0")) == {
            "ok": False,
            "error": "NoHistory",
            "tab": 7,
        }
        assert len(rig.broker.claims) == 1  # NoHistory keeps the lease
        away = mutating_frame("page_navigate", lease, "n")
        away["url"] = "https://elsewhere.test/second"
        assert (await call(away))["access"] is False
        back = await call(frame("page_back", "b1"))
        assert back["ok"] and back["access"] is True and back["origin"] == ORIGIN
        assert back["settled"] is True and back["tab"] == 7
        assert await call(frame("page_back", "b1")) == back  # journaled, not re-run
        assert len(rig.a.sent("page_back")) == 2
        reloaded = await call(frame("page_reload", "r1"))
        assert reloaded["ok"] and reloaded["origin"] == ORIGIN
        assert reloaded["document"] != back["document"]
        read = await call({"op": "page_read", "lease": lease})
        assert read["document"] == reloaded["document"] and not read["document_changed"]
        ahead = await call(frame("page_forward", "f1"))
        assert ahead["ok"] and ahead["access"] is False and ahead["origin"] is None
        assert (await call(frame("page_forward", "f2")))["error"] == "NoHistory"
        frames = rig.a.sent("page_back", "page_forward", "page_reload")
        assert all(
            f["tab"] == 7 and set(f) == {"id", "op", "email", "grant", "tab"}
            for f in frames
        )

    run(short, body)


@pytest.mark.parametrize("op", ["page_back", "page_forward", "page_reload"])
def test_history_ops_refuse_outside_the_grant_and_send_nothing(short, clock, op):
    async def body(rig):
        _, whole = await session(rig)
        other = (await whole({"op": "select", "tab": 9}))["lease"]
        await whole({"op": "release", "lease": other})
        _, call = await session(rig)
        sent = len(rig.a.sent(op))
        assert (await call(mutating_frame(op, other)))["error"] == "StaleLease"
        assert (await call({**mutating_frame(op, "x"), "tab": 9}))["error"] == (
            "InvalidRequest"
        )
        assert len(rig.a.sent(op)) == sent == 0

    run(short, body)


@pytest.mark.parametrize("worker_error", ["OutcomeUnknown", "BrowserDisconnected"])
@pytest.mark.parametrize(
    "op",
    [
        "page_click",
        "page_hover",
        "page_type",
        "page_press",
        "page_select",
        "page_navigate",
        "page_upload",
        "page_back",
        "page_reload",
        "page_drag",
    ],
)
def test_worker_outcome_unknown_is_journaled_and_a_retry_sends_nothing(
    short, clock, op, worker_error
):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse[op] = {"error": worker_error, "text": PAGE_TEXT}
        frame = mutating_frame(op, lease)
        unknown = {"ok": False, "error": "OutcomeUnknown"}
        assert await call(frame) == unknown
        assert await call(frame) == unknown  # the retry answers from the journal
        assert len(rig.a.sent(op)) == 1  # the browser ran the command once
        assert (await call({**frame, "request_id": "r", "op": op, "lease": "x"})) == {
            "ok": False,
            "error": "Conflict",
        }
        assert len(rig.a.sent(op)) == 1
        grant = rig.broker.grants[ack["grant"]]
        assert grant.state == "active" and list(grant.sessions[0].journal) == ["r"]
        assert not rig.broker.claims  # the lease drops, as the fate table says

    run(short, body)


@pytest.mark.parametrize("worker_error", ["BrowserDisconnected", "OutcomeUnknown"])
def test_worker_browser_disconnected_on_a_read_is_relayed_and_retry_runs_again(
    short, clock, worker_error
):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.refuse["page_read"] = {"error": worker_error, "text": PAGE_TEXT}
        disconnected = {"ok": False, "error": "BrowserDisconnected"}
        assert await call({"op": "page_read", "lease": lease}) == disconnected
        assert rig.broker.grants[ack["grant"]].state == "active"
        rig.a.refuse["page_tabs"] = {"error": worker_error}
        assert await call({"op": "tabs"}) == disconnected
        assert await call({"op": "tabs"}) == disconnected
        assert len(rig.a.sent("page_tabs")) == 2  # a read is safe to run again
        del rig.a.refuse["page_tabs"]
        assert (await call({"op": "tabs"}))["ok"]
        assert not rig.broker.grants[ack["grant"]].sessions[0].journal

    run(short, body)


def test_a_failing_indicator_clear_cannot_stop_a_revoke_all(short, clock, capsys):
    async def body(rig):
        acks = set()
        for tab in (7, 9):
            ack, call = await session(rig)
            assert (await call({"op": "select", "tab": tab}))["ok"]
            acks.add(ack["grant"])

        async def broken(_claim):
            raise RuntimeError("indicator")

        rig.broker.clear = broken
        assert await rig.operator(op="revoke_grants") == {"ok": True, "revoked": 1}
        await settle(rig)
        assert not rig.broker.grants and not rig.broker.claims
        sent = [(f["grant"], f["reason"]) for f in rig.a.sent("grant_end")]
        assert sorted(sent) == sorted((g, "revoked") for g in acks)
        assert rig.broker.sweeper is None
        assert "indicator" in capsys.readouterr().err

    run(short, body)


def test_a_sweep_that_raises_still_rearms_the_deadline(short, clock, capsys):
    async def body(rig):
        minted, _ = await session(rig)
        await rig.a.leave()  # arms the reconnect-grace deadline
        real = rig.broker.expire

        async def broken(_now):
            raise RuntimeError("sweep")

        rig.broker.expire = broken
        before = rig.broker.sweeper
        rig.broker.sweep()
        await settle(rig)
        assert rig.broker.sweeper not in (None, before)  # the timer is re-armed
        assert not rig.broker.sweeper.cancelled()
        assert "sweep" in capsys.readouterr().err
        rig.broker.expire = real
        clock[0] += codes.RECONNECT_GRACE
        rig.broker.sweep()
        await settle(rig)
        assert minted["grant"] not in rig.broker.grants
        assert rig.outcomes("grant_end") == ["browser_gone"]

    run(short, body)


def test_the_deadline_callback_never_runs_after_close(short, clock):
    async def body(rig):
        await session(rig)
        await rig.a.leave()  # arms the reconnect-grace deadline
        broker = rig.broker
        assert broker.sweeper is not None
        broker.sweep()  # a sweep task in flight when the broker closes
        await broker.close()
        assert broker.sweeper is None and not broker.tasks
        broker.arm()
        broker.sweep()
        assert broker.sweeper is None and not broker.tasks
        assert len(broker.grants) == 1  # nothing ran against torn-down state

    run(short, body)


def test_page_upload_relays_files_and_journals_only_a_digest(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = mutating_frame("page_upload", lease, "u")
        assert await call(frame) == {"ok": True, "tab": 7, "document": "d7-1"}
        assert await call(frame) == {"ok": True, "tab": 7, "document": "d7-1"}
        sent = rig.a.sent("page_upload")
        assert len(sent) == 1 and sent[0]["files"] == [FILE] and sent[0]["ref"] == "1.0"
        other = {**FILE, "data": "Ynll"}
        assert (await call({**frame, "files": [other]}))["error"] == "Conflict"
        journal = rig.broker.grants[ack["grant"]].sessions[0].journal
        assert "aGVsbG8=" not in repr(journal)
        assert "aGVsbG8=" not in (rig.state / "audit.jsonl").read_text()
        assert rig.outcomes("operation")[-1] == "Conflict"

    run(short, body)


@pytest.mark.parametrize(
    "files",
    [
        [],
        [FILE] * 11,
        [{**FILE, "name": "../a"}],
        [{**FILE, "name": ""}],
        [{**FILE, "data": "not base64!"}],
        [{**FILE, "extra": 1}],
        [{**FILE, "data": "A" * (4 * 175000)}],
        "a.txt",
    ],
)
def test_malformed_upload_files_are_refused_before_the_browser(short, clock, files):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = {**mutating_frame("page_upload", lease), "files": files}
        assert await call(frame) == {"ok": False, "error": "InvalidRequest"}
        assert rig.a.sent("page_upload") == []

    run(short, body)


def test_page_downloads_reports_the_workers_list_in_shape(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        listed = await call({"op": "page_downloads", "lease": lease})
        assert listed == {
            "ok": True,
            "tab": 7,
            "origin": ORIGIN,
            "downloads": [DOWNLOAD],
        }
        rig.a.refuse["page_downloads"] = {"origin": ORIGIN, "downloads": [{"id": 1}]}
        bad = await call({"op": "page_downloads", "lease": lease})
        assert bad == {"ok": False, "error": "BrowserRefused"}

    run(short, body)


def test_a_browser_drop_during_an_in_flight_op_leaves_the_grant_in_grace(
    short, clock, monkeypatch
):
    monkeypatch.setattr(protocol, "PAGE_WAIT", 5.0)

    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        frame = {"op": "page_click", "lease": lease, "request_id": "r", "ref": "1.0"}
        rig.a.hold["page_click"] = asyncio.Event()
        pending = asyncio.ensure_future(call(frame))
        for _ in range(200):
            if rig.a.sent("page_click"):
                break
            await asyncio.sleep(0.01)
        await rig.a.leave()  # the browser drops while the click is in flight
        reply = await pending
        assert reply["ok"] is False and reply["error"] != "GrantEnded"
        grant = rig.broker.grants[ack["grant"]]
        assert grant.state == "active" and grant.grace_until is not None

    run(short, body)


def test_a_late_outcome_unknown_from_the_old_peer_keeps_a_rebound_grant(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        grant = rig.broker.grants[ack["grant"]]
        old = rig.broker.peers[rig.alpha["profile"]]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair()  # same incarnation: rebound
        assert grant.generation != old.generation and grant.grace_until is None

        async def late(*_a, **_k):
            raise protocol.Refusal("OutcomeUnknown")

        old.page = late  # the old peer's reply arrives after the rebind
        with pytest.raises(protocol.Refusal):
            await rig.broker.send(grant, None, old, "page_click")
        assert grant.state == "active"
        assert (await call({"op": "tabs"}))["ok"]

    run(short, body)


def test_dialog_policy_is_forwarded_and_the_dialog_relayed(short, clock):
    async def body(rig):
        _, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        click = {"op": "page_click", "lease": lease, "ref": "1.0"}
        assert (await call({**click, "request_id": "a"}))["ok"]
        assert rig.a.sent("page_click")[-1]["dialog"] == "dismiss"
        seen = {"type": "confirm", "message": "Delete it?", "accepted": True}
        rig.a.dialog = seen
        reply = await call({**click, "request_id": "b", "dialog": "accept"})
        assert reply == {"ok": True, "tab": 7, "document": "d7-1", "dialog": seen}
        assert rig.a.sent("page_click")[-1]["dialog"] == "accept"
        bad = await call({**click, "request_id": "c", "dialog": "maybe"})
        assert bad == {"ok": False, "error": "InvalidRequest"}
        rig.a.dialog = {**seen, "type": "devtools"}
        forged = await call({**click, "request_id": "d"})
        assert forged["ok"] is False and forged["error"] == "BrowserRefused"
        rig.a.dialog = None
        rig.a.refuse["page_click"] = {"error": "Obscured", "tab": 7}
        covered = await call({**click, "request_id": "e"})
        assert covered == {"ok": False, "error": "Obscured", "tab": 7}
        assert len(rig.broker.claims) == 1  # Obscured keeps the lease
        # Option A: typing stays synthetic, so page_type takes no dialog policy.
        typed = {"op": "page_type", "lease": lease, "ref": "1.0", "text": "x"}
        refused = await call({**typed, "request_id": "f", "dialog": "accept"})
        assert refused == {"ok": False, "error": "InvalidRequest"}

    run(short, body)

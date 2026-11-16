"""Activation codes (#11239) exercised through the real broker sockets and a fake extension peer."""

import asyncio
import contextlib
import json
import re
import secrets
import shutil
import stat
import tempfile
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import pytest

from trusty_browser import audit, codes, enrollment, protocol, service

URL = "https://qa.test/app/page?q=private-query"
PAGE_TEXT = "page-secret-text"
ORIGIN = "https://qa.test"
TITLE = "private-title"
# #11289: one entry of a worker page_downloads result.
DOWNLOAD = {
    "id": 3,
    "path": "/Users/qa/Downloads/report.txt",
    "url": ORIGIN + "/download.txt",
    "mime": "text/plain",
    "state": "complete",
    "bytes": 32,
    "total": 32,
    "error": None,
    "started": "2026-11-15T00:00:00.000Z",
}


@pytest.fixture
def short():
    # macOS caps Unix socket paths near 104 bytes; pytest's tmp_path is too long.
    root = Path(tempfile.mkdtemp(dir="/tmp"))
    yield root / "s"
    shutil.rmtree(root)


@pytest.fixture
def clock(monkeypatch):
    now = [1000.0]
    monkeypatch.setattr(codes, "clock", lambda: now[0])
    # Keep the test registry off the login Keychain: sealing is not under test here.
    monkeypatch.setattr(enrollment, "seal", lambda _s, _p, value: value)
    monkeypatch.setattr(enrollment, "unseal", lambda _s, _p, value: value)
    monkeypatch.setattr(service, "verify_chrome_host", lambda *_: None)
    return now


class FakeBrowser:
    """Answers broker commands as the extension would, for one paired profile.

    Workflow commands (``inspect``, ``snapshot``, ``click``, ``lease_*``) answer
    from ``tab``; page commands answer from the ``tabs`` model, and
    ``refuse[op]`` forces a worker refusal frame for one op.
    """

    def __init__(self, rig, row):
        self.rig, self.row, self.tab = rig, row, 7
        self.missing: set[int] = set()
        self.seen: list[tuple[str, object]] = []
        self.frames: list[dict] = []
        self.hold: dict[str, asyncio.Event] = {}
        self.arrived: dict[str, asyncio.Event] = {}
        self.refuse: dict[str, dict] = {}
        self.done: list[tuple] = []
        self.ack: dict = {}
        self.tabs = {
            7: {
                "window": 1,
                "index": 0,
                "active": True,
                "access": True,
                "origin": ORIGIN,
            },
            8: {
                "window": 1,
                "index": 1,
                "active": False,
                "access": False,
                "origin": None,
            },
            9: {
                "window": 2,
                "index": 0,
                "active": True,
                "access": True,
                "origin": ORIGIN,
            },
        }
        for tab, item in self.tabs.items():
            item.update(title=TITLE if item["access"] else None, document=f"d{tab}-1")

    def add(self, tab, access=True):
        self.tabs[tab] = {"window": 3, "index": tab, "active": False, "access": access}
        self.tabs[tab].update(
            origin=ORIGIN if access else None,
            title=TITLE if access else None,
            document=f"d{tab}-1",
        )

    async def pair(self, life="boot"):
        hello = {
            "type": "hello",
            "profile": self.row["profile"],
            "email": self.row["email"],
            "incarnation": life,
            "nonce": secrets.token_hex(32),
        }
        self.reader, self.writer = await asyncio.open_unix_connection(
            self.rig.state / "service.sock"
        )
        await service.write_frame(self.writer, hello)
        challenge = await service.read_frame(self.reader)
        proof = service.pairing_proof(
            self.row["key"], "extension", hello, challenge["nonce"]
        )
        await service.write_frame(self.writer, {"type": "proof", "proof": proof})
        self.ack = await service.read_frame(self.reader)
        assert self.ack["ok"], self.ack
        self.task = asyncio.create_task(self.loop())
        return self

    async def loop(self):
        with contextlib.suppress(
            EOFError, asyncio.IncompleteReadError, ConnectionError
        ):
            while True:
                req = await service.read_frame(self.reader)
                asyncio.create_task(self.answer(req))

    def page(self, req):
        """The page-command result, or a refusal frame (``error`` key)."""
        op, tab = req["op"], req.get("tab")
        if op in self.refuse:
            return self.refuse[op]
        if op == "grant_end":
            return "ended"
        if op == "page_tabs":
            keys = ("window", "index", "active", "access", "origin", "title")
            entries = [
                {"tab": t, **{k: i[k] for k in keys}, "extra": "dropped"}
                for t, i in self.tabs.items()
            ]
            return {"tabs": entries}
        item = self.tabs.get(tab)
        if item is None:
            return {"error": "TabNotFound", "tab": tab}
        seen = {k: item[k] for k in ("access", "document", "origin", "title")}
        if not item["access"]:
            seen.update(document=None, origin=None, title=None)
        if op == "page_tab":
            return seen
        if op == "page_navigate":
            target = urlsplit(req["url"])
            item["origin"] = f"{target.scheme}://{target.netloc}"
            item["access"] = item["origin"] == ORIGIN
            item["title"] = TITLE if item["access"] else None
            item["document"] = f"d{tab}-{secrets.token_hex(2)}"
            landed = self.page({**req, "op": "page_tab"})
            del landed["title"]
            return {**landed, "settled": True}
        if not item["access"]:
            return {"error": "PermissionNeeded", "tab": tab, "origin": None}
        if op == "page_read":
            return {
                "document": item["document"],
                "origin": ORIGIN,
                "url": ORIGIN + "/app/page?q=private-query",
                "title": TITLE,
                "text": PAGE_TEXT,
                "elements": [
                    {"ref": "1.0", "tag": "input", "role": "textbox", "name": "Search"}
                ],
                "truncated": False,
            }
        if op == "page_downloads":
            return {"origin": ORIGIN, "downloads": [dict(DOWNLOAD)]}
        if req.get("document") != item["document"]:
            # ``secret`` is a field the broker must drop before relaying.
            return {
                "error": "DocumentChanged",
                "tab": tab,
                "access": True,
                "document": item["document"],
                "secret": PAGE_TEXT,
            }
        if op == "page_scroll":
            self.done.append((op, req["to"], req["ref"], req["wait_ms"]))
            view = {"scroll_y": 540, "scroll_height": 4000, "viewport_height": 600}
            return {"document": item["document"], **view, "grew": True, "at_end": False}
        self.done.append((op, req.get("ref"), req.get("text")))
        return {"document": item["document"]}

    async def answer(self, req):
        op = req["op"]
        self.seen.append((op, req.get("tab")))
        self.frames.append(req)
        if op in self.arrived:
            self.arrived[op].set()
        if op in self.hold:
            await self.hold[op].wait()
        if op == "grant_end" or op.startswith("page_"):
            result = self.page(req)
            if isinstance(result, dict) and "error" in result:
                reply = dict(id=req["id"], ok=False, **result)
            else:
                reply = {
                    "id": req["id"],
                    "ok": True,
                    "email": req["email"],
                    "result": result,
                }
                if "tab" in req:
                    reply["tab"] = req["tab"]
        else:
            tab = req.get("tab", self.tab)
            reply = {
                "id": req["id"],
                "ok": True,
                "email": req["email"],
                "url": req["url"],
                "account": req["account"],
                "tab": tab,
                "result": {"field": PAGE_TEXT},
            }
            if tab is None or tab in self.missing:
                reply = {"id": req["id"], "ok": False, "error": "TabNotFound"}
        with contextlib.suppress(ConnectionError, RuntimeError):
            await service.write_frame(self.writer, reply)

    def sent(self, *ops):
        """Frames of the given ops the broker sent, in order."""
        return [f for f in self.frames if f["op"] in ops]

    async def leave(self):
        self.writer.close()
        await self.task
        for _ in range(200):
            if self.row["profile"] not in self.rig.broker.peers:
                return
            await asyncio.sleep(0)


class Rig:
    def __init__(self, state):
        self.state = state

    def seed(self, name, url=URL):
        key = secrets.token_hex(32)
        row = {
            "op": "enroll",
            "profile": str(uuid.uuid4()),
            "email": name + "@example.test",
            "workflow": name,
            "url": url,
            "account": name,
            "key": key,
        }
        with contextlib.closing(enrollment.database(self.state)) as db:
            enrollment.register(db, self.state, row)
        ticket = enrollment.read_credential(
            self.state / "workflows" / (name + ".json"), name
        )
        return {**row, "ticket": ticket}

    async def connect(self, sock, frame):
        reader, writer = await asyncio.open_unix_connection(self.state / sock)
        await service.write_frame(writer, frame)
        return reader, writer, await service.read_frame(reader)

    async def once(self, sock, frame):
        _, writer, reply = await self.connect(sock, frame)
        writer.close()
        return reply

    def credential(self, name):
        """A fresh pairing credential, as the worker generates one."""
        return {
            "profile": str(uuid.uuid4()),
            "email": name + "@example.test",
            "key": secrets.token_hex(32),
        }

    async def pair(self, cred, op="pair"):
        return await self.once("enrollment.sock", {"op": op, **cred})

    async def popup(self, cred, op, **fields):
        triple = {k: cred[k] for k in ("profile", "email", "key")}
        return await self.once("enrollment.sock", {"op": op, **triple, **fields})

    async def mint(self, row, scope="profile", tab=None):
        names = enrollment.FIELDS if "workflow" in row else ("profile", "email", "key")
        frame = {k: row[k] for k in names}
        frame.update(op="mint", scope=scope)
        if tab is not None:
            frame["tab"] = tab
        return await self.once("enrollment.sock", frame)

    async def redeem(self, code):
        reader, writer, reply = await self.connect(
            "agent.sock", {"op": "redeem", "code": code}
        )

        async def call(frame):
            await service.write_frame(writer, frame)
            return await service.read_frame(reader)

        self.writers.append(writer)
        return reply, call

    async def open(self, row):
        reader, writer, reply = await self.connect(
            "agent.sock",
            {"op": "open", "workflow": row["workflow"], "ticket": row["ticket"]},
        )
        self.writers.append(writer)

        async def call(frame):
            await service.write_frame(writer, frame)
            return await service.read_frame(reader)

        return reply, call

    async def operator(self, **frame):
        return await self.once("operator.sock", frame)

    def audit(self):
        path = self.state / "audit.jsonl"
        return (
            [json.loads(line) for line in path.read_text().splitlines()]
            if path.exists()
            else []
        )

    def outcomes(self, event):
        return [line.get("outcome") for line in self.audit() if line["event"] == event]


def run(state, body):
    async def main():
        rig = Rig(state)
        rig.writers = []
        rig.alpha = rig.seed("alpha")
        rig.beta = rig.seed("beta")
        async with service.Broker(state) as broker:
            rig.broker = broker
            rig.a = await FakeBrowser(rig, rig.alpha).pair()
            rig.b = await FakeBrowser(rig, rig.beta).pair()
            try:
                await body(rig)
            finally:
                for writer in rig.writers + [rig.a.writer, rig.b.writer]:
                    writer.close()

    asyncio.run(main())


async def gone(call, frame=None):
    """Assert the session's next call reports GrantEnded and the broker then closes it."""
    reply = await call(frame or {"op": "claim"})
    with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
        await call({"op": "claim"})
    return reply


def test_mint_reply_carries_a_pasteable_code_scope_and_expiry(short, clock):
    async def body(rig):
        minted = await rig.mint(rig.alpha, "tab", 7)
        assert set(minted) == {
            "ok",
            "code",
            "grant",
            "scope",
            "profile",
            "tab",
            "expires_at",
        }
        assert re.fullmatch(
            r"tb-([0-9A-HJKMNP-TV-Z]{6}-){3}[0-9A-HJKMNP-TV-Z]{6}", minted["code"]
        )
        assert minted["scope"] == "tab" and minted["tab"] == 7
        assert minted["profile"] == rig.alpha["profile"]
        plain = await rig.mint(rig.alpha)
        assert plain["scope"] == "profile" and "tab" not in plain
        assert plain["code"] != minted["code"]
        assert stat.S_IMODE((rig.state / "audit.jsonl").stat().st_mode) == 0o600
        reply, _call = await rig.redeem(minted["code"].lower().replace("1", "l"))
        assert reply["ok"] and reply["grant"] == minted["grant"] and reply["tab"] == 7

    run(short, body)


def test_wrong_replayed_and_expired_codes_refuse_alike_but_audit_apart(short, clock):
    async def body(rig):
        wrong, _ = await rig.redeem(codes.new_code())
        used = (await rig.mint(rig.alpha))["code"]
        first, _ = await rig.redeem(used)
        assert first["ok"]
        replay, _ = await rig.redeem(used)
        late = (await rig.mint(rig.alpha))["code"]
        clock[0] += codes.CODE_TTL
        expired, _ = await rig.redeem(late)
        assert wrong == replay == expired == {"ok": False, "error": "CodeRefused"}
        assert rig.outcomes("redeem") == ["wrong", "ok", "replay", "expired"]

    run(short, body)


def test_two_concurrent_redeems_have_exactly_one_winner(short, clock):
    async def body(rig):
        code = (await rig.mint(rig.alpha, "tab", 7))["code"]
        rig.a.hold["page_tab"], rig.a.arrived["page_tab"] = (
            asyncio.Event(),
            asyncio.Event(),
        )
        first = asyncio.create_task(rig.redeem(code))
        await rig.a.arrived["page_tab"].wait()
        loser, _ = await rig.redeem(code)
        rig.a.hold["page_tab"].set()
        winner, _ = await first
        assert winner["ok"] and loser == {"ok": False, "error": "CodeRefused"}
        profile = (await rig.mint(rig.alpha))["code"]
        replies = await asyncio.gather(*(rig.redeem(profile) for _ in range(4)))
        assert sorted(r["ok"] for r, _ in replies) == [False, False, False, True]
        assert "contended" in rig.outcomes("redeem")

    run(short, body)


def test_tab_closed_before_redeem_is_refused_and_never_rebound(short, clock):
    async def body(rig):
        closed = (await rig.mint(rig.alpha, "tab", 7))["code"]
        unknown = (await rig.mint(rig.alpha, "tab", 9))["code"]
        saved = rig.a.tabs.pop(7)
        assert (await rig.redeem(closed))[0] == {"ok": False, "error": "TabClosed"}
        rig.a.refuse["page_tab"] = {"error": "UnknownGrant"}
        assert (await rig.redeem(unknown))[0] == {"ok": False, "error": "TabClosed"}
        del rig.a.refuse["page_tab"]
        rig.a.tabs[7] = saved
        assert (await rig.redeem(closed))[0]["error"] == "CodeRefused"
        restarted = (await rig.mint(rig.alpha, "tab", 8))["code"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
        assert (await rig.redeem(restarted))[0]["error"] == "TabClosed"
        assert ("page_tab", 8) not in rig.a.seen
        assert rig.outcomes("redeem").count("tab_closed") == 3

    run(short, body)


def test_tab_closed_after_redeem_ends_the_grant(short, clock):
    async def body(rig):
        _reply, call = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        rig.a.tabs.pop(7)
        ended = await gone(call, {"op": "page_read", "lease": lease})
        assert ended == {"ok": False, "error": "GrantEnded", "reason": "tab_closed"}
        assert not rig.broker.claims and not rig.broker.grants

    run(short, body)


def test_lockout_backs_off_and_recovers(short, clock):
    async def body(rig):
        for expected in (60, 120, 240, 480, 900, 900):
            for _ in range(codes.FAIL_LIMIT):
                assert (await rig.redeem(codes.new_code()))[0]["error"] == "CodeRefused"
            good = (await rig.mint(rig.alpha))["code"]
            locked, _ = await rig.redeem(good)
            assert locked == {
                "ok": False,
                "error": "RedeemLocked",
                "retry_after_s": expected,
            }
            clock[0] += expected - 1
            assert (await rig.redeem(good))[0]["retry_after_s"] == 1
            clock[0] += 1
            await rig.operator(op="revoke_grants")
        good = (await rig.mint(rig.alpha))["code"]
        assert (await rig.redeem(good))[0]["ok"]
        for _ in range(codes.FAIL_LIMIT):
            await rig.redeem(codes.new_code())
        assert (await rig.redeem(good))[0]["retry_after_s"] == 60
        assert rig.outcomes("lock") == [
            "60s",
            "120s",
            "240s",
            "480s",
            "900s",
            "900s",
            "60s",
        ]

    run(short, body)


def test_revoke_one_and_all_refuse_the_next_call(short, clock):
    async def body(rig):
        one = await rig.mint(rig.alpha)
        two = await rig.mint(rig.alpha, "tab", 7)
        pending = await rig.mint(rig.beta)
        _, first = await rig.redeem(one["code"])
        _, second = await rig.redeem(two["code"])
        listed = await rig.operator(op="grants")
        states = {g["grant"]: g["state"] for g in listed["grants"]}
        assert states == {
            one["grant"]: "active",
            two["grant"]: "active",
            pending["grant"]: "code",
        }
        assert not any(c["code"] in json.dumps(listed) for c in (one, two, pending))
        assert await rig.operator(op="revoke_grant", grant=one["grant"]) == {
            "ok": True,
            "revoked": 1,
        }
        assert (await gone(first))["reason"] == "revoked"
        assert (await second({"op": "tabs"}))["ok"]
        assert await rig.operator(op="revoke_grant", grant="g-unknown") == {
            "ok": False,
            "error": "UnknownGrant",
        }
        assert (await rig.operator(op="revoke_grants"))["revoked"] == 2
        assert (await gone(second))["reason"] == "revoked"
        assert (await rig.redeem(pending["code"]))[0]["error"] == "CodeRefused"
        assert not rig.broker.claims
        assert (await rig.redeem((await rig.mint(rig.alpha))["code"]))[0]["ok"]

    run(short, body)


def test_revoke_during_an_in_flight_click_answers_outcome_unknown(short, clock):
    async def body(rig):
        minted = await rig.mint(rig.alpha)
        _, call = await rig.redeem(minted["code"])
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        hold, arrived = asyncio.Event(), asyncio.Event()
        rig.a.hold["page_click"], rig.a.arrived["page_click"] = hold, arrived
        click = asyncio.create_task(
            call({"op": "page_click", "lease": lease, "request_id": "c", "ref": "1.0"})
        )
        await arrived.wait()
        await rig.operator(op="revoke_grant", grant=minted["grant"])
        hold.set()
        assert await click == {"ok": False, "error": "OutcomeUnknown"}
        assert (await gone(call))["error"] == "GrantEnded"

    run(short, body)


def test_tab_cap_refuses_mint_and_redeem_of_a_second_grant_on_a_tab(short, clock):
    async def body(rig):
        early = [(await rig.mint(rig.alpha, "tab", 7))["code"] for _ in range(2)]
        assert (await rig.redeem(early[0]))[0]["ok"]
        assert (await rig.mint(rig.alpha, "tab", 7)) == {
            "ok": False,
            "error": "GrantLimit",
            "limit": "tab",
        }
        assert (await rig.redeem(early[1]))[0] == {
            "ok": False,
            "error": "GrantLimit",
            "limit": "tab",
        }
        # The refused code stays redeemable once the tab's grant is gone.
        await rig.operator(op="revoke_grants")
        assert (await rig.mint(rig.alpha, "tab", 7))["ok"]

    run(short, body)


def test_more_than_three_grants_on_distinct_tabs_and_profiles_coexist(short, clock):
    async def body(rig):
        calls = []
        for tab in range(1, 6):
            rig.a.add(tab)
            ack, call = await rig.redeem(
                (await rig.mint(rig.alpha, "tab", tab))["code"]
            )
            assert ack["ok"], ack
            calls.append(call)
        for row in (rig.alpha, rig.beta, rig.beta):
            ack, call = await rig.redeem((await rig.mint(row))["code"])
            assert ack["ok"], ack
            calls.append(call)
        listed = (await rig.operator(op="grants"))["grants"]
        assert len(listed) == 8
        assert rig.outcomes("redeem") == ["ok"] * 8
        for tab, call in enumerate(calls[:5], start=1):
            assert (await call({"op": "select", "tab": tab}))["tab"] == tab
        assert len(rig.broker.claims) == 5
        assert (await rig.mint(rig.alpha, "tab", 3))["limit"] == "tab"

    run(short, body)


def test_mint_is_refused_on_every_socket_but_enrollment(short, clock):
    async def body(rig):
        frame = {k: rig.alpha[k] for k in enrollment.FIELDS}
        frame.update(op="mint", scope="profile")
        assert (await rig.once("agent.sock", frame))["error"] == "InvalidRequest"
        assert (await rig.once("service.sock", frame))["error"] == "InvalidRequest"
        assert (await rig.once("operator.sock", frame))["error"] == "RevokeFailed"
        assert not rig.broker.codes and rig.outcomes("mint") == []

    run(short, body)


def test_mint_is_refused_for_an_unenrolled_profile(short, clock):
    async def body(rig):
        frame = {k: rig.alpha[k] for k in enrollment.FIELDS}
        frame.update(op="mint", scope="profile")
        assert (
            await rig.once("enrollment.sock", {**frame, "key": secrets.token_hex(32)})
        )["error"] == "NotEnrolled"
        assert (
            await rig.once("enrollment.sock", {**frame, "profile": str(uuid.uuid4())})
        )["error"] == "NotEnrolled"
        assert (await rig.once("enrollment.sock", {**frame, "scope": "window"}))[
            "error"
        ] == "InvalidMint"
        assert (await rig.once("enrollment.sock", {**frame, "tab": 7}))[
            "error"
        ] == "InvalidMint"
        await rig.broker.enrollment_request(
            {k: rig.alpha[k] for k in enrollment.FIELDS} | {"op": "unenroll"}
        )
        assert (await rig.once("enrollment.sock", frame))["error"] == "NotEnrolled"
        assert not rig.broker.codes

    run(short, body)


def test_unenroll_ends_codes_and_grants_for_the_profile(short, clock):
    async def body(rig):
        _, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        waiting = (await rig.mint(rig.alpha))["code"]
        frame = {k: rig.alpha[k] for k in enrollment.FIELDS}
        await rig.broker.enrollment_request({**frame, "op": "unenroll"})
        assert (await rig.redeem(waiting))[0]["error"] == "CodeRefused"
        with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
            await call({"op": "claim"})
        assert "unenrolled" in rig.outcomes("grant_end")

    run(short, body)


# #11239 owner ruling: longer than the retired 30-minute idle timeout.
LONG_IDLE = 35 * 60 + 1


def test_browser_loss_and_socket_close_end_the_grant(short, clock):
    async def body(rig):
        _, lost = await rig.redeem((await rig.mint(rig.alpha))["code"])
        assert (await lost({"op": "tabs"}))["ok"]
        await rig.a.leave()
        clock[0] += codes.RECONNECT_GRACE
        assert (await gone(lost))["reason"] == "browser_gone"
        _reply, _closing = await rig.redeem((await rig.mint(rig.beta))["code"])
        rig.writers[-1].close()
        for _ in range(200):
            if not rig.broker.grants:
                break
            await asyncio.sleep(0)
        assert not rig.broker.grants
        assert rig.outcomes("grant_end") == ["browser_gone", "closed"]

    run(short, body)


def test_idle_profile_grants_stay_listed_past_the_old_timeout(short, clock):
    async def body(rig):
        calls = []
        for _ in range(3):
            ack, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
            assert ack["ok"] and "idle_timeout_s" not in ack
            calls.append(call)
        clock[0] += LONG_IDLE
        listed = (await rig.operator(op="grants"))["grants"]
        assert [g["state"] for g in listed] == ["active"] * 3
        assert rig.broker.sweeper is None  # no idle deadline is armed
        for call in calls:
            assert (await call({"op": "tabs"}))["ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_an_idle_tab_grant_keeps_its_tab_cap_past_the_old_timeout(short, clock):
    async def body(rig):
        _, held = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        assert (await held({"op": "select", "tab": 7}))["ok"]
        clock[0] += LONG_IDLE
        assert (await rig.mint(rig.alpha, "tab", 7))["limit"] == "tab"
        assert (await held({"op": "select", "tab": 7}))["ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_a_grant_survives_a_worker_reconnect_within_the_grace(short, clock):
    async def body(rig):
        ack, whole = await rig.redeem((await rig.mint(rig.alpha))["code"])
        tab_ack, tab = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        await rig.a.leave()  # Chrome stopped the idle service worker
        assert await whole({"op": "tabs"}) == {
            "ok": False,
            "error": "BrowserDisconnected",
        }
        clock[0] += codes.RECONNECT_GRACE - 1
        rig.a = await FakeBrowser(rig, rig.alpha).pair()  # same incarnation
        assert {ack["grant"], tab_ack["grant"]} <= set(rig.a.ack["live"])
        assert (await whole({"op": "tabs"}))["ok"]
        assert (await tab({"op": "select", "tab": 7}))["ok"]
        clock[0] += LONG_IDLE
        assert len((await rig.operator(op="grants"))["grants"]) == 2
        assert (await whole({"op": "tabs"}))["ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_a_grant_outlives_lease_expiry_and_audits_create_reuse_and_end(short, clock):
    async def body(rig):
        ack, call = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        assert (await call({"op": "select", "tab": 7}))["ok"]
        for claim in rig.broker.claims.values():
            claim.deadline = 0.0  # the lease expired
        clock[0] += LONG_IDLE
        assert (await call({"op": "select", "tab": 7}))["ok"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair()
        assert ack["grant"] in rig.a.ack["live"]
        assert (await call({"op": "select", "tab": 7}))["ok"]
        assert rig.outcomes("redeem") == ["ok"]
        assert rig.outcomes("grant_reuse") == ["ok"]
        await rig.operator(op="revoke_grant", grant=ack["grant"])
        assert rig.outcomes("grant_end") == ["revoked"]

    run(short, body)


def test_revoke_inside_the_reconnect_grace_ends_the_grant_at_once(short, clock):
    async def body(rig):
        ack, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        await rig.a.leave()
        assert rig.broker.grants[ack["grant"]].grace_until is not None
        await rig.operator(op="revoke_grant", grant=ack["grant"])
        assert ack["grant"] not in rig.broker.grants
        rig.a = await FakeBrowser(rig, rig.alpha).pair()  # same incarnation
        assert ack["grant"] not in rig.a.ack["live"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "revoked"
        assert rig.outcomes("grant_reuse") == []

    run(short, body)


def test_a_grant_without_an_incarnation_does_not_persist(short, clock):
    async def body(rig):
        ack, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        rig.broker.grants[ack["grant"]].incarnation = None  # lookup failed
        await rig.a.leave()
        assert ack["grant"] not in rig.broker.grants  # no grace, no fail-open
        rig.a = await FakeBrowser(rig, rig.alpha).pair()
        assert ack["grant"] not in rig.a.ack["live"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"
        assert rig.outcomes("grant_end") == ["browser_gone"]

    run(short, body)


def test_a_rebind_the_audit_cannot_record_ends_the_grant(short, clock, monkeypatch):
    async def body(rig):
        ack, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        await rig.a.leave()
        real = rig.broker.audit.write

        def broken(event, **fields):
            if event == "grant_reuse":
                raise OSError("disk full")
            return real(event, **fields)

        monkeypatch.setattr(rig.broker.audit, "write", broken)
        rig.a = await FakeBrowser(rig, rig.alpha).pair()
        assert ack["grant"] not in rig.a.ack["live"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"

    run(short, body)


def test_a_browser_restart_ends_the_grant_at_reconnect(short, clock):
    async def body(rig):
        ack, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
        assert ack["grant"] not in rig.a.ack["live"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"
        assert rig.outcomes("grant_end") == ["browser_gone"]

    run(short, body)


@pytest.mark.parametrize("life", [None, ""])
def test_a_hello_without_an_incarnation_gets_no_peer_so_no_mint_or_redeem(
    short, clock, life
):
    async def body(rig):
        await rig.a.leave()
        reader, writer = await asyncio.open_unix_connection(rig.state / "service.sock")
        hello = {
            "type": "hello",
            "profile": rig.alpha["profile"],
            "email": rig.alpha["email"],
            "incarnation": life,
            "nonce": secrets.token_hex(32),
        }
        await service.write_frame(writer, hello)
        assert await service.read_frame(reader) == {
            "ok": False,
            "error": "InvalidRequest",
        }
        writer.close()
        assert rig.alpha["profile"] not in rig.broker.peers
        tab = await rig.mint(rig.alpha, "tab", 7)
        assert tab == {"ok": False, "error": "BrowserDisconnected"}
        code = (await rig.mint(rig.alpha))["code"]
        refused, _ = await rig.redeem(code)
        assert refused == {"ok": False, "error": "BrowserDisconnected"}
        assert not [g for g in rig.broker.grants.values() if g.state == "active"]

    run(short, body)


def test_a_grant_whose_worker_never_returns_ends_after_the_grace(short, clock):
    async def body(rig):
        _, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        await rig.a.leave()
        assert rig.broker.sweeper is not None  # the grace deadline is armed
        clock[0] += codes.RECONNECT_GRACE - 1
        assert len((await rig.operator(op="grants"))["grants"]) == 1
        clock[0] += 1
        assert (await rig.operator(op="grants")) == {"ok": True, "grants": []}
        assert rig.outcomes("grant_end") == ["browser_gone"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"

    run(short, body)


def test_broker_restart_leaves_no_code_or_grant(short, clock):
    async def body(rig):
        code = (await rig.mint(rig.alpha))["code"]
        await rig.redeem((await rig.mint(rig.alpha))["code"])
        rig.code = code

    holder = {}

    def keep(rig):
        holder["rig"] = rig
        return body(rig)

    run(short, keep)
    rig = holder["rig"]

    async def again():
        async with service.Broker(rig.state) as broker:
            assert not broker.grants and not broker.codes
            reply = await rig.once("agent.sock", {"op": "redeem", "code": rig.code})
            assert reply == {"ok": False, "error": "CodeRefused"}
            assert (await rig.once("operator.sock", {"op": "grants"})) == {
                "ok": True,
                "grants": [],
            }

    asyncio.run(again())
    for path in rig.state.rglob("*"):
        if path.is_file():
            assert rig.code.encode() not in path.read_bytes()


def test_audit_lines_carry_no_secret_path_or_page_text(short, clock):
    async def body(rig):
        minted = await rig.mint(rig.alpha, "tab", 7)
        _, call = await rig.redeem(minted["code"])
        claim = await call({"op": "select", "tab": 7})
        lease = claim["lease"]
        read = await call({"op": "page_read", "lease": lease})
        assert read["text"] == PAGE_TEXT and read["title"] == TITLE
        ref, typed = "424242.987654", "typed-secret-text"
        frames = [
            {
                "op": "page_type",
                "lease": lease,
                "request_id": "t",
                "ref": ref,
                "text": typed,
            },
            {"op": "page_click", "lease": lease, "request_id": "c", "ref": ref},
            {
                "op": "page_press",
                "lease": lease,
                "request_id": "p",
                "ref": ref,
                "key": "Enter",
                "modifiers": ["Shift"],
            },
            {
                "op": "page_navigate",
                "lease": lease,
                "request_id": "n",
                "url": URL + "#frag",
            },
            {"op": "release", "lease": lease},
        ]
        for frame in frames:
            assert (await call(frame))["ok"], frame
        await rig.redeem(minted["code"])
        await rig.operator(op="revoke_grants")
        text = (rig.state / "audit.jsonl").read_text()
        forbidden = [
            minted["code"],
            minted["code"].replace("-", "")[2:],
            codes.code_digest(minted["code"]),
            rig.alpha["ticket"],
            rig.alpha["key"],
            lease,
            PAGE_TEXT,
            TITLE,
            typed,
            ref,
            "Search",
            "/app/page",
            "private-query",
            "frag",
        ]
        assert [value for value in forbidden if value in text] == []
        lines = rig.audit()
        assert {k for line in lines for k in line} <= {"ts", *audit.FIELDS}
        assert {line["origin"] for line in lines if "origin" in line} == {"qa.test"}
        ops = [(e["op"], e["outcome"]) for e in lines if e["event"] == "operation"]
        assert ops == [
            (op, o)
            for op in (
                "select",
                "page_read",
                "page_type",
                "page_click",
                "page_press",
                "page_navigate",
                "release",
            )
            for o in ("admitted", "ok")
        ]
        results = [e for e in lines if e["event"] == "operation"][1::2]
        assert [e.get("tab") for e in results] == [7] * 7
        assert "Enter" not in text and "Shift" not in text  # #11286: no key is recorded
        assert stat.S_IMODE((rig.state / "audit.jsonl").stat().st_mode) == 0o600

    run(short, body)


def test_audit_write_failure_fails_mint_redeem_and_operations_closed(
    short, clock, monkeypatch
):
    async def body(rig):
        real = rig.broker.audit.write

        def broken(*_a, **_k):
            raise OSError("disk full")

        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await rig.mint(rig.alpha)) == {"ok": False, "error": "AuditUnavailable"}
        assert not rig.broker.codes
        monkeypatch.setattr(rig.broker.audit, "write", real)
        code = (await rig.mint(rig.alpha))["code"]
        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await rig.redeem(code))[0] == {"ok": False, "error": "AuditUnavailable"}
        monkeypatch.setattr(rig.broker.audit, "write", real)
        _, call = await rig.redeem(code)
        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await call({"op": "select", "tab": 7})) == {
            "ok": False,
            "error": "AuditUnavailable",
        }
        assert not rig.broker.claims and rig.a.sent("page_tab") == []

    run(short, body)


def test_audit_log_rotates_and_keeps_two(tmp_path, monkeypatch):
    monkeypatch.setattr(audit, "ROTATE_BYTES", 200)
    tmp_path.chmod(0o700)
    log = audit.Audit(tmp_path)
    for index in range(40):
        log.write("mint", grant=f"g-{index}", outcome="ok")
    names = sorted(p.name for p in tmp_path.iterdir())
    assert names == ["audit.jsonl", "audit.jsonl.1", "audit.jsonl.2"]
    longest = max(
        len(line)
        for p in tmp_path.iterdir()
        for line in p.read_bytes().splitlines(True)
    )
    assert all(
        p.stat().st_size < audit.ROTATE_BYTES + longest for p in tmp_path.iterdir()
    )


def test_open_path_works_beside_a_code_session(short, clock):
    async def body(rig):
        opened, workflow = await rig.open(rig.alpha)
        assert opened == {"ok": True}
        _, coded = await rig.redeem((await rig.mint(rig.alpha))["code"])
        held = await workflow({"op": "claim"})
        assert (await coded({"op": "select", "tab": 7}))["error"] == "TabClaimed"
        assert (await workflow({"op": "release", "lease": held["lease"]}))["ok"]
        mine = await coded({"op": "select", "tab": 7})
        assert (await workflow({"op": "claim"}))["error"] == "TabClaimed"
        assert (await coded({"op": "page_read", "lease": mine["lease"]}))["ok"]
        assert (await coded({"op": "claim"}))["error"] == "InvalidRequest"
        assert (await workflow({"op": "tabs"}))["error"] == "InvalidRequest"

    run(short, body)


def test_the_worker_offline_quiet_matches_the_code_lifetime():
    """The worker's offline check (contract "All-https access", signal 5) is
    ``CODE_TTL + 60`` s; changing either side alone fails here."""
    package = Path(codes.__file__).parent
    source = (package / "extension" / "codes.js").read_text()
    found = re.findall(r"const QUIET_MS = (\d+);", source)
    margin = 60
    assert margin > protocol.PAGE_WAIT  # a redeem probe in flight fits the margin
    expected = int((codes.CODE_TTL + margin) * 1000)
    assert found == [str(expected)]
    # The contract doc is not package data; reach it from the source tree (#11239).
    docs = Path(__file__).resolve().parents[1] / "docs"
    contract = (docs / "activation-codes.md").read_text()
    assert f"`QUIET_MS = {expected}`" in contract
    assert f"which is {expected // 1000} s" in contract

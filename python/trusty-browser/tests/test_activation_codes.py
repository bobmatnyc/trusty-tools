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

import pytest

from trusty_browser import audit, codes, enrollment, service

URL = "https://qa.test/app/page?q=private-query"
PAGE_TEXT = "page-secret-text"


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
    """Answers broker commands as the extension would, for one paired profile."""

    def __init__(self, rig, row):
        self.rig, self.row, self.tab = rig, row, 7
        self.missing: set[int] = set()
        self.seen: list[tuple[str, object]] = []
        self.hold: dict[str, asyncio.Event] = {}
        self.arrived: dict[str, asyncio.Event] = {}

    async def pair(self, life="boot"):
        hello = dict(
            type="hello",
            profile=self.row["profile"],
            email=self.row["email"],
            incarnation=life,
            nonce=secrets.token_hex(32),
        )
        self.reader, self.writer = await asyncio.open_unix_connection(
            self.rig.state / "service.sock"
        )
        await service.write_frame(self.writer, hello)
        challenge = await service.read_frame(self.reader)
        proof = service.pairing_proof(
            self.row["key"], "extension", hello, challenge["nonce"]
        )
        await service.write_frame(self.writer, dict(type="proof", proof=proof))
        assert (await service.read_frame(self.reader))["ok"]
        self.task = asyncio.create_task(self.loop())
        return self

    async def loop(self):
        with contextlib.suppress(
            EOFError, asyncio.IncompleteReadError, ConnectionError
        ):
            while True:
                req = await service.read_frame(self.reader)
                asyncio.create_task(self.answer(req))

    async def answer(self, req):
        op = req["op"]
        self.seen.append((op, req.get("tab")))
        if op in self.arrived:
            self.arrived[op].set()
        if op in self.hold:
            await self.hold[op].wait()
        tab = req.get("tab", self.tab)
        reply = dict(
            id=req["id"],
            ok=True,
            email=req["email"],
            url=req["url"],
            account=req["account"],
            tab=tab,
            result={"field": PAGE_TEXT},
        )
        if tab is None or tab in self.missing:
            reply = dict(id=req["id"], ok=False, error="TabNotFound")
        with contextlib.suppress(ConnectionError, RuntimeError):
            await service.write_frame(self.writer, reply)

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
        row = dict(
            op="enroll",
            profile=str(uuid.uuid4()),
            email=name + "@example.test",
            workflow=name,
            url=url,
            account=name,
            key=key,
        )
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

    async def mint(self, row, scope="profile", tab=None):
        frame = {k: row[k] for k in enrollment.FIELDS}
        frame.update(op="mint", scope=scope)
        if tab is not None:
            frame["tab"] = tab
        return await self.once("enrollment.sock", frame)

    async def redeem(self, code):
        reader, writer, reply = await self.connect(
            "agent.sock", dict(op="redeem", code=code)
        )

        async def call(frame):
            await service.write_frame(writer, frame)
            return await service.read_frame(reader)

        self.writers.append(writer)
        return reply, call

    async def open(self, row):
        reader, writer, reply = await self.connect(
            "agent.sock",
            dict(op="open", workflow=row["workflow"], ticket=row["ticket"]),
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
        reply, call = await rig.redeem(minted["code"].lower().replace("1", "l"))
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
        rig.a.hold["inspect"], rig.a.arrived["inspect"] = (
            asyncio.Event(),
            asyncio.Event(),
        )
        first = asyncio.create_task(rig.redeem(code))
        await rig.a.arrived["inspect"].wait()
        loser, _ = await rig.redeem(code)
        rig.a.hold["inspect"].set()
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
        replaced = (await rig.mint(rig.alpha, "tab", 7))["code"]
        rig.a.tab = None
        assert (await rig.redeem(closed))[0] == {"ok": False, "error": "TabClosed"}
        rig.a.tab = 8
        assert (await rig.redeem(replaced))[0] == {"ok": False, "error": "TabClosed"}
        assert (await rig.redeem(closed))[0]["error"] == "CodeRefused"
        restarted = (await rig.mint(rig.alpha, "tab", 8))["code"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
        rig.a.tab = 8
        assert (await rig.redeem(restarted))[0]["error"] == "TabClosed"
        assert ("lease_show", 8) not in rig.a.seen
        assert rig.outcomes("redeem").count("tab_closed") == 3

    run(short, body)


def test_tab_closed_after_redeem_ends_the_grant(short, clock):
    async def body(rig):
        reply, call = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        lease = (await call({"op": "claim"}))["lease"]
        rig.a.missing.add(7)
        ended = await gone(call, dict(op="snapshot", lease=lease, request_id="r1"))
        assert ended == {"ok": False, "error": "GrantEnded", "reason": "tab_closed"}
        assert not rig.broker.claims and not rig.broker.grants

    run(short, body)


def test_tab_scoped_session_is_refused_on_another_tab(short, clock):
    async def body(rig):
        _, call = await rig.redeem((await rig.mint(rig.alpha, "tab", 7))["code"])
        rig.a.tab = 8
        assert (await call({"op": "claim"})) == {"ok": False, "error": "OutOfScope"}
        assert not rig.broker.claims
        assert [s for s in rig.a.seen if s[0] != "inspect"] == []
        rig.a.tab = 7
        assert (await call({"op": "claim"}))["tab"] == 7

    run(short, body)


def test_profile_scoped_session_is_refused_on_another_profile(short, clock):
    async def body(rig):
        _, travel = await rig.open(rig.beta)
        beta_lease = (await travel({"op": "claim"}))["lease"]
        seen = list(rig.b.seen)
        _, call = await rig.redeem((await rig.mint(rig.alpha))["code"])
        assert (await call({"op": "claim", "profile": rig.beta["profile"]}))[
            "error"
        ] == "InvalidRequest"
        foreign = dict(op="snapshot", lease=beta_lease, request_id="x")
        assert (await call(foreign))["error"] == "StaleLease"
        assert (await call({"op": "claim"}))["email"] == rig.alpha["email"]
        assert rig.b.seen == seen

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
        assert (await second({"op": "claim"}))["ok"]
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
        lease = (await call({"op": "claim"}))["lease"]
        rig.a.hold["click"], rig.a.arrived["click"] = asyncio.Event(), asyncio.Event()
        click = asyncio.create_task(
            call(dict(op="click", lease=lease, request_id="c", marker="m"))
        )
        await rig.a.arrived["click"].wait()
        await rig.operator(op="revoke_grant", grant=minted["grant"])
        rig.a.hold["click"].set()
        assert await click == {"ok": False, "error": "OutcomeUnknown"}
        assert (await gone(call))["error"] == "GrantEnded"

    run(short, body)


def test_caps_refuse_mint_and_redeem_beyond_the_limits(short, clock):
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
        for _ in range(codes.MAX_GRANTS - 1):
            assert (await rig.redeem((await rig.mint(rig.beta))["code"]))[0]["ok"]
        assert (await rig.mint(rig.beta)) == {
            "ok": False,
            "error": "GrantLimit",
            "limit": "total",
        }
        await rig.operator(op="revoke_grants")
        assert (await rig.mint(rig.beta))["ok"]

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


def test_idle_browser_loss_and_socket_close_end_the_grant(short, clock):
    async def body(rig):
        _, idle = await rig.redeem((await rig.mint(rig.alpha))["code"])
        assert (await idle({"op": "claim"}))["ok"]
        clock[0] += codes.GRANT_IDLE
        assert (await gone(idle))["reason"] == "idle"
        _, lost = await rig.redeem((await rig.mint(rig.alpha))["code"])
        await rig.a.leave()
        assert (await gone(lost))["reason"] == "browser_gone"
        reply, closing = await rig.redeem((await rig.mint(rig.beta))["code"])
        rig.writers[-1].close()
        for _ in range(200):
            if not rig.broker.grants:
                break
            await asyncio.sleep(0)
        assert not rig.broker.grants
        assert rig.outcomes("grant_end") == ["idle", "browser_gone", "closed"]

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
            reply = await rig.once("agent.sock", dict(op="redeem", code=rig.code))
            assert reply == {"ok": False, "error": "CodeRefused"}
            assert (await rig.once("operator.sock", dict(op="grants"))) == {
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
        claim = await call({"op": "claim"})
        assert (await call(dict(op="snapshot", lease=claim["lease"], request_id="s")))[
            "result"
        ]["field"] == PAGE_TEXT
        await call(dict(op="click", lease=claim["lease"], request_id="c", marker="m"))
        await call(dict(op="release", lease=claim["lease"]))
        await rig.redeem(minted["code"])
        await rig.operator(op="revoke_grants")
        text = (rig.state / "audit.jsonl").read_text()
        forbidden = [
            minted["code"],
            minted["code"].replace("-", "")[2:],
            codes.code_digest(minted["code"]),
            rig.alpha["ticket"],
            rig.alpha["key"],
            claim["lease"],
            PAGE_TEXT,
            "/app/page",
            "private-query",
        ]
        assert [value for value in forbidden if value in text] == []
        lines = rig.audit()
        assert {k for line in lines for k in line} <= {"ts", *audit.FIELDS}
        assert {line["origin"] for line in lines if "origin" in line} == {"qa.test"}
        ops = [(l["op"], l["outcome"]) for l in lines if l["event"] == "operation"]
        assert ops == [
            (op, o)
            for op in ("claim", "snapshot", "click", "release")
            for o in ("admitted", "ok")
        ]
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
        assert (await call({"op": "claim"})) == {
            "ok": False,
            "error": "AuditUnavailable",
        }
        assert not rig.broker.claims and ("inspect", None) not in rig.a.seen

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
        assert (await coded({"op": "claim"}))["error"] == "TabClaimed"
        assert (await workflow(dict(op="release", lease=held["lease"])))["ok"]
        mine = await coded({"op": "claim"})
        assert (await workflow({"op": "claim"}))["error"] == "TabClaimed"
        assert (await coded(dict(op="snapshot", lease=mine["lease"], request_id="s")))[
            "ok"
        ]

    run(short, body)

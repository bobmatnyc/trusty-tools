"""One-click Connect grants (#11239) exercised through the real broker sockets and a fake extension peer."""

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
        self.dialog: object = None  # a click or type result's ``dialog`` (#11294)
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

    def land(self, req, tab, item):
        """The LANDED result after the tab's origin changed or reloaded."""
        item["access"] = item["origin"] == ORIGIN
        item["title"] = TITLE if item["access"] else None
        item["document"] = f"d{tab}-{secrets.token_hex(2)}"
        landed = self.page({**req, "op": "page_tab"})
        del landed["title"]
        return {**landed, "settled": True}

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
        if op in ("page_back", "page_forward", "page_reload"):
            # #11292: a per-tab history of origins; reload is a new document in place.
            past, ahead = item.setdefault("past", []), item.setdefault("ahead", [])
            if op != "page_reload":
                source, sink = (past, ahead) if op == "page_back" else (ahead, past)
                if not source:
                    return {"error": "NoHistory", "tab": tab}
                sink.append(item["origin"])
                item["origin"] = source.pop()
            return self.land(req, tab, item)
        if op == "page_switch":
            for other in self.tabs.values():
                if other["window"] == item["window"]:
                    other["active"] = False
            item["active"] = True
            return {"window": item["window"], "focused": True}
        if op == "page_navigate":
            target = urlsplit(req["url"])
            item.setdefault("past", []).append(item["origin"])
            item["ahead"] = []
            item["origin"] = f"{target.scheme}://{target.netloc}"
            return self.land(req, tab, item)
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
        if op == "page_find":  # #11357
            match = {"ref": "1.0", "tag": "button", "role": "button", "name": "Delete"}
            match["in"] = {"role": "row", "name": "Alice Delete", "ref": "1.1"}
            return {"document": item["document"], "origin": ORIGIN,
                    "matches": [match], "truncated": False}  # fmt: skip
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
        if op == "page_select":
            # #11285: the fake select starts at "" and moves to any new value.
            self.done.append((op, req["ref"], req["value"]))
            changed = item.get("selected", "") != req["value"]
            item["selected"] = req["value"]
            return {
                "document": item["document"],
                "changed": changed,
                "value": req["value"],
            }
        self.done.append((op, req.get("ref"), req.get("text")))
        if op == "page_drag":
            return {"document": item["document"], "dropped": req["target"] != "1.9"}
        if self.dialog is not None:
            return {"document": item["document"], "dialog": self.dialog}
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

    async def grant(self, row):
        """The popup's Connect: a standing grant for the row's profile, no code."""
        return await self.popup(row, "connect")

    async def attach(self, profile=None):
        hello = {"op": "attach"} if profile is None else {"op": "attach", "profile": profile}
        reader, writer, reply = await self.connect("agent.sock", hello)

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


async def session(rig, row=None):
    """Connect the row's profile in the popup, then attach an agent to it."""
    row = row or rig.alpha
    connected = await rig.grant(row)
    assert connected["ok"], connected
    ack, call = await rig.attach(row["profile"])
    assert ack["ok"], ack
    return ack, call


async def settle(rig):
    """Let fire-and-forget grant_end sends reach the fake worker."""
    for _ in range(50):
        await asyncio.sleep(0)
    await asyncio.gather(*rig.broker.tasks)


def test_connect_with_no_code_creates_a_standing_profile_grant(short, clock):
    async def body(rig):
        connected = await rig.grant(rig.alpha)
        assert connected == {
            "ok": True,
            "grant": connected["grant"],
            "scope": "profile",
            "profile": rig.alpha["profile"],
        }
        assert re.fullmatch(r"g-[0-9a-f]{12}", connected["grant"])
        # A repeat click reuses the grant; it never makes a second one.
        assert (await rig.grant(rig.alpha))["grant"] == connected["grant"]
        assert len(rig.broker.grants) == 1
        assert stat.S_IMODE((rig.state / "audit.jsonl").stat().st_mode) == 0o600
        first, call = await rig.attach()
        assert first == {
            "ok": True,
            "grant": connected["grant"],
            "scope": "profile",
            "profile": rig.alpha["profile"],
            "email": rig.alpha["email"],
        }
        assert (await call({"op": "tabs"}))["ok"]
        # Any same-user agent drives the profile; one leaving never ends the grant.
        second, other = await rig.attach(rig.alpha["profile"])
        assert second["grant"] == connected["grant"]
        rig.writers[0].close()
        await settle(rig)
        assert (await other({"op": "tabs"}))["ok"]
        assert connected["grant"] in rig.broker.grants
        assert rig.outcomes("connect") == ["ok"]
        assert rig.outcomes("attach") == ["ok", "ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_connect_whose_audit_fails_creates_no_grant(short, clock, monkeypatch):
    async def body(rig):
        def broken(*_a, **_k):
            raise OSError("disk full")

        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await rig.grant(rig.alpha)) == {"ok": False, "error": "AuditUnavailable"}
        assert not rig.broker.grants
        assert (await rig.attach())[0] == {"ok": False, "error": "NotConnected"}
        assert (await rig.popup(rig.alpha, "grants")) == {"ok": True, "grants": []}

    run(short, body)


def test_connect_needs_the_paired_browser_and_its_key(short, clock):
    async def body(rig):
        wrong = {**rig.alpha, "key": secrets.token_hex(32)}
        assert (await rig.grant(wrong))["error"] == "NotEnrolled"
        stranger = {**rig.alpha, "profile": str(uuid.uuid4())}
        assert (await rig.grant(stranger))["error"] == "NotEnrolled"
        assert (await rig.popup(rig.alpha, "connect", scope="tab"))[
            "error"
        ] == "InvalidRequest"
        await rig.a.leave()
        assert (await rig.grant(rig.alpha)) == {
            "ok": False,
            "error": "BrowserDisconnected",
        }
        assert not rig.broker.grants

    run(short, body)


def test_attach_needs_no_code_and_several_profiles_need_a_choice(short, clock):
    async def body(rig):
        assert (await rig.attach())[0] == {"ok": False, "error": "NotConnected"}
        assert (await rig.once("agent.sock", {"op": "profiles"})) == {
            "ok": True,
            "profiles": [],
        }
        alpha = (await rig.grant(rig.alpha))["grant"]
        beta = (await rig.grant(rig.beta))["grant"]
        listed = [
            {"profile": rig.alpha["profile"], "email": rig.alpha["email"], "grant": alpha},
            {"profile": rig.beta["profile"], "email": rig.beta["email"], "grant": beta},
        ]
        assert (await rig.once("agent.sock", {"op": "profiles"}))["profiles"] == listed
        # Never a silent pick between two connected profiles.
        assert (await rig.attach())[0] == {
            "ok": False,
            "error": "ProfileChoiceRequired",
            "profiles": listed,
        }
        ack, call = await rig.attach(rig.beta["profile"])
        assert ack["grant"] == beta and ack["profile"] == rig.beta["profile"]
        assert (await call({"op": "tabs"}))["ok"]
        assert rig.a.sent("page_tabs") == [] and rig.b.sent("page_tabs")
        assert (await rig.attach(str(uuid.uuid4())))[0]["error"] == "NotConnected"
        assert (await rig.once("agent.sock", {"op": "attach", "code": "x"}))[
            "error"
        ] == "InvalidRequest"

    run(short, body)


def test_disconnect_revokes_the_grant_at_once(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        assert (await call({"op": "tabs"}))["ok"]
        assert (await rig.popup(rig.alpha, "revoke_grants")) == {"ok": True, "revoked": 1}
        assert not rig.broker.grants
        assert (await gone(call, {"op": "tabs"}))["reason"] == "revoked"
        await settle(rig)
        ends = rig.a.sent("grant_end")
        assert [(f["grant"], f["reason"], f["last"]) for f in ends] == [
            (ack["grant"], "revoked", True)
        ]
        assert rig.outcomes("revoke") == ["ok"]
        assert (await rig.attach())[0]["error"] == "NotConnected"

    run(short, body)


def test_disconnect_whose_audit_fails_still_revokes_at_once(
    short, clock, monkeypatch, capsys
):
    async def body(rig):
        _, call = await session(rig)

        def broken(*_a, **_k):
            raise OSError("disk full")

        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await rig.popup(rig.alpha, "revoke_grants")) == {"ok": True, "revoked": 1}
        assert not rig.broker.grants
        assert (await gone(call, {"op": "tabs"}))["reason"] == "revoked"
        assert "audit write failed for revoke" in capsys.readouterr().err

    run(short, body)


def test_removed_enroll_and_code_ops_are_refused(short, clock):
    async def body(rig):
        frame = {k: rig.alpha[k] for k in enrollment.FIELDS}
        for op in ("mint", "enroll", "replace", "unenroll", "status", "resolve"):
            reply = await rig.once("enrollment.sock", {**frame, "op": op})
            assert reply == {"ok": False, "error": "InvalidRequest"}, op
        assert (await rig.popup(rig.alpha, "mint", scope="profile"))[
            "error"
        ] == "InvalidRequest"
        redeem = {"op": "redeem", "code": "tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA"}
        assert (await rig.once("agent.sock", redeem))["error"] == "InvalidRequest"
        # A leftover routes row's ticket opens no grant-free, unaudited session.
        ticket = {"op": "open", "workflow": "alpha", "ticket": rig.alpha["ticket"]}
        assert await rig.once("agent.sock", ticket) == {"ok": False, "error": "InvalidRequest"}
        assert not rig.broker.sessions
        assert not rig.broker.grants and rig.outcomes("mint") == []

    run(short, body)


def test_connect_is_refused_on_every_socket_but_enrollment(short, clock):
    async def body(rig):
        frame = {"op": "connect", **{k: rig.alpha[k] for k in ("profile", "email", "key")}}
        assert (await rig.once("agent.sock", frame))["error"] == "InvalidRequest"
        assert (await rig.once("service.sock", frame))["error"] == "InvalidRequest"
        assert (await rig.once("operator.sock", frame))["error"] == "RevokeFailed"
        assert not rig.broker.grants

    run(short, body)


def test_revoke_one_and_all_refuse_the_next_call(short, clock):
    async def body(rig):
        one, first = await session(rig, rig.alpha)
        two, second = await session(rig, rig.beta)
        listed = await rig.operator(op="grants")
        assert {g["grant"]: (g["state"], g["agents"]) for g in listed["grants"]} == {
            one["grant"]: ("active", 1),
            two["grant"]: ("active", 1),
        }
        assert rig.alpha["key"] not in json.dumps(listed)
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
        assert (await rig.operator(op="revoke_grants"))["revoked"] == 1
        assert (await gone(second))["reason"] == "revoked"
        assert not rig.broker.claims
        assert (await session(rig))[0]["ok"]

    run(short, body)


def test_revoke_during_an_in_flight_click_answers_outcome_unknown(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        lease = (await call({"op": "select", "tab": 7}))["lease"]
        hold, arrived = asyncio.Event(), asyncio.Event()
        rig.a.hold["page_click"], rig.a.arrived["page_click"] = hold, arrived
        click = asyncio.create_task(
            call({"op": "page_click", "lease": lease, "request_id": "c", "ref": "1.0"})
        )
        await arrived.wait()
        await rig.operator(op="revoke_grant", grant=ack["grant"])
        hold.set()
        assert await click == {"ok": False, "error": "OutcomeUnknown"}
        assert (await gone(call))["error"] == "GrantEnded"

    run(short, body)


def test_unenroll_ends_the_grant_for_the_profile(short, clock):
    async def body(rig):
        _, call = await session(rig)
        assert await rig.operator(op="revoke", workflow="alpha") == {"ok": True}
        with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
            await call({"op": "claim"})
        assert "unenrolled" in rig.outcomes("grant_end")
        assert not rig.broker.grants

    run(short, body)


# #11239 owner ruling: longer than the retired 30-minute idle timeout.
LONG_IDLE = 35 * 60 + 1


def test_browser_loss_ends_the_grant_and_an_agent_leaving_does_not(short, clock):
    async def body(rig):
        _, lost = await session(rig, rig.alpha)
        assert (await lost({"op": "tabs"}))["ok"]
        await rig.a.leave()
        clock[0] += codes.RECONNECT_GRACE
        assert (await gone(lost))["reason"] == "browser_gone"
        ack, _closing = await session(rig, rig.beta)
        rig.writers[-1].close()
        for _ in range(200):
            if not rig.broker.grants[ack["grant"]].sessions:
                break
            await asyncio.sleep(0)
        assert ack["grant"] in rig.broker.grants
        assert rig.outcomes("grant_end") == ["browser_gone"]
        assert rig.outcomes("detach") == ["ok"]

    run(short, body)


def test_idle_profile_grants_stay_listed_past_the_old_timeout(short, clock):
    async def body(rig):
        _, call = await session(rig)
        clock[0] += LONG_IDLE
        listed = (await rig.operator(op="grants"))["grants"]
        assert [g["state"] for g in listed] == ["active"]
        assert rig.broker.sweeper is None  # no idle deadline is armed
        assert (await call({"op": "tabs"}))["ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_a_grant_survives_a_worker_reconnect_within_the_grace(short, clock):
    async def body(rig):
        ack, whole = await session(rig)
        await rig.a.leave()  # Chrome stopped the idle service worker
        assert await whole({"op": "tabs"}) == {
            "ok": False,
            "error": "BrowserDisconnected",
        }
        clock[0] += codes.RECONNECT_GRACE - 1
        rig.a = await FakeBrowser(rig, rig.alpha).pair()  # same incarnation
        assert ack["grant"] in rig.a.ack["live"]
        assert (await whole({"op": "tabs"}))["ok"]
        clock[0] += LONG_IDLE
        assert len((await rig.operator(op="grants"))["grants"]) == 1
        assert (await whole({"op": "tabs"}))["ok"]
        assert rig.outcomes("grant_end") == []

    run(short, body)


def test_a_grant_outlives_lease_expiry_and_audits_create_reuse_and_end(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        assert (await call({"op": "select", "tab": 7}))["ok"]
        for claim in rig.broker.claims.values():
            claim.deadline = 0.0  # the lease expired
        clock[0] += LONG_IDLE
        assert (await call({"op": "select", "tab": 7}))["ok"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair()
        assert ack["grant"] in rig.a.ack["live"]
        assert (await call({"op": "select", "tab": 7}))["ok"]
        assert rig.outcomes("connect") == ["ok"]
        assert rig.outcomes("grant_reuse") == ["ok"]
        await rig.operator(op="revoke_grant", grant=ack["grant"])
        assert rig.outcomes("grant_end") == ["revoked"]

    run(short, body)


def test_revoke_inside_the_reconnect_grace_ends_the_grant_at_once(short, clock):
    async def body(rig):
        ack, call = await session(rig)
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
        ack, call = await session(rig)
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
        ack, call = await session(rig)
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


def test_a_reconnect_after_the_grace_ends_the_grant_without_a_sweep(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        await rig.a.leave()
        rig.broker.halt()  # no sweep runs; only the hello sees the lapsed grace
        clock[0] += codes.RECONNECT_GRACE + 30
        rig.a = await FakeBrowser(rig, rig.alpha).pair()  # same incarnation
        assert ack["grant"] not in rig.a.ack["live"]
        assert rig.outcomes("grant_reuse") == []
        assert rig.outcomes("grant_end") == ["browser_gone"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"

    run(short, body)


def test_a_browser_restart_ends_the_grant_at_reconnect(short, clock):
    async def body(rig):
        ack, call = await session(rig)
        assert (await call({"op": "select", "tab": 7}))["ok"]
        await rig.a.leave()
        rig.a = await FakeBrowser(rig, rig.alpha).pair("boot-two")
        assert not rig.broker.claims  # the ended grant holds no lease
        assert ack["grant"] not in rig.a.ack["live"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"
        assert rig.outcomes("grant_end") == ["browser_gone"]
        # #11239 ruling Dpj: no auto-reconnect; the popup offers Connect again.
        assert (await rig.popup(rig.alpha, "grants")) == {"ok": True, "grants": []}
        assert (await rig.attach())[0]["error"] == "NotConnected"

    run(short, body)


@pytest.mark.parametrize("life", [None, ""])
def test_a_hello_without_an_incarnation_gets_no_peer_so_no_connect(
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
        assert (await rig.grant(rig.alpha))["error"] == "BrowserDisconnected"
        assert not rig.broker.grants

    run(short, body)


def test_a_grant_whose_worker_never_returns_ends_after_the_grace(short, clock):
    async def body(rig):
        _, call = await session(rig)
        await rig.a.leave()
        assert rig.broker.sweeper is not None  # the grace deadline is armed
        clock[0] += codes.RECONNECT_GRACE - 1
        assert len((await rig.operator(op="grants"))["grants"]) == 1
        clock[0] += 1
        assert (await rig.operator(op="grants")) == {"ok": True, "grants": []}
        assert rig.outcomes("grant_end") == ["browser_gone"]
        assert (await gone(call, {"op": "tabs"}))["reason"] == "browser_gone"

    run(short, body)


def test_broker_restart_leaves_no_grant(short, clock):
    holder = {}

    async def body(rig):
        holder["rig"] = rig
        await session(rig)

    run(short, body)
    rig = holder["rig"]

    async def again():
        async with service.Broker(rig.state) as broker:
            assert not broker.grants
            assert (await rig.attach())[0] == {"ok": False, "error": "NotConnected"}
            assert (await rig.once("operator.sock", {"op": "grants"})) == {
                "ok": True,
                "grants": [],
            }

    rig.writers = []
    asyncio.run(again())


def test_audit_lines_carry_no_secret_path_or_page_text(short, clock):
    async def body(rig):
        _, call = await session(rig)
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
        await rig.operator(op="revoke_grants")
        text = (rig.state / "audit.jsonl").read_text()
        forbidden = [
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
        assert [e["event"] for e in lines if e["event"] in ("connect", "attach", "revoke")] == [
            "connect",
            "attach",
            "revoke",
        ]

    run(short, body)


def test_audit_write_failure_fails_attach_and_operations_closed(
    short, clock, monkeypatch
):
    async def body(rig):
        real = rig.broker.audit.write

        def broken(*_a, **_k):
            raise OSError("disk full")

        assert (await rig.grant(rig.alpha))["ok"]
        monkeypatch.setattr(rig.broker.audit, "write", broken)
        assert (await rig.attach())[0] == {"ok": False, "error": "AuditUnavailable"}
        monkeypatch.setattr(rig.broker.audit, "write", real)
        _, call = await rig.attach()
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
        log.write("connect", grant=f"g-{index}", outcome="ok")
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


def test_the_worker_offline_quiet_matches_the_reconnect_grace():
    """The worker's offline check (contract "All-https access", signal 5) is
    ``RECONNECT_GRACE + 60`` s; changing either side alone fails here."""
    package = Path(codes.__file__).parent
    source = (package / "extension" / "codes.js").read_text()
    found = re.findall(r"const QUIET_MS = (\d+);", source)
    margin = 60
    assert margin > protocol.PAGE_WAIT  # a probe in flight fits the margin
    expected = int((codes.RECONNECT_GRACE + margin) * 1000)
    assert found == [str(expected)]
    # The contract doc is not package data; reach it from the source tree (#11239).
    docs = Path(__file__).resolve().parents[1] / "docs"
    contract = (docs / "activation-codes.md").read_text()
    assert f"`QUIET_MS = {expected}`" in contract
    assert f"which is {expected // 1000} s" in contract

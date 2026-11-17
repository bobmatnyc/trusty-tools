"""Per-profile pairing (#11239) through the real broker's enrollment and operator sockets."""

# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import asyncio
import contextlib
import secrets
import shutil
import sqlite3
import tempfile
import uuid
from pathlib import Path

import pytest
from test_activation_codes import (  # noqa: F401
    FakeBrowser,
    Rig,
    clock,
    run,
    session,
    short,
)

from trusty_browser import service, vault


def rows(state):
    with contextlib.closing(sqlite3.connect(state / "registry.sqlite3")) as db:
        db.row_factory = sqlite3.Row
        return [dict(r) for r in db.execute("SELECT * FROM pairings")]


async def paired(rig, name="gamma"):
    """Pair a fresh profile and connect its fake browser."""
    cred = rig.credential(name)
    assert await rig.pair(cred) == {"ok": True, "profile": cred["profile"]}
    return cred, await FakeBrowser(rig, cred).pair()


@pytest.fixture
def keychain(monkeypatch):
    # The real vault seals here; only the Chrome-host check is stubbed.
    root = Path(tempfile.mkdtemp(dir="/tmp"))
    monkeypatch.setattr(service, "verify_chrome_host", lambda *_: None)
    yield root / "s"
    shutil.rmtree(root)


def test_pair_is_idempotent_and_seals_exactly_one_key(keychain):
    async def body():
        rig = Rig(keychain)
        rig.writers = []
        async with service.Broker(keychain) as broker:
            rig.broker = broker
            cred, browser = await paired(rig)
            assert browser.ack["ok"] and browser.ack["generation"]
            assert await rig.pair(cred) == {"ok": True, "profile": cred["profile"]}
            stored = rows(keychain)
            assert [(r["profile"], r["email"]) for r in stored] == [
                (cred["profile"], cred["email"])
            ]
            sealed = stored[0]["browser_key"]
            assert sealed.startswith(vault.PREFIX) and cred["key"] not in sealed
            assert (
                vault.unseal(keychain, "browser:" + cred["profile"], sealed)
                == (cred["key"])
            )
            reply, _ = await session(rig, cred)
            assert reply["ok"] and reply["profile"] == cred["profile"]
            browser.writer.close()
            for writer in rig.writers:
                writer.close()
        files = [p for p in keychain.rglob("*") if p.is_file()]
        assert {p.name for p in files} <= {
            "registry.sqlite3",
            "keychain.lock",
            "service.lock",
            "audit.jsonl",
        }
        for path in files:
            data = path.read_bytes()
            assert cred["key"].encode() not in data, path

    asyncio.run(body())


def test_conflicting_pair_answers_enrollment_conflict_and_changes_nothing(short, clock):
    async def body(rig):
        cred, _ = await paired(rig)
        before = rows(rig.state)
        # Ruling Dqw 2026-11-16: a new profile id for the same email now replaces
        # (see the re-pair test); a route or a changed key on the profile still refuses.
        conflicts = [
            ({**cred, "key": secrets.token_hex(32)}, "enroll_conflict_key"),
            ({**cred, "email": "other@example.test"}, "enroll_conflict_key"),
            ({k: rig.alpha[k] for k in ("profile", "email", "key")}, "enroll_conflict_route"),
            ({**rig.credential("x"), "email": rig.alpha["email"]}, "enroll_conflict_route"),
        ]
        for frame, reason in conflicts:
            assert await rig.pair(frame) == {
                "ok": False,
                "error": "EnrollmentConflict",
                "reason": reason,
            }
        malformed = [
            {**cred, "extra": 1},
            {**cred, "profile": "not-a-uuid"},
            {**cred, "key": cred["key"].upper()},
            {**cred, "email": "no-at-sign"},
            {k: cred[k] for k in ("profile", "email")},
        ]
        for frame in malformed:
            assert (await rig.pair(frame))["error"] == "InvalidEnrollment"
        assert rows(rig.state) == before
        # #11239: the workflow enroll op is gone, so it cannot take a paired profile.
        enroll = {**cred, "op": "enroll", "workflow": "fresh", "url": "https://qa.test/"}
        reply = await rig.once("enrollment.sock", enroll)
        assert reply["error"] == "InvalidRequest"
        assert rows(rig.state) == before

    run(short, body)


def test_a_same_email_repair_replaces_the_old_pairing_and_ends_its_grants(short, clock):
    async def body(rig):
        old, browser = await paired(rig)
        _, call = await session(rig, old)
        # Lost extension storage: the same account pairs again under a new id and key.
        new = {**rig.credential("gamma"), "email": old["email"]}
        assert await rig.pair(new) == {"ok": True, "profile": new["profile"]}
        assert [(r["profile"], r["email"]) for r in rows(rig.state)] == [
            (new["profile"], new["email"])
        ]
        assert not rig.broker.grants
        assert rig.outcomes("grant_end") == ["unenrolled"]
        with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
            await call({"op": "tabs"})
        await browser.task  # the old profile's native connection is dropped
        assert old["profile"] not in rig.broker.peers
        replaced = [line for line in rig.audit() if line["event"] == "pair_replaced"]
        assert replaced == [
            {
                "ts": replaced[0]["ts"],
                "event": "pair_replaced",
                "profile": new["profile"],
                "replaced": old["profile"],
                "outcome": "ok",
            }
        ]
        assert await rig.grant(old) == {"ok": False, "error": "NotEnrolled"}
        await FakeBrowser(rig, new).pair()
        assert (await rig.grant(new))["ok"]

    run(short, body)


def test_connect_with_a_pairing_credential_succeeds_and_a_wrong_key_does_not(
    short, clock
):
    async def body(rig):
        cred, browser = await paired(rig)
        other, _ = await paired(rig, "delta")
        made = await rig.grant(cred)
        assert made["ok"] and made["profile"] == cred["profile"]
        for frame in (
            {**cred, "key": secrets.token_hex(32)},
            {**cred, "key": other["key"]},
            {**cred, "email": other["email"]},
        ):
            assert await rig.grant(frame) == {"ok": False, "error": "NotEnrolled"}
        assert (await rig.grant(rig.alpha))["ok"]  # an enrolled profile connects alike
        ack, call = await rig.attach(cred["profile"])
        assert ack["grant"] == made["grant"]
        assert (await call({"op": "select", "tab": 7}))["ok"]
        assert ("page_tab", 7) in browser.seen

    run(short, body)


def test_unpair_ends_grants_and_a_removed_key_cannot_connect(short, clock):
    async def body(rig):
        cred, browser = await paired(rig)
        _, call = await session(rig, cred)
        assert await rig.pair(cred, "unpair") == {
            "ok": True,
            "profile": cred["profile"],
        }
        assert await rig.grant(cred) == {"ok": False, "error": "NotEnrolled"}
        assert rows(rig.state) == []
        with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
            await call({"op": "tabs"})
        assert not rig.broker.grants
        assert rig.outcomes("grant_end").count("unenrolled") == 1
        assert rig.outcomes("unpair") == ["ok"]
        await browser.task  # the broker closed the profile's native connection
        assert cred["profile"] not in rig.broker.peers
        assert (await rig.pair(cred))["ok"]  # the profile may pair again

    run(short, body)


def test_unpair_with_a_wrong_key_is_unauthorized_and_keeps_the_pairing(short, clock):
    async def body(rig):
        cred, _ = await paired(rig)
        ack, call = await session(rig, cred)
        before = rows(rig.state)
        for frame in (
            {**cred, "key": secrets.token_hex(32)},
            {**cred, "profile": str(uuid.uuid4())},
            {k: rig.alpha[k] for k in ("profile", "email", "key")},
        ):
            assert await rig.pair(frame, "unpair") == {
                "ok": False,
                "error": "Unauthorized",
                "reason": "unpair_bad_credential",
            }
        assert (await rig.pair({**cred, "extra": 1}, "unpair"))[
            "error"
        ] == "InvalidEnrollment"
        assert rows(rig.state) == before
        assert rig.broker.grants[ack["grant"]].state == "active"
        assert (await call({"op": "tabs"}))["ok"]

    run(short, body)


def test_operator_pairings_list_and_unpair(short, clock):
    async def body(rig):
        cred, _ = await paired(rig)
        _, call = await session(rig, cred)
        listed = await rig.operator(op="pairings")
        assert listed == {
            "ok": True,
            "pairings": [{"profile": cred["profile"], "email": cred["email"]}],
        }
        assert await rig.operator(op="unpair", profile=str(uuid.uuid4())) == {
            "ok": False,
            "error": "UnknownProfile",
        }
        for frame in ({"profile": "x"}, {"profile": cred["profile"], "key": "k"}):
            assert (await rig.operator(op="unpair", **frame))["error"] == "RevokeFailed"
        assert rows(rig.state) != []
        reply = await rig.operator(op="unpair", profile=cred["profile"])
        assert reply == {"ok": True, "profile": cred["profile"]}
        assert rows(rig.state) == [] and not rig.broker.grants
        with pytest.raises((EOFError, asyncio.IncompleteReadError, ConnectionError)):
            await call({"op": "tabs"})

    run(short, body)


def test_popup_list_and_revoke_see_only_the_credential_profile(short, clock):
    async def body(rig):
        cred, _ = await paired(rig)
        mine = await rig.grant(cred)
        theirs = await rig.grant(rig.alpha)
        listed = await rig.popup(cred, "grants")
        assert [g["grant"] for g in listed["grants"]] == [mine["grant"]]
        assert await rig.popup(cred, "revoke_grant", grant=theirs["grant"]) == {
            "ok": False,
            "error": "UnknownGrant",
        }
        assert theirs["grant"] in rig.broker.grants
        assert await rig.popup({**cred, "key": secrets.token_hex(32)}, "grants") == {
            "ok": False,
            "error": "NotEnrolled",
        }
        assert (await rig.popup(cred, "grants", extra=1))["error"] == "InvalidRequest"
        assert (await rig.popup(cred, "revoke_grant", grant=mine["grant"]))[
            "revoked"
        ] == 1
        assert (await rig.grant(cred))["ok"]
        assert await rig.popup(cred, "revoke_grants") == {"ok": True, "revoked": 1}
        assert list(rig.broker.grants) == [theirs["grant"]]

    run(short, body)

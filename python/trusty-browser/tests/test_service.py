"""Native service contract exercised through two real AF_UNIX listeners."""

import asyncio
import tempfile
import contextlib
import uuid
import json
from trusty_browser import enrollment
import secrets
from pathlib import Path

import pytest

from trusty_browser import service
from trusty_browser.protocol import HEADER, MAX_FRAME


def seed(state, profile, email, workflow, url, account):
    installation = str(uuid.uuid4())
    key = secrets.token_hex(32)
    with contextlib.closing(enrollment.database(state)) as db:
        enrollment.register(
            db,
            state,
            dict(
                op="enroll",
                profile=installation,
                email=email,
                workflow=workflow,
                url=url,
                account=account,
                key=key,
            ),
        )
    ticket = enrollment.read_credential(
        state / "workflows" / (workflow + ".json"), workflow
    )
    return dict(
        profile=installation, email=email, workflow=workflow, key=key, ticket=ticket
    )


def test_enrollment_is_durable_and_unique():
    with tempfile.TemporaryDirectory() as root:
        state = Path(root)
        first = seed(state, "p", "p@one.test", "w", "https://qa.test/", "one")
        assert first["key"] != first["ticket"]
        with pytest.raises(ValueError):
            seed(state, "other", "P@one.test", "q", "https://qa.test/", "two")
        assert first["ticket"].encode() not in (state / "registry.sqlite3").read_bytes()


def test_restart_and_untrusted_handshake():
    asyncio.run(restart_and_untrusted_handshake())


async def restart_and_untrusted_handshake():
    with tempfile.TemporaryDirectory() as root:
        state = Path(root)
        row = seed(
            state, "a", "a@example.test", "run-a", "https://qa.test/", "account-a"
        )
        previous = None
        for boot in range(2):
            async with service.Broker(state):

                async def begin():
                    reader, writer = await asyncio.open_unix_connection(
                        state / "service.sock"
                    )
                    hello = dict(
                        type="hello",
                        profile=row["profile"],
                        email="a@example.test",
                        incarnation=f"browser-{boot}",
                        nonce=secrets.token_hex(32),
                    )
                    await service.write_frame(writer, hello)
                    challenge = await service.read_frame(reader)
                    assert challenge["proof"] == service.pairing_proof(
                        row["key"], "broker", hello, challenge["nonce"]
                    )
                    return reader, writer, hello, challenge

                # A process can copy the native host's argv and connect under the correct UID.
                # The reflected broker proof still cannot satisfy the extension role.
                mr, mw, _, mc = await begin()
                await service.write_frame(mw, {"type": "proof", "proof": mc["proof"]})
                assert (await service.read_frame(mr))["error"] == "Unauthorized"
                mw.close()
                if previous:
                    mr, mw, _, _ = await begin()
                    await service.write_frame(mw, {"type": "proof", "proof": previous})
                    assert (await service.read_frame(mr))["error"] == "Unauthorized"
                    mw.close()
                reader, writer, hello, ch = await begin()
                previous = service.pairing_proof(
                    row["key"], "extension", hello, ch["nonce"]
                )
                await service.write_frame(writer, {"type": "proof", "proof": previous})
                assert (await service.read_frame(reader))["ok"]
                ar, aw = await asyncio.open_unix_connection(state / "agent.sock")
                await service.write_frame(
                    aw, {"op": "open", "ticket": row["ticket"], "workflow": "run-a"}
                )
                # #11239: the workflow ticket path is removed.
                assert (await service.read_frame(ar))["error"] == "InvalidRequest"
                writer.close()
                aw.close()
                await writer.wait_closed()
                await aw.wait_closed()


def test_singleton_locks_both_sockets_and_preserves_occupied_path():
    async def check():
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            async with service.Broker(state):
                before = [
                    (state / name).stat().st_ino
                    for name in ("service.sock", "agent.sock")
                ]
                with pytest.raises(BlockingIOError):
                    async with service.Broker(state):
                        pytest.fail("second broker acquired the socket namespace")
                assert before == [
                    (state / name).stat().st_ino
                    for name in ("service.sock", "agent.sock")
                ]
            (state / "agent.sock").write_text("user data")
            with pytest.raises(ValueError, match="occupied"):
                async with service.Broker(state):
                    pass
            assert (state / "agent.sock").read_text() == "user data"
            assert not (state / "service.sock").exists()

    asyncio.run(check())


def test_cli_revokes_offline_and_validates_route(tmp_path, monkeypatch):
    import sys

    seed(tmp_path, "p", "p@qa.test", "work", "https://qa.test/", "a")
    monkeypatch.setattr(
        sys, "argv", ["trusty-browser", "--state", str(tmp_path), "revoke", "work"]
    )
    service.main()
    assert not (tmp_path / "workflows" / "work.json").exists()
    with pytest.raises(ValueError, match="InvalidEnrollment"):
        seed(tmp_path, "p", "p@qa.test", "work", "http://public.test/", "a")
    tmp_path.chmod(0o755)
    with pytest.raises(ValueError, match="private directory"):
        enrollment.private_directory(tmp_path)


def test_invalid_native_frames_are_rejected_before_dispatch():

    async def check(payload):
        reader = asyncio.StreamReader()
        reader.feed_data(payload)
        reader.feed_eof()
        with pytest.raises(ValueError, match="InvalidFrame"):
            await service.read_frame(reader)

    asyncio.run(check(HEADER.pack(MAX_FRAME + 1)))
    data = json.dumps(["claim"]).encode()
    asyncio.run(check(HEADER.pack(len(data)) + data))


def test_idle_claim_expires_while_connection_is_open(monkeypatch, tmp_path):
    fake = object()
    claim = service.Claim(fake, 1, "agent", "lease", 100.0)
    broker = service.Broker(tmp_path)

    class Peer:
        generation = "generation"

    broker.claims[("generation", 1)] = claim
    monkeypatch.setattr(service.time, "monotonic", lambda: 200.0)
    assert broker.active(Peer(), 1) is None
    assert not broker.claims

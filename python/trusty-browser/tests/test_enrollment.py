"""Verify the enrollment boundary, immutable routes, recovery and revocation."""

import asyncio
import contextlib
import json
import os
from pathlib import Path
import secrets
import sys
import tempfile
import uuid

import pytest

from trusty_browser import enrollment
from trusty_browser import mac_peer
from trusty_browser import service


def request():
    return dict(
        op="enroll",
        profile=str(uuid.uuid4()),
        email="test@example.test",
        workflow="travel",
        url="https://qa.test/",
        account="travel",
        key=secrets.token_hex(32),
    )


def test_retry_recovers_without_rebinding_or_overwriting(tmp_path, monkeypatch):
    req = request()
    destination = tmp_path / "workflows" / "travel.json"
    with contextlib.closing(enrollment.database(tmp_path)) as db:
        real = enrollment.publish

        def interrupted(*_):
            raise OSError("interrupted")

        monkeypatch.setattr(enrollment, "publish", interrupted)
        with pytest.raises(OSError, match="interrupted"):
            enrollment.register(db, tmp_path, req)
        assert db.execute("SELECT count(*) FROM routes").fetchone()[0] == 1
        monkeypatch.setattr(enrollment, "publish", real)
        assert enrollment.register(db, tmp_path, req) == {
            "ok": True,
            "workflow": "travel",
        }
        old = destination.read_bytes()
        assert destination.stat().st_mode & 0o777 == 0o600
        assert set(json.loads(old)) == {"workflow", "ticket"}
        assert enrollment.register(db, tmp_path, req)["ok"]
        assert destination.read_bytes() == old
        for key, val in (
            ("key", secrets.token_hex(32)),
            ("profile", str(uuid.uuid4())),
            ("url", "https://new.test/"),
            ("account", "other"),
        ):
            with pytest.raises(ValueError, match="EnrollmentConflict"):
                enrollment.register(db, tmp_path, {**req, key: val})
        with pytest.raises(ValueError, match="InvalidEnrollment"):
            enrollment.register(db, tmp_path, {**req, "workflow": "../elsewhere"})
        bad = {**req, "op": "unenroll", "key": secrets.token_hex(32)}
        with pytest.raises(ValueError, match="Unauthorized"):
            enrollment.remove(db, tmp_path, "travel", bad)
        assert destination.read_bytes() == old
        revoked = {**req, "op": "unenroll"}
        assert enrollment.remove(db, tmp_path, "travel", revoked) == req["profile"]
        enrollment.finish_removals(db, tmp_path)
        assert not destination.exists()
        assert enrollment.remove(db, tmp_path, "travel", revoked) is None
        with pytest.raises(ValueError, match="EnrollmentRevoked"):
            enrollment.register(db, tmp_path, req)
        enrollment.register(
            db,
            tmp_path,
            {**req, "profile": str(uuid.uuid4()), "key": secrets.token_hex(32)},
        )
        with pytest.raises(ValueError, match="Unauthorized"):
            enrollment.remove(db, tmp_path, "travel", revoked)


def test_ordinary_client_cannot_enroll():
    async def exercise():
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            async with service.Broker(state) as broker:
                r, w = await asyncio.open_unix_connection(state / "enrollment.sock")
                await service.write_frame(w, request())
                assert (await service.read_frame(r)) == {
                    "ok": False,
                    "error": "Unauthorized",
                }
                w.close()
                assert not broker.db.execute("SELECT * FROM routes").fetchall()
                assert not (state / "workflows").exists()

    asyncio.run(exercise())


def test_host_with_genuine_argv_but_wrong_parent_is_explicitly_rejected():
    async def exercise():
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp)
            here = Path(service.__file__).resolve().parent
            broker = await asyncio.create_subprocess_exec(
                sys.executable, str(here / "service.py"), "--state", str(state), "serve"
            )
            try:
                async with asyncio.timeout(6):
                    while not (state / "enrollment.sock").is_socket():
                        await asyncio.sleep(0.01)
                child = await asyncio.create_subprocess_exec(
                    sys.executable,
                    str(here / "native_host.py"),
                    "--socket",
                    str(state / "enrollment.sock"),
                    mac_peer.ORIGIN,
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                try:
                    # Keep stdin open as Chrome would. EOF would close the relay before
                    # its opposite direction could deliver the explicit rejection frame.
                    from trusty_browser.protocol import HEADER

                    msg = json.dumps(request()).encode()
                    child.stdin.write(HEADER.pack(len(msg)) + msg)
                    await child.stdin.drain()
                    head = await asyncio.wait_for(child.stdout.readexactly(4), 5)
                    (size,) = HEADER.unpack(head)
                    assert json.loads(await child.stdout.readexactly(size)) == {
                        "ok": False,
                        "error": "Unauthorized",
                    }
                finally:
                    child.stdin.close()
                    await asyncio.wait_for(child.wait(), 5)
                with contextlib.closing(enrollment.database(state)) as db:
                    assert db.execute("SELECT count(*) FROM routes").fetchone()[0] == 0
                assert not (state / "workflows").exists()
            finally:
                broker.terminate()
                await broker.wait()

    if sys.platform != "darwin":
        pytest.skip("Requires the macOS native bootstrap host")
    asyncio.run(exercise())


@pytest.mark.skipif(sys.platform != "darwin", reason="Darwin kernel identity API")
def test_kernel_process_queries_read_actual_executable_and_direct_parent():
    assert mac_peer.process_exe(os.getpid()).is_file()
    assert mac_peer.parent_pid(os.getpid()) == os.getppid()
    assert len(mac_peer.process_argv(os.getpid())) >= 1


def test_orphan_grant_is_definitive_before_any_registration(tmp_path):
    with contextlib.closing(enrollment.database(tmp_path)) as db:
        req = request()
        root = tmp_path / "workflows"
        root.mkdir(mode=0o700)
        path = root / (req["workflow"] + ".json")
        path.write_text("unrelated credential file")
        with pytest.raises(ValueError, match="^WorkflowCredentialOccupied$"):
            enrollment.register(db, tmp_path, req)
        assert not db.execute("SELECT workflow FROM routes").fetchall()
        assert not db.execute("SELECT workflow FROM pending_credentials").fetchall()
        assert enrollment.register(db, tmp_path, {**req, "workflow": "fresh"})["ok"]
        assert path.read_text() == "unrelated credential file"


def test_damaged_pending_removal_preserves_foreign_file_and_service_startup():
    async def check(state):
        req = request()
        with contextlib.closing(enrollment.database(state)) as db:
            enrollment.register(db, state, req)
            enrollment.remove(db, state, req["workflow"])
        path = state / "workflows" / (req["workflow"] + ".json")
        path.write_text("do not unlink")
        async with service.Broker(state) as broker:
            assert path.read_text() == "do not unlink"
            assert (
                broker.db.execute("SELECT workflow FROM pending_removals").fetchone()[0]
                == req["workflow"]
            )
            assert enrollment.register(
                broker.db, state, {**request(), "workflow": "another"}
            )["ok"]

    with tempfile.TemporaryDirectory() as tmp:
        asyncio.run(check(Path(tmp)))


@pytest.mark.parametrize("action", ["popup", "cli"])
def test_removed_extension_cannot_prove_its_old_browser_key(action, monkeypatch):
    async def check(state):
        req = request()
        async with service.Broker(state) as broker:
            await broker.enrollment_request(req)

            async def pair():
                reader, writer = await asyncio.open_unix_connection(
                    state / "service.sock"
                )
                hello = dict(
                    type="hello",
                    profile=req["profile"],
                    email=req["email"],
                    incarnation="browser",
                    nonce=secrets.token_hex(32),
                )
                try:
                    await service.write_frame(writer, hello)
                    challenge = await service.read_frame(reader)
                    if not challenge.get("nonce"):
                        return challenge
                    assert challenge["proof"] == service.pairing_proof(
                        req["key"], "broker", hello, challenge["nonce"]
                    )
                    await service.write_frame(
                        writer,
                        dict(
                            type="proof",
                            proof=service.pairing_proof(
                                req["key"], "extension", hello, challenge["nonce"]
                            ),
                        ),
                    )
                    return await service.read_frame(reader)
                finally:
                    writer.close()
                    await writer.wait_closed()

            assert (await pair())["ok"]
            if action == "popup":
                # Exercise the signed popup request handler including frame decoding.
                monkeypatch.setattr(service, "verify_chrome_host", lambda *_: None)
                reader, writer = await asyncio.open_unix_connection(
                    state / "enrollment.sock"
                )
                await service.write_frame(writer, {**req, "op": "unenroll"})
                assert (await service.read_frame(reader))["ok"]
                writer.close()
                await writer.wait_closed()
            else:
                proc = await asyncio.create_subprocess_exec(
                    sys.executable,
                    service.__file__,
                    "--state",
                    str(state),
                    "revoke",
                    req["workflow"],
                )
                assert await proc.wait() == 0
            assert await pair() == {"ok": False, "error": "Unauthorized"}
            assert req["profile"] not in broker.peers

    with tempfile.TemporaryDirectory() as tmp:
        asyncio.run(check(Path(tmp)))

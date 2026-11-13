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

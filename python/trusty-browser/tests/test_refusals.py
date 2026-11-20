"""Every broker refusal is logged with a reason code, the email domain only (ruling Dqw 2026-11-16)."""

# ruff: noqa: F811 -- the imported fixtures `short` and `clock` are test parameters.

import asyncio
import contextlib
import secrets
import sqlite3

import pytest
from test_activation_codes import FakeBrowser, clock, run, short  # noqa: F401

from trusty_browser import audit, enrollment, mac_peer, service
from trusty_browser.protocol import Refusal

SECRET_FIELDS = {"key", "ticket", "email", "nonce", "proof", "browser_key"}


def refusals(rig):
    return [line for line in rig.audit() if line["event"] == "refused"]


def assert_logged(rig, site, reason, profile=None, secrets_=()):
    """The newest ``refused`` line names the site and reason and carries no secret."""
    line = refusals(rig)[-1]
    assert (line["site"], line["reason"], line["outcome"]) == (site, reason, "refused")
    assert line["reason"] in audit.REASONS
    assert line.get("profile") == profile
    assert not SECRET_FIELDS & set(line), line
    if "email_domain" in line:
        assert line["email_domain"] == "example.test"
    text = (rig.state / "audit.jsonl").read_text()
    for value in secrets_:
        assert value not in text, "a secret reached the audit log"


async def hello(rig, cred, proof_key, before_proof=None):
    """Run the service.sock pairing by hand; answer the broker's final frame."""
    reader, writer = await asyncio.open_unix_connection(rig.state / "service.sock")
    frame = {
        "type": "hello",
        "profile": cred["profile"],
        "email": cred["email"],
        "incarnation": "boot",
        "nonce": secrets.token_hex(32),
    }
    try:
        await service.write_frame(writer, frame)
        challenge = await service.read_frame(reader)
        if "nonce" not in challenge:
            return challenge
        if before_proof is not None:
            before_proof()
        proof = service.pairing_proof(proof_key, "extension", frame, challenge["nonce"])
        await service.write_frame(writer, {"type": "proof", "proof": proof})
        return await service.read_frame(reader)
    finally:
        writer.close()


async def paired(rig):
    cred = rig.credential("gamma")
    assert (await rig.pair(cred))["ok"]
    return cred


def test_each_refusal_site_writes_one_domain_only_audit_line(short, clock, monkeypatch):
    async def body(rig):
        cred = await paired(rig)
        unknown = rig.credential("delta")
        email = cred["email"]
        # service.sock: an unknown profile, then a proof made with the wrong key.
        reply = await hello(rig, unknown, unknown["key"])
        assert reply == {"ok": False, "error": "Unauthorized"}  # no reason off the popup path
        assert_logged(rig, "service", "route_unknown", unknown["profile"], [unknown["email"]])
        reply = await hello(rig, cred, secrets.token_hex(32))
        assert reply == {"ok": False, "error": "Unauthorized"}
        assert_logged(rig, "service", "proof_mismatch", cred["profile"], [cred["key"], email])

        # The pairing changes while the browser computes its proof.
        def rotate():
            with contextlib.closing(sqlite3.connect(rig.state / "registry.sqlite3")) as db:
                db.execute(
                    "UPDATE pairings SET browser_key=? WHERE profile=?",
                    (secrets.token_hex(32), cred["profile"]),
                )
                db.commit()

        reply = await hello(rig, cred, cred["key"], before_proof=rotate)
        assert reply == {"ok": False, "error": "Unauthorized"}
        assert_logged(rig, "service", "key_rotated", cred["profile"], [cred["key"], email])
        cred = await paired(rig)  # a fresh pairing for the enrollment.sock cases

        # enrollment.sock: unpair with a wrong key, and the two pair conflicts.
        wrong = {**cred, "key": secrets.token_hex(32)}
        reply = await rig.pair(wrong, "unpair")
        assert reply == {"ok": False, "error": "Unauthorized", "reason": "unpair_bad_credential"}
        assert_logged(rig, "unpair", "unpair_bad_credential", cred["profile"], [wrong["key"]])
        reply = await rig.pair(wrong)
        assert reply["reason"] == "enroll_conflict_key"
        assert_logged(rig, "pair", "enroll_conflict_key", cred["profile"], [wrong["key"]])
        route = {**rig.credential("x"), "email": rig.alpha["email"]}
        assert (await rig.pair(route))["reason"] == "enroll_conflict_route"
        assert_logged(rig, "pair", "enroll_conflict_route", route["profile"], [route["key"]])

        # The Chrome-host check: each typed failure carries its own code to the popup.
        for code in ("host_not_native_host", "host_parent_not_chrome", "host_unsafe_flag",
                     "host_codesign", "host_timeout"):  # fmt: skip
            def refuse(*_, code=code):
                raise mac_peer.HostRefused(code, "refused")

            monkeypatch.setattr(service, "verify_chrome_host", refuse)
            assert await rig.pair(cred) == {"ok": False, "error": "Unauthorized", "reason": code}
            assert_logged(rig, "enrollment", code)
        monkeypatch.setattr(service, "verify_chrome_host", lambda *_: None)

        # A peer of another user, on each socket.
        def other_user(_writer):
            raise Refusal("Unauthorized")

        real = service.verify_peer
        monkeypatch.setattr(service, "verify_peer", other_user)
        for sock, site in (("agent.sock", "agent"), ("operator.sock", "operator"),
                           ("enrollment.sock", "enrollment")):  # fmt: skip
            await rig.once(sock, {"op": "attach"})
            assert_logged(rig, site, "peer_uid")
        assert (await rig.pair(cred))["reason"] == "peer_uid"
        reply = await hello(rig, cred, cred["key"])
        assert reply == {"ok": False, "error": "Unauthorized"}
        assert_logged(rig, "service", "peer_uid")
        monkeypatch.setattr(service, "verify_peer", real)

        # A workflow ticket that does not match its route (agent.sock, no live path).
        with pytest.raises(Refusal):
            rig.broker.route({"workflow": "alpha", "ticket": "wrong-ticket"}, False)
        assert_logged(rig, "agent", "ticket_mismatch", None, ["wrong-ticket"])

        # enrollment.py: an unknown removal or status request.
        bad = {k: v for k, v in rig.alpha.items() if k != "ticket"}
        bad["key"] = secrets.token_hex(32)
        db = rig.broker.db
        with pytest.raises(ValueError, match="Unauthorized"):
            enrollment.remove(db, rig.state, "alpha", {**bad, "op": "unenroll"})
        assert_logged(rig, "remove", "remove_unauthorized", rig.alpha["profile"], [bad["key"]])
        with pytest.raises(ValueError, match="Unauthorized"):
            enrollment.status(db, rig.state, {**bad, "workflow": "nobody", "op": "status"})
        assert_logged(rig, "status", "status_unauthorized", None, [bad["key"]])

    run(short, body)


def test_a_refusal_whose_audit_write_fails_still_refuses(short, clock, capfd):
    async def body(rig):
        cred = await paired(rig)
        before = rig.broker.db.execute("SELECT profile FROM pairings").fetchall()
        # A directory in the log's place makes every audit write fail.
        (rig.state / "audit.jsonl").unlink()
        (rig.state / "audit.jsonl").mkdir()
        wrong = {**cred, "key": secrets.token_hex(32)}
        assert (await rig.pair(wrong, "unpair"))["error"] == "Unauthorized"
        assert (await rig.pair(wrong))["error"] == "EnrollmentConflict"
        reply = await hello(rig, cred, secrets.token_hex(32))
        assert reply["error"] == "Unauthorized"
        assert cred["profile"] not in rig.broker.peers
        after = rig.broker.db.execute("SELECT profile FROM pairings").fetchall()
        assert [tuple(r) for r in after] == [tuple(r) for r in before]
        (rig.state / "audit.jsonl").rmdir()

    run(short, body)
    err = capfd.readouterr().err
    for reason in ("unpair_bad_credential", "enroll_conflict_key", "proof_mismatch"):
        assert "refused site=" in err and f"reason={reason}" in err, reason
    assert "audit write failed for refused" in err


def test_each_host_check_raises_its_reason_code(monkeypatch, tmp_path):
    exe = mac_peer.CHROME_EXE
    monkeypatch.setattr(mac_peer, "kernel_pid", lambda _sock: 10)
    monkeypatch.setattr(mac_peer, "parent_pid", lambda _pid: 9)
    host = mac_peer.Path(mac_peer.__file__).resolve().with_name("native_host.py")
    argv = {10: ["python", str(host), "--socket", str(tmp_path / "enrollment.sock"),
                 mac_peer.ORIGIN], 9: [str(exe)]}  # fmt: skip
    # Any pid but the parent, our own included, runs the broker's interpreter.
    exes = {9: exe}
    monkeypatch.setattr(mac_peer, "process_exe", lambda pid: exes.get(pid, mac_peer.Path("/bin/me")))
    monkeypatch.setattr(mac_peer, "process_argv", lambda pid: argv[pid])

    class Done:
        returncode = 0

    monkeypatch.setattr(mac_peer.subprocess, "run", lambda *a, **k: Done())
    mac_peer.verify_chrome_host(None, tmp_path)  # the all-pass baseline

    def code():
        with pytest.raises(mac_peer.HostRefused) as caught:
            mac_peer.verify_chrome_host(None, tmp_path)
        return caught.value.code

    Done.returncode = 1
    assert code() == "host_codesign"
    Done.returncode = 0

    def slow(*_a, **_k):
        raise mac_peer.subprocess.TimeoutExpired("codesign", 5)

    monkeypatch.setattr(mac_peer.subprocess, "run", slow)
    assert code() == "host_timeout"
    argv[9] = [str(exe), "--load-extension=/tmp/x"]
    assert code() == "host_unsafe_flag"
    exes[9] = mac_peer.Path("/bin/other")
    assert code() == "host_parent_not_chrome"
    argv[10] = ["python", "other.py"]
    assert code() == "host_not_native_host"

    def unreadable(_pid):
        raise PermissionError("cannot identify peer executable")

    monkeypatch.setattr(mac_peer, "process_exe", unreadable)
    assert code() == "host_not_native_host"

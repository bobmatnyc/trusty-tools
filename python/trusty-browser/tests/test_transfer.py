"""Two installations can transfer one workflow only against its reviewed version."""

from contextlib import closing
from pathlib import Path
import secrets
import uuid

import pytest

from trusty_browser import enrollment


def proposal(email, workflow="qa"):
    return dict(
        op="enroll",
        profile=str(uuid.uuid4()),
        key=secrets.token_hex(32),
        email=email,
        workflow=workflow,
        url="https://qa.example.test/",
        account=email.split("@")[0],
    )


def test_confirmed_transfer_is_atomic_and_recovers_pending_publication(
    tmp_path, monkeypatch
):
    a, b, c = map(proposal, ("a@one.test", "b@two.test", "c@three.test"))
    with closing(enrollment.database(tmp_path)) as db:
        enrollment.register(db, tmp_path, a)
        grant = tmp_path / "workflows/qa.json"
        ticket_a = enrollment.read_credential(grant, "qa")
        v = enrollment.inspect_conflict(db, "qa")["version"]
        with pytest.raises(ValueError, match="EnrollmentConflict"):
            enrollment.register(db, tmp_path, b)
        assert enrollment.read_credential(grant, "qa") == ticket_a
        transfer = {**b, "op": "replace", "previous_version": v}
        original_publish = enrollment.publish

        def crash(*_):
            raise OSError("disk interruption")

        monkeypatch.setattr(enrollment, "publish", crash)
        old = enrollment.replace(db, tmp_path, transfer)
        assert old == a["profile"]
        with pytest.raises(OSError, match="interruption"):
            enrollment.register(db, tmp_path, b)
        assert db.execute("select profile from routes").fetchone()[0] == b["profile"]
        monkeypatch.setattr(enrollment, "publish", original_publish)
        assert enrollment.replace(db, tmp_path, transfer) is None
        assert enrollment.register(db, tmp_path, b)["ok"]
        assert enrollment.read_credential(grant, "qa") != ticket_a
        with pytest.raises(ValueError, match="ReplacementStale"):
            enrollment.replace(
                db, tmp_path, {**c, "op": "replace", "previous_version": v}
            )
        assert (
            enrollment.status(db, tmp_path, {**a, "op": "status"})["state"] == "revoked"
        )
        assert (
            enrollment.status(db, tmp_path, {**b, "op": "status"})["state"] == "active"
        )
        with pytest.raises(ValueError, match="Unauthorized"):
            enrollment.status(
                db, tmp_path, {**a, "op": "status", "key": secrets.token_hex(32)}
            )
        with pytest.raises(ValueError, match="EnrollmentRevoked"):
            enrollment.register(db, tmp_path, a)
        assert enrollment.inspect_conflict(db, "qa")["version"] != v


def test_replacement_cannot_capture_an_unrelated_route(tmp_path):
    a = proposal("a@example.test")
    other = proposal("real@other.test", "other")
    with closing(enrollment.database(tmp_path)) as db:
        for req in (a, other):
            enrollment.register(db, tmp_path, req)
        records = db.execute("select * from routes order by workflow").fetchall()
        v = enrollment.inspect_conflict(db, "qa")["version"]
        for req in (
            {**other, "workflow": "qa"},
            {**a, "key": a["key"], "account": "new"},
        ):
            with pytest.raises(ValueError, match="EnrollmentConflict"):
                enrollment.replace(
                    db, tmp_path, {**req, "op": "replace", "previous_version": v}
                )
        assert (
            records == db.execute("select * from routes order by workflow").fetchall()
        )


def test_two_simultaneous_confirmations_choose_exactly_one_installation(tmp_path):
    import threading
    from concurrent.futures import ThreadPoolExecutor

    start = threading.Barrier(2)
    a = proposal("old@one.test")
    b = proposal("one@new.test")
    c = proposal("two@new.test")
    with closing(enrollment.database(tmp_path)) as db:
        enrollment.register(db, tmp_path, a)
        v = enrollment.inspect_conflict(db, "qa")["version"]

    def compete(req):
        with closing(enrollment.database(tmp_path)) as conn:
            start.wait(timeout=5)
            try:
                return enrollment.replace(
                    conn, tmp_path, {**req, "op": "replace", "previous_version": v}
                )
            except ValueError as e:
                return str(e)

    with ThreadPoolExecutor(max_workers=2) as pool:
        outcomes = list(pool.map(compete, [b, c]))
    assert sorted(outcomes) == sorted([a["profile"], "ReplacementStale"])
    winning = [b, c][outcomes.index(a["profile"])]
    with closing(enrollment.database(tmp_path)) as db:
        assert enrollment.register(db, tmp_path, winning)["ok"]
        assert enrollment.inspect_conflict(db, "qa")["email"] == winning["email"]

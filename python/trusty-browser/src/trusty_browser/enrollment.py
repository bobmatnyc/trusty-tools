"""Persist immutable workflow routes and recover their local credentials across interruptions."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import sqlite3
import stat
import tempfile
from urllib.parse import urlsplit
import uuid


def private_directory(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = path.lstat()
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
        raise ValueError("state must be a private directory owned by the current user")


def digest(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()


def database(path: Path) -> sqlite3.Connection:
    private_directory(path)
    db_path = path / "registry.sqlite3"
    fd = os.open(db_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        item = os.fstat(fd)
        if (
            not stat.S_ISREG(item.st_mode)
            or item.st_uid != os.getuid()
            or item.st_mode & 0o077
        ):
            raise ValueError("registry must be an owner-only regular file")
    finally:
        os.close(fd)
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA secure_delete=ON")
    db.execute("""CREATE TABLE IF NOT EXISTS routes (
        profile TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, workflow TEXT UNIQUE NOT NULL,
        url TEXT NOT NULL, account TEXT NOT NULL, browser_key TEXT NOT NULL, ticket TEXT NOT NULL)""")
    # The outbox stores only the ticket to publish if a previous process died between
    # the registry commit and the atomic creation of the workflow credential file.
    db.execute("""CREATE TABLE IF NOT EXISTS pending_credentials (
        workflow TEXT PRIMARY KEY, ticket TEXT NOT NULL)""")
    db.execute("CREATE TABLE IF NOT EXISTS revoked (registration TEXT PRIMARY KEY)")
    db.execute(
        "CREATE TABLE IF NOT EXISTS pending_removals (workflow TEXT PRIMARY KEY, ticket_hash TEXT NOT NULL)"
    )
    return db


def publish(path: Path, workflow: str, ticket: str) -> None:
    root = path / "workflows"
    private_directory(root)
    destination = root / (workflow + ".json")
    content = (
        json.dumps({"workflow": workflow, "ticket": ticket}, indent=2) + "\n"
    ).encode()
    with tempfile.NamedTemporaryFile(dir=root, delete=False) as tmp:
        temporary = Path(tmp.name)
        try:
            os.fchmod(tmp.fileno(), 0o600)
            tmp.write(content)
            tmp.flush()
            os.fsync(tmp.fileno())
            try:
                os.link(temporary, destination)
            except FileExistsError:
                if not secrets.compare_digest(
                    read_credential(destination, workflow), ticket
                ):
                    raise ValueError("CredentialConflict") from None
            fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            temporary.unlink(missing_ok=True)


def read_credential(path: Path, workflow: str) -> str:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        attr = os.fstat(fd)
        if (
            not stat.S_ISREG(attr.st_mode)
            or attr.st_uid != os.getuid()
            or attr.st_mode & 0o777 != 0o600
        ):
            raise ValueError("CredentialConflict")
        fields = json.loads(stream.read(2048))
        if (
            not isinstance(fields, dict)
            or set(fields) != {"workflow", "ticket"}
            or fields["workflow"] != workflow
            or not isinstance(fields["ticket"], str)
        ):
            raise ValueError("CredentialConflict")
        return str(fields["ticket"])


def register(
    db: sqlite3.Connection, state: Path, request: dict[str, object]
) -> dict[str, object]:
    """Only the authenticated installation may create a route or repeat its exact request."""
    names = ("profile", "email", "workflow", "url", "account", "key")
    if request.get("op") != "enroll" or set(request) != {"op", *names}:
        raise ValueError("InvalidEnrollment")
    fields = [request[k] for k in names]
    if not all(
        isinstance(v, str) and v and v.strip() == v and len(v) <= 8192 for v in fields
    ):
        raise ValueError("InvalidEnrollment")
    profile, email, workflow, url, account, key = (str(v) for v in fields)
    address = urlsplit(url)
    email = email.casefold()
    try:
        identifier = uuid.UUID(profile)
    except ValueError:
        raise ValueError("InvalidEnrollment") from None
    if (
        str(identifier) != profile
        or identifier.version != 4
        or "@" not in email
        or re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", workflow) is None
        or re.fullmatch(r"[a-f0-9]{64}", key) is None
        or address.fragment
        or address.username is not None
        or not address.hostname
        or not (
            address.scheme == "https"
            or address.scheme == "http"
            and address.hostname in {"localhost", "127.0.0.1", "::1"}
        )
    ):
        raise ValueError("InvalidEnrollment")

    finish_removals(db, state)
    if db.execute(
        "SELECT 1 FROM revoked WHERE registration=?", (registration_id(request),)
    ).fetchone():
        raise ValueError("EnrollmentRevoked")

    # Serialize concurrent insert/retry and check all unique dimensions before minting.
    with db:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute(
            "SELECT * FROM routes WHERE profile=? OR email=? OR workflow=?",
            (profile, email, workflow),
        ).fetchone()
        if row is None:
            ticket = secrets.token_urlsafe(32)
            directory = state / "workflows"
            private_directory(directory)
            if os.path.lexists(directory / (workflow + ".json")):
                raise ValueError("CredentialConflict")
            db.execute(
                "INSERT INTO routes VALUES (?,?,?,?,?,?,?)",
                (profile, email, workflow, url, account, key, digest(ticket)),
            )
            db.execute(
                "INSERT INTO pending_credentials VALUES (?,?)", (workflow, ticket)
            )
        elif not (
            tuple(row[n] for n in names[:-1])
            == (profile, email, workflow, url, account)
            and secrets.compare_digest(key, row["browser_key"])
        ):
            raise ValueError("EnrollmentConflict")

    pending = db.execute(
        "SELECT ticket FROM pending_credentials WHERE workflow=?", (workflow,)
    ).fetchone()
    if pending is not None:
        publish(state, workflow, pending["ticket"])
        with db:
            db.execute("DELETE FROM pending_credentials WHERE workflow=?", (workflow,))
    else:
        # Detect tampering; never silently replace or rotate an existing agent's credential.
        saved = read_credential(state / "workflows" / (workflow + ".json"), workflow)
        if row is None or not secrets.compare_digest(row["ticket"], digest(saved)):
            raise ValueError("CredentialConflict")
    return {"ok": True, "workflow": workflow}


def registration_id(request: dict[str, object]) -> str:
    """Identify an exact registration without persisting its revoked key."""
    return digest(
        json.dumps(
            [
                request.get(k)
                for k in ("profile", "email", "workflow", "url", "account", "key")
            ],
            separators=(",", ":"),
            ensure_ascii=False,
        )
    )


def finish_removals(db: sqlite3.Connection, state: Path) -> None:
    """Retry credential-file cleanup after a process death during revoke."""
    for old in db.execute(
        "SELECT workflow,ticket_hash FROM pending_removals"
    ).fetchall():
        path = state / "workflows" / (old["workflow"] + ".json")
        try:
            token = read_credential(path, old["workflow"])
            if secrets.compare_digest(digest(token), old["ticket_hash"]):
                path.unlink(missing_ok=True)
        except FileNotFoundError:
            pass
        with db:
            db.execute(
                "DELETE FROM pending_removals WHERE workflow=?", (old["workflow"],)
            )


def remove(
    db: sqlite3.Connection,
    state: Path,
    workflow: str,
    request: dict[str, object] | None = None,
) -> str | None:
    if re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", workflow) is None:
        raise ValueError("InvalidEnrollment")
    with db:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute(
            "SELECT * FROM routes WHERE workflow=?", (workflow,)
        ).fetchone()
        if row is None:
            if (
                request is not None
                and not db.execute(
                    "SELECT 1 FROM revoked WHERE registration=?",
                    (registration_id(request),),
                ).fetchone()
            ):
                raise ValueError("Unauthorized")
            return None
        original = {
            n: row[n] for n in ("profile", "email", "workflow", "url", "account")
        }
        original["key"] = row["browser_key"]
        if request is not None and (
            set(request) != {*original, "op"}
            or not secrets.compare_digest(
                registration_id(original), registration_id(request)
            )
        ):
            raise ValueError("Unauthorized")
        db.execute(
            "INSERT OR IGNORE INTO revoked VALUES (?)", (registration_id(original),)
        )
        db.execute(
            "INSERT OR REPLACE INTO pending_removals VALUES (?,?)",
            (workflow, row["ticket"]),
        )
        db.execute("DELETE FROM pending_credentials WHERE workflow=?", (workflow,))
        db.execute("DELETE FROM routes WHERE workflow=?", (workflow,))
    return str(row["profile"])

"""Per-profile pairing records for activation codes; contract in docs/activation-codes.md, "pair"."""

from __future__ import annotations

import re
import secrets
import sqlite3
import uuid
from pathlib import Path

from trusty_browser import enrollment
from trusty_browser.audit import Audit, email_domain
from trusty_browser.enrollment import browser_key
from trusty_browser.protocol import Refusal

# #11239: per-profile pairing for activation codes; contract in
# docs/activation-codes.md, "pair". Tests live in tests/test_pairing.py.
CREDENTIAL = ("profile", "email", "key")
PAIR_COLUMNS = (
    "profile, email, NULL AS workflow, NULL AS url, NULL AS account,"
    " browser_key, NULL AS ticket"
)


def profile_id(value: object) -> str | None:
    """``value`` when it is a canonical UUID v4 string, else ``None``."""
    if not isinstance(value, str) or len(value) != 36:
        return None
    try:
        identifier = uuid.UUID(value)
    except ValueError:
        return None
    return value if str(identifier) == value and identifier.version == 4 else None


def triple(request: dict[str, object]) -> tuple[str, str, str] | None:
    """Return the well-formed ``(profile, email, key)`` of a frame, else ``None``."""
    values = [request.get(k) for k in CREDENTIAL]
    if not all(isinstance(v, str) and 0 < len(v) <= 8192 for v in values):
        return None
    profile, email, key = (str(v) for v in values)
    if (
        profile_id(profile) is None
        or "@" not in email
        or email.strip() != email
        or re.fullmatch(r"[a-f0-9]{64}", key) is None
    ):
        return None
    return profile, email.casefold(), key


def profile_row(db: sqlite3.Connection, profile: str) -> sqlite3.Row | None:
    """The profile's enrollment route, else its pairing in the same columns."""
    row = db.execute("SELECT * FROM routes WHERE profile=?", (profile,)).fetchone()
    if row is None:
        row = db.execute(
            f"SELECT {PAIR_COLUMNS} FROM pairings WHERE profile=?", (profile,)
        ).fetchone()
    return row


def credential_row(
    db: sqlite3.Connection, state: Path, fields: tuple[str, str, str]
) -> sqlite3.Row | None:
    """The row whose profile, email and browser key all match, else ``None``."""
    profile, email, key = fields
    row = profile_row(db, profile)
    if row is None or row["email"] != email:
        return None
    return row if secrets.compare_digest(browser_key(state, row), key) else None


def conflict(state: Path, reason: str, profile: str, email: str) -> Refusal:
    Audit(state).refuse("pair", reason, profile=profile, email_domain=email_domain(email))
    return Refusal("EnrollmentConflict", reason=reason)


def pair_profile(
    db: sqlite3.Connection, state: Path, request: dict[str, object]
) -> tuple[dict[str, object], str | None]:
    """Store one sealed browser key for a profile; return the reply and any replaced profile.

    Why: a profile with no workflow enrollment still needs a credential to pair its
    native port (#11239); a profile that lost its extension storage pairs again
    under a new id and must not be locked out (owner ruling Dqw 2026-11-16).
    What: an identical repeat changes nothing. With no route and exactly one
    pairing of the same email under another profile id, that row is replaced in
    this transaction and its profile id returned, for the caller to detach.
    A route on the profile or email, or a stored profile with another email or
    key, refuses ``EnrollmentConflict`` and changes nothing.
    Test: `test_pair_is_idempotent_and_seals_exactly_one_key`,
    `test_conflicting_pair_answers_enrollment_conflict_and_changes_nothing`,
    `test_a_same_email_repair_replaces_the_old_pairing_and_ends_its_grants`.
    """
    fields = triple(request) if set(request) == {"op", *CREDENTIAL} else None
    if fields is None:
        raise ValueError("InvalidEnrollment")
    profile, email, key = fields
    replaced = None
    with db:
        db.execute("BEGIN IMMEDIATE")
        if db.execute(
            "SELECT 1 FROM routes WHERE profile=? OR email=?", (profile, email)
        ).fetchone():
            raise conflict(state, "enroll_conflict_route", profile, email)
        rows = db.execute(
            "SELECT * FROM pairings WHERE profile=? OR email=?", (profile, email)
        ).fetchall()
        if len(rows) == 1 and rows[0]["profile"] != profile:
            # Only an email match reaches here: a new profile id for the same account.
            replaced = str(rows[0]["profile"])
            db.execute("DELETE FROM pairings WHERE profile=?", (replaced,))
            rows = []
        if not rows:
            db.execute(
                "INSERT INTO pairings VALUES (?,?,?)",
                (profile, email, enrollment.seal(state, "browser:" + profile, key)),
            )
        elif not (
            len(rows) == 1
            and rows[0]["email"] == email
            and secrets.compare_digest(browser_key(state, rows[0]), key)
        ):
            raise conflict(state, "enroll_conflict_key", profile, email)
    return {"ok": True, "profile": profile}, replaced


def unpair_profile(
    db: sqlite3.Connection, state: Path, request: dict[str, object] | None, profile: str
) -> bool:
    """Delete a pairing; with a ``request`` its credential must match exactly.

    Test: `test_unpair_ends_grants_and_a_removed_key_cannot_mint`,
    `test_unpair_with_a_wrong_key_is_unauthorized_and_keeps_the_pairing`.
    """
    with db:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute(
            "SELECT * FROM pairings WHERE profile=?", (profile,)
        ).fetchone()
        if row is None:
            return False
        if request is not None:
            fields = triple(request)
            if fields is None or credential_row(db, state, fields) is None:
                return False
        db.execute("DELETE FROM pairings WHERE profile=?", (profile,))
    return True


def pairings(db: sqlite3.Connection) -> list[dict[str, str]]:
    rows = db.execute("SELECT profile, email FROM pairings ORDER BY email").fetchall()
    return [{"profile": r["profile"], "email": r["email"]} for r in rows]

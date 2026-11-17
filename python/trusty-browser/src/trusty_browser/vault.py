"""Seal local browser pairing keys and workflow grants using the macOS login Keychain."""

from __future__ import annotations

import base64
import fcntl
import os
from pathlib import Path
import stat
import sys

from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from keyring.errors import KeyringError

PREFIX = "tb1."
SERVICE = "com.trusty.browser.local-credentials"


def _key(state: Path, create: bool) -> bytes:
    if sys.platform != "darwin":
        raise ValueError("Trusty Browser credential storage requires macOS Keychain")
    from keyring.backends.macOS import Keyring

    state = state.resolve(strict=True)
    attrs = state.stat()
    if (
        attrs.st_uid != os.getuid()
        or attrs.st_mode & 0o077
        or not stat.S_ISDIR(attrs.st_mode)
    ):
        raise ValueError("Credential directory must be private")
    # Ordinary and recovery registrations in this state share a single wrapping key.
    # Absence on read is an error. Only a writer may initialize a new key.
    fd = os.open(state / "keychain.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "rb") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        store = Keyring()
        try:
            value = store.get_password(SERVICE, str(state))
            if value is None and create:
                value = base64.b64encode(AESGCM.generate_key(bit_length=256)).decode(
                    "ascii"
                )
                store.set_password(SERVICE, str(state), value)
            if value is None:
                raise ValueError("Trusty Browser Keychain item is missing")
            return base64.b64decode(value, validate=True)
        except KeyringError as exc:
            raise ValueError(
                "Cannot access the Trusty Browser login Keychain item"
            ) from exc


def seal(state: Path, purpose: str, value: str) -> str:
    nonce = os.urandom(12)
    cipher = AESGCM(_key(state, True))
    body = cipher.encrypt(nonce, value.encode(), purpose.encode())
    return PREFIX + base64.urlsafe_b64encode(nonce + body).decode()


def unseal(state: Path, purpose: str, value: str) -> str:
    if not isinstance(value, str) or not value.startswith(PREFIX):
        raise ValueError("Invalid sealed credential")
    try:
        body = base64.b64decode(value[len(PREFIX) :], altchars=b"-_", validate=True)
        cipher = AESGCM(_key(state, False))
        return cipher.decrypt(body[:12], body[12:], purpose.encode()).decode()
    except (InvalidTag, UnicodeError, ValueError) as exc:
        raise ValueError("Cannot open credential for this workflow or profile") from exc

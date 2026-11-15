"""Activation-code format, grant records and redeem lockout; contract in docs/activation-codes.md."""

from __future__ import annotations

import hashlib
import secrets
import sqlite3
import time
from collections import deque
from dataclasses import dataclass
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

if TYPE_CHECKING:
    from trusty_browser.protocol import Session

CODE_TTL = 300.0
# #11239 owner ruling: a redeemed grant has no idle timeout; it lasts until the
# browser closes, so only a lost service worker that never returns ends it.
RECONNECT_GRACE = 45.0
MAX_GRANTS_PER_TAB = 1
FAIL_LIMIT = 5
FAIL_WINDOW = 60.0
LOCK_BASE = 60.0
LOCK_MAX = 900.0
CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
GROUPS, GROUP = 4, 6

# #11239: a module seam so tests move time without patching the event loop's clock.
clock = time.monotonic


def new_code() -> str:
    """Return ``tb-`` plus four groups of six Crockford characters (120 random bits)."""
    count = GROUPS * GROUP
    value = int.from_bytes(secrets.token_bytes(count * 5 // 8), "big")
    body = "".join(CROCKFORD[(value >> (5 * i)) & 31] for i in reversed(range(count)))
    return "tb-" + "-".join(body[i : i + GROUP] for i in range(0, count, GROUP))


def code_digest(code: str) -> str | None:
    """Normalise a pasted code and hash it; ``None`` when it cannot be a code."""
    text = code.strip().upper()
    if not text.startswith("TB-"):
        return None
    body = text[3:].replace("-", "").translate(str.maketrans("ILO", "110"))
    if len(body) != GROUPS * GROUP or any(c not in CROCKFORD for c in body):
        return None
    return hashlib.sha256(body.encode()).hexdigest()


@dataclass(eq=False)
class Grant:
    """An unredeemed code (``code``/``pending``) or a live grant (``active``)."""

    ident: str
    scope: str
    row: sqlite3.Row
    tab: int | None
    generation: str | None
    digest: str
    deadline: float
    state: str = "code"
    session: Session | None = None
    last_used: float = 0.0
    reason: str | None = None
    incarnation: str | None = None
    grace_until: float | None = None

    def fields(self) -> dict[str, object]:
        """Audit fields: never the code, its digest or the URL path."""
        return {
            "grant": self.ident,
            "scope": self.scope,
            "profile": str(self.row["profile"]),
            "tab": self.tab,
            "origin": urlsplit(str(self.row["url"] or "")).hostname,
        }

    def entry(self, now: float) -> dict[str, object]:
        item: dict[str, object] = {
            "grant": self.ident,
            "scope": self.scope,
            "profile": str(self.row["profile"]),
        }
        if self.tab is not None:
            item["tab"] = self.tab
        if self.state == "active":
            item.update(state="active", idle_s=round(now - self.last_used, 3))
        else:
            item.update(state="code", expires_in_s=round(self.deadline - now, 3))
        return item


class Lockout:
    """Global redeem lockout: FAIL_LIMIT failures in FAIL_WINDOW lock, doubling to LOCK_MAX."""

    def __init__(self) -> None:
        self.failures: deque[float] = deque()
        self.until = float("-inf")
        self.level = 0
        self.last_end = float("-inf")

    def remaining(self, now: float) -> float:
        return max(0.0, self.until - now)

    def fail(self, now: float) -> float | None:
        """Count one refused redeem; return the new lock length when it trips."""
        self.failures.append(now)
        while self.failures and self.failures[0] <= now - FAIL_WINDOW:
            self.failures.popleft()
        if len(self.failures) < FAIL_LIMIT:
            return None
        if now - self.last_end >= LOCK_MAX:
            self.level = 0
        length = min(LOCK_BASE * 2**self.level, LOCK_MAX)
        self.level += 1
        self.until = self.last_end = now + length
        self.failures.clear()
        return length

    def succeed(self) -> None:
        self.failures.clear()
        self.level = 0

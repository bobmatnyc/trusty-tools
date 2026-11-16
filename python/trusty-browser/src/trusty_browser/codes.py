"""Standing profile grants made by the popup's Connect; contract in docs/activation-codes.md."""

from __future__ import annotations

import sqlite3
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

if TYPE_CHECKING:
    from trusty_browser.protocol import Session

# #11239 owner ruling: a grant has no idle timeout; it lasts until the browser
# closes or the user disconnects, so only a worker that never returns ends it.
RECONNECT_GRACE = 45.0

# #11239: a module seam so tests move time without patching the event loop's clock.
clock = time.monotonic


@dataclass(eq=False)
class Grant:
    """A standing profile grant, ``active`` from Connect until it ends.

    Any number of same-user agent sessions may attach to it (#11239 owner
    ruling); an agent leaving never ends it.
    """

    ident: str
    scope: str
    row: sqlite3.Row
    tab: int | None
    generation: str | None
    state: str = "active"
    sessions: list[Session] = field(default_factory=list)
    last_used: float = 0.0
    reason: str | None = None
    incarnation: str | None = None
    grace_until: float | None = None

    def fields(self) -> dict[str, object]:
        """Audit fields: never a key or the URL path."""
        return {
            "grant": self.ident,
            "scope": self.scope,
            "profile": str(self.row["profile"]),
            "tab": self.tab,
            "origin": urlsplit(str(self.row["url"] or "")).hostname,
        }

    def entry(self, now: float) -> dict[str, object]:
        return {
            "grant": self.ident,
            "scope": self.scope,
            "profile": str(self.row["profile"]),
            "state": self.state,
            "agents": len(self.sessions),
            "idle_s": round(now - self.last_used, 3),
        }

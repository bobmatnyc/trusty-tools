"""Append-only, size-bounded audit log for activation codes; contract in docs/activation-codes.md."""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import sys
import time

FIELDS = ("event", "grant", "scope", "profile", "op", "tab", "outcome", "origin")
ROTATE_BYTES = 5 * 1024 * 1024
KEEP = 2


class Audit:
    """One JSON line per event, mode 0600; tests live in tests/test_activation_codes.py."""

    def __init__(self, state: Path) -> None:
        self.path = state / "audit.jsonl"

    def write(self, event: str, **fields: object) -> None:
        """Raise OSError when the line cannot be recorded; callers decide fail-closed."""
        unknown = set(fields) - set(FIELDS)
        if unknown:
            raise ValueError("unknown audit field")
        record = {"ts": round(time.time(), 3), "event": event}
        record.update({k: v for k, v in fields.items() if v is not None})
        line = (
            json.dumps(record, separators=(",", ":"), allow_nan=False) + "\n"
        ).encode()
        self.rotate()
        fd = os.open(
            self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600
        )
        try:
            item = os.fstat(fd)
            if (
                not stat.S_ISREG(item.st_mode)
                or item.st_uid != os.getuid()
                or item.st_mode & 0o077
            ):
                raise PermissionError("audit log must be an owner-only regular file")
            if os.write(fd, line) != len(line):
                raise OSError("short audit write")
        finally:
            os.close(fd)

    def rotate(self) -> None:
        try:
            size = self.path.lstat().st_size
        except FileNotFoundError:
            return
        if size < ROTATE_BYTES:
            return
        for index in range(KEEP, 1, -1):
            older = self.path.with_name(f"{self.path.name}.{index - 1}")
            if older.exists():
                older.replace(self.path.with_name(f"{self.path.name}.{index}"))
        self.path.replace(self.path.with_name(self.path.name + ".1"))

    def best_effort(self, event: str, **fields: object) -> None:
        """Record a revoke, lock or grant end; a failed write never blocks it."""
        try:
            self.write(event, **fields)
        except OSError as exc:
            sys.stderr.write(f"trusty-browser: audit write failed for {event}: {exc}\n")

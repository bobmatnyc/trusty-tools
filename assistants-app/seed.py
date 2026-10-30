"""Seeds the default assistants (Slice 1.2).

Why: a fresh install needs assistants to chat with before any UI exists.
What: creates the database if missing and inserts "General" and "Code Helper"
unless an assistant with that name already exists, so re-runs add nothing.
Test: `test_seed_is_idempotent` in `assistants-app/tests/test_db.py`.
"""

from __future__ import annotations

import os
import sys

import db

DEFAULT_ASSISTANTS: tuple[tuple[str, str], ...] = (
    (
        "General",
        "You are a helpful general-purpose assistant. Answer clearly and "
        "concisely, ask a clarifying question when a request is ambiguous, "
        "and say so when you do not know something.",
    ),
    (
        "Code Helper",
        "You are a coding assistant. Write correct, idiomatic, tested code. "
        "Explain the cause of a bug before proposing a fix, prefer small "
        "focused changes, and state the language and version you assume.",
    ),
)


def seed(db_path: str | os.PathLike[str] | None = None) -> list[str]:
    """Inserts missing default assistants; returns the names it created."""
    db.init_db(db_path)
    existing = {a["name"] for a in db.list_assistants(db_path)}
    created: list[str] = []
    for name, instructions in DEFAULT_ASSISTANTS:
        if name not in existing:
            db.create_assistant(name, instructions, db.DEFAULT_MODEL, db_path)
            created.append(name)
    return created


def main() -> int:
    created = seed()
    path = db.resolve_db_path()
    if created:
        print(f"Created {', '.join(created)} in {path}")
    else:
        print(f"Default assistants already present in {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

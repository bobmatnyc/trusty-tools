"""SQLite storage for assistants (Slice 1.2).

Why: the prototype server needs persistent assistants and a message log for
token and cost tracking, without a database server.
What: thin functions over the standard-library `sqlite3` module. Every call
opens its own connection with foreign keys on, so `delete_assistant` cascades
to that assistant's messages.
Test: `assistants-app/tests/test_db.py`.

The database path resolves, in order: the `db_path` argument, the
`ASSISTANTS_DB_PATH` environment variable, then `assistants.db` next to this
file.
"""

from __future__ import annotations

import os
import sqlite3
from contextlib import closing
from pathlib import Path
from typing import Any

APP_DIR = Path(__file__).resolve().parent
SCHEMA_PATH = APP_DIR / "schema.sql"
DEFAULT_DB_PATH = APP_DIR / "assistants.db"
DEFAULT_MODEL = "us.anthropic.claude-sonnet-5-5"

Assistant = dict[str, Any]


def resolve_db_path(db_path: str | os.PathLike[str] | None = None) -> Path:
    """Returns the database path from the argument, env var, or default."""
    if db_path is not None:
        return Path(db_path)
    env_path = os.environ.get("ASSISTANTS_DB_PATH")
    return Path(env_path) if env_path else DEFAULT_DB_PATH


def connect(db_path: str | os.PathLike[str] | None = None) -> sqlite3.Connection:
    """Opens a connection with foreign keys enforced and dict-like rows."""
    conn = sqlite3.connect(resolve_db_path(db_path))
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    return conn


def init_db(db_path: str | os.PathLike[str] | None = None) -> None:
    """Creates the tables from `schema.sql`; safe to call repeatedly."""
    schema = SCHEMA_PATH.read_text(encoding="utf-8")
    with closing(connect(db_path)) as conn, conn:
        conn.executescript(schema)


def create_assistant(
    name: str,
    instructions: str,
    model: str = DEFAULT_MODEL,
    db_path: str | os.PathLike[str] | None = None,
) -> int:
    """Inserts an assistant and returns its id.

    Raises `sqlite3.IntegrityError` when `name` is already taken.
    """
    with closing(connect(db_path)) as conn, conn:
        cur = conn.execute(
            "INSERT INTO assistants (name, instructions, model) VALUES (?, ?, ?)",
            (name, instructions, model),
        )
        assert cur.lastrowid is not None
        return cur.lastrowid


def list_assistants(db_path: str | os.PathLike[str] | None = None) -> list[Assistant]:
    """Returns every assistant, ordered by id."""
    with closing(connect(db_path)) as conn:
        rows = conn.execute("SELECT * FROM assistants ORDER BY id").fetchall()
    return [dict(row) for row in rows]


def get_assistant(
    assistant_id: int, db_path: str | os.PathLike[str] | None = None
) -> Assistant | None:
    """Returns one assistant, or None when the id does not exist."""
    with closing(connect(db_path)) as conn:
        row = conn.execute(
            "SELECT * FROM assistants WHERE id = ?", (assistant_id,)
        ).fetchone()
    return dict(row) if row is not None else None


def update_assistant(
    assistant_id: int,
    name: str,
    instructions: str,
    model: str,
    db_path: str | os.PathLike[str] | None = None,
) -> bool:
    """Replaces an assistant's fields; returns False when the id does not exist."""
    with closing(connect(db_path)) as conn, conn:
        cur = conn.execute(
            "UPDATE assistants SET name = ?, instructions = ?, model = ? WHERE id = ?",
            (name, instructions, model, assistant_id),
        )
        return cur.rowcount == 1


def delete_assistant(
    assistant_id: int, db_path: str | os.PathLike[str] | None = None
) -> bool:
    """Deletes an assistant and its messages; returns False when the id does not exist."""
    with closing(connect(db_path)) as conn, conn:
        cur = conn.execute("DELETE FROM assistants WHERE id = ?", (assistant_id,))
        return cur.rowcount == 1


def add_message(
    assistant_id: int,
    role: str,
    content: str,
    tokens_in: int = 0,
    tokens_out: int = 0,
    cost_usd: float = 0.0,
    db_path: str | os.PathLike[str] | None = None,
) -> int:
    """Appends a message to an assistant's log and returns its id.

    Raises `sqlite3.IntegrityError` for an unknown assistant or a role other
    than "user" or "assistant".
    """
    with closing(connect(db_path)) as conn, conn:
        cur = conn.execute(
            "INSERT INTO messages"
            " (assistant_id, role, content, tokens_in, tokens_out, cost_usd)"
            " VALUES (?, ?, ?, ?, ?, ?)",
            (assistant_id, role, content, tokens_in, tokens_out, cost_usd),
        )
        assert cur.lastrowid is not None
        return cur.lastrowid


def list_messages(
    assistant_id: int, db_path: str | os.PathLike[str] | None = None
) -> list[dict[str, Any]]:
    """Returns an assistant's messages, oldest first."""
    with closing(connect(db_path)) as conn:
        rows = conn.execute(
            "SELECT * FROM messages WHERE assistant_id = ? ORDER BY id",
            (assistant_id,),
        ).fetchall()
    return [dict(row) for row in rows]

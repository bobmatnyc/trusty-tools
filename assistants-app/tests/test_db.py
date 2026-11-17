"""Tests for `db.py` CRUD and `seed.py`, each against a temporary database."""

from __future__ import annotations

import sqlite3
from contextlib import closing
from pathlib import Path

import pytest

import db
import seed

MISSING_ID = 9999


def test_init_db_is_idempotent(db_path: Path) -> None:
    db.init_db(db_path)
    assert db.list_assistants(db_path) == []


def test_create_assistant_returns_id_and_stores_fields(db_path: Path) -> None:
    new_id = db.create_assistant("Writer", "Write well.", "m-1", db_path)

    got = db.get_assistant(new_id, db_path)

    assert got is not None
    assert got["id"] == new_id
    assert (got["name"], got["instructions"], got["model"]) == (
        "Writer",
        "Write well.",
        "m-1",
    )
    assert got["created_at"]


def test_create_assistant_defaults_to_inference_profile(db_path: Path) -> None:
    new_id = db.create_assistant("Writer", "Write well.", db_path=db_path)

    got = db.get_assistant(new_id, db_path)

    assert got is not None
    assert got["model"] == "us.anthropic.claude-sonnet-5-5"


def test_create_assistant_rejects_duplicate_name(db_path: Path) -> None:
    db.create_assistant("Writer", "a", db_path=db_path)

    with pytest.raises(sqlite3.IntegrityError):
        db.create_assistant("Writer", "b", db_path=db_path)


def test_list_assistants_returns_all_in_id_order(db_path: Path) -> None:
    first = db.create_assistant("A", "a", db_path=db_path)
    second = db.create_assistant("B", "b", db_path=db_path)

    rows = db.list_assistants(db_path)

    assert [r["id"] for r in rows] == [first, second]
    assert [r["name"] for r in rows] == ["A", "B"]


def test_get_assistant_returns_none_when_missing(db_path: Path) -> None:
    assert db.get_assistant(MISSING_ID, db_path) is None


def test_update_assistant_replaces_fields(db_path: Path) -> None:
    new_id = db.create_assistant("Old", "old", "m-1", db_path)

    assert db.update_assistant(new_id, "New", "new", "m-2", db_path) is True

    got = db.get_assistant(new_id, db_path)
    assert got is not None
    assert (got["name"], got["instructions"], got["model"]) == ("New", "new", "m-2")


def test_update_assistant_returns_false_when_missing(db_path: Path) -> None:
    assert db.update_assistant(MISSING_ID, "x", "y", "z", db_path) is False


def test_delete_assistant_removes_row_and_its_messages(db_path: Path) -> None:
    keep = db.create_assistant("Keep", "k", db_path=db_path)
    gone = db.create_assistant("Gone", "g", db_path=db_path)
    with closing(db.connect(db_path)) as conn, conn:
        conn.execute(
            "INSERT INTO messages (assistant_id, role, content) VALUES (?, 'user', 'hi')",
            (gone,),
        )

    assert db.delete_assistant(gone, db_path) is True

    assert db.get_assistant(gone, db_path) is None
    assert [r["id"] for r in db.list_assistants(db_path)] == [keep]
    with closing(db.connect(db_path)) as conn:
        count = conn.execute("SELECT COUNT(*) FROM messages").fetchone()[0]
    assert count == 0


def test_delete_assistant_returns_false_when_missing(db_path: Path) -> None:
    assert db.delete_assistant(MISSING_ID, db_path) is False


def test_messages_role_is_constrained(db_path: Path) -> None:
    new_id = db.create_assistant("A", "a", db_path=db_path)
    with closing(db.connect(db_path)) as conn:
        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                "INSERT INTO messages (assistant_id, role, content) "
                "VALUES (?, 'system', 'x')",
                (new_id,),
            )


def test_db_path_comes_from_env_var(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    env_path = tmp_path / "env.db"
    monkeypatch.setenv("ASSISTANTS_DB_PATH", str(env_path))

    db.init_db()
    db.create_assistant("Env", "e")

    assert env_path.exists()
    assert [r["name"] for r in db.list_assistants(env_path)] == ["Env"]


def test_seed_is_idempotent(tmp_path: Path) -> None:
    path = tmp_path / "seed.db"

    assert seed.seed(path) == ["General", "Code Helper"]
    assert seed.seed(path) == []

    rows = db.list_assistants(path)
    assert [r["name"] for r in rows] == ["General", "Code Helper"]
    assert all(r["model"] == "us.anthropic.claude-sonnet-5-5" for r in rows)

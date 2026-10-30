"""Puts `assistants-app/` on the import path and gives each test a fresh DB."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import db  # noqa: E402


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "test.db"
    db.init_db(path)
    return path

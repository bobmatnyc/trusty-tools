#!/usr/bin/env python3
"""Builds `cto_fixture.db` — the FIXTURE database bundled with this skill.

READ THIS BEFORE TRUSTING ANY NUMBER FROM THIS FILE'S OUTPUT: every row
here is invented sample data for structural/testability purposes only. It is
NOT a copy, sample, or export of Duetto's real CTO operations database
(`~/Duetto/cto/data/cto.db`), which this sandbox has no access to and could
not verify even if it did. See the skill's `README.md` for the full
disclosure and the swap-in instructions for the real database.

Why a committed script instead of a committed-only binary blob: regenerating
`cto_fixture.db` from this script keeps the fixture's provenance auditable
(diff the script, not a binary) and reproducible on any machine with
Python's stdlib `sqlite3`. Run `python3 build_fixture.py` from this
directory to regenerate the committed `cto_fixture.db` after editing the
sample rows below.

Schema: matches `crates/trusty-cto-db/src/lib.rs`'s in-memory test schema
(`person`, `rd_budget_2026`, `user_work_distribution`, view
`v_needs_review`) — the closest thing this repo has to a schema spec for
"the db", since that Rust module documents the real file's expected shape
even though it was never wired into a running binary.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

FIXTURE_PATH = Path(__file__).resolve().parent / "cto_fixture.db"

SCHEMA_SQL = """
CREATE TABLE person (
    person_id INTEGER PRIMARY KEY,
    full_name TEXT,
    team TEXT,
    department TEXT,
    title TEXT,
    level TEXT,
    employment_type TEXT,
    status TEXT,
    contractor_source TEXT
);

CREATE TABLE rd_budget_2026 (
    team TEXT,
    organization TEXT,
    status TEXT,
    annual_cost REAL,
    cy_26_total REAL
);

CREATE TABLE user_work_distribution (
    person_id INTEGER,
    year INTEGER,
    month INTEGER,
    work_type TEXT,
    product_category TEXT,
    work_units REAL,
    percentage REAL
);

CREATE VIEW v_needs_review AS
    SELECT 'repo' AS entity_type, 'sample-repo-1' AS entity_id,
           'detection' AS classification, 0.42 AS confidence,
           'llm' AS classification_source, '2026-01-01' AS classified_at
    UNION ALL
    SELECT 'repo', 'sample-repo-2', 'platform', 0.61, 'llm', '2026-02-01'
    UNION ALL
    SELECT 'repo', 'sample-repo-3', 'core', 0.88, 'llm', '2026-03-01'
    UNION ALL
    SELECT 'commit', 'sample-commit-9', 'unclassified', 0.31, 'llm', '2026-04-01';
"""

PERSON_ROWS = [
    # (id, full_name, team, department, title, level, employment_type, status, contractor_source)
    (1, "Ada Fixture", "Pricing", "Engineering", "SWE", "IC4", "FTE", "active", None),
    (2, "Grace Sample", "Pricing", "Engineering", "SWE", "IC3", "FTE", "active", None),
    (3, "Alan Placeholder", "Forecasting", "Engineering", "SWE", "IC5", "Contractor", "active", "Acme Staffing"),
    (4, "Margaret Testdata", "Forecasting", "Engineering", "EM", "M1", "FTE", "active", None),
    (5, "Edsger Mockrow", "Platform", "Engineering", "SWE", "IC2", "FTE", "departed", None),
    (6, "Barbara Fictional", "Platform", "Engineering", "SWE", "IC4", "FTE", "active", None),
]

BUDGET_ROWS = [
    # (team, organization, status, annual_cost, cy_26_total)
    ("Pricing", "Engineering", "Active", 210000.0, 210000.0),
    ("Pricing", "Engineering", "Active", 195000.0, 195000.0),
    ("Forecasting", "Engineering", "Active", 225000.0, 225000.0),
    ("Forecasting", "Engineering", "Active", 140000.0, 140000.0),
    ("Platform", "Engineering", "Active", 260000.0, 260000.0),
    ("GTM Sample Team", "GTM", "Active", 155000.0, 155000.0),
]

WORK_ROWS = [
    # (person_id, year, month, work_type, product_category, work_units, percentage)
    (1, 2026, 6, "feature", "pricing", 8.0, 80.0),
    (1, 2026, 6, "bugfix", "pricing", 2.0, 20.0),
    (2, 2026, 6, "feature", "pricing", 9.5, 95.0),
    (2, 2026, 6, "oncall", "pricing", 0.5, 5.0),
    (3, 2026, 6, "platform", "forecasting", 6.0, 100.0),
    (4, 2026, 6, "planning", "forecasting", 4.0, 40.0),
    (4, 2026, 6, "feature", "forecasting", 6.0, 60.0),
    (6, 2026, 6, "platform", "platform", 10.0, 100.0),
]


def build(path: Path = FIXTURE_PATH) -> None:
    if path.exists():
        path.unlink()
    conn = sqlite3.connect(path)
    try:
        conn.executescript(SCHEMA_SQL)
        conn.executemany(
            "INSERT INTO person VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)", PERSON_ROWS
        )
        conn.executemany(
            "INSERT INTO rd_budget_2026 VALUES (?, ?, ?, ?, ?)", BUDGET_ROWS
        )
        conn.executemany(
            "INSERT INTO user_work_distribution VALUES (?, ?, ?, ?, ?, ?, ?)", WORK_ROWS
        )
        conn.commit()
    finally:
        conn.close()


if __name__ == "__main__":
    build()
    print(f"wrote fixture db to {FIXTURE_PATH}")

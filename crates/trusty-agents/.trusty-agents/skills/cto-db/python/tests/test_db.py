"""Unit tests for `cto_db_skill.db` against an in-memory seeded database.

Why: Fast, isolated tests should never touch the committed fixture file on
disk — seeding an in-memory `sqlite3` connection with a small, known dataset
keeps every test deterministic and independent of `build_fixture.py`'s
sample rows drifting over time.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

from cto_db_skill import db


@pytest.fixture()
def conn() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.row_factory = sqlite3.Row
    connection.executescript(
        """
        CREATE TABLE person (
            person_id INTEGER PRIMARY KEY,
            full_name TEXT, team TEXT, department TEXT, title TEXT, level TEXT,
            employment_type TEXT, status TEXT, contractor_source TEXT
        );
        INSERT INTO person VALUES
            (1,'Alice','Pricing','Engineering','SWE','IC4','FTE','active',NULL),
            (2,'Bob',  'Pricing','Engineering','SWE','IC3','FTE','active',NULL),
            (3,'Carol','Forecasting','Engineering','SWE','IC5','Contractor','active','Sherpany'),
            (4,'Dave', 'Pricing','Engineering','SWE','IC2','FTE','departed',NULL);

        CREATE TABLE rd_budget_2026 (
            team TEXT, organization TEXT, status TEXT,
            annual_cost REAL, cy_26_total REAL
        );
        INSERT INTO rd_budget_2026 VALUES
            ('Pricing','Engineering','Active', 200000.0, 200000.0),
            ('Pricing','Engineering','Active', 180000.0, 180000.0),
            ('Forecasting','Engineering','Active', 220000.0, 220000.0),
            ('Sales',   'GTM',        'Active', 150000.0, 150000.0);

        CREATE TABLE user_work_distribution (
            person_id INTEGER, year INTEGER, month INTEGER,
            work_type TEXT, product_category TEXT,
            work_units REAL, percentage REAL
        );
        INSERT INTO user_work_distribution VALUES
            (1, 2026, 4, 'feature',  'pricing', 8.0,  80.0),
            (1, 2026, 4, 'bugfix',   'pricing', 2.0,  20.0),
            (2, 2026, 4, 'feature',  'pricing', 10.0, 100.0),
            (3, 2026, 4, 'platform', 'forecasting', 5.0, 100.0);

        CREATE VIEW v_needs_review AS
            SELECT 'repo' AS entity_type, 'r1' AS entity_id,
                   'detection' AS classification, 0.4 AS confidence,
                   'llm' AS classification_source, '2026-01-01' AS classified_at
            UNION ALL
            SELECT 'repo','r2','platform',0.65,'llm','2026-01-01'
            UNION ALL
            SELECT 'repo','r3','core',0.85,'llm','2026-01-01';
        """
    )
    try:
        yield connection
    finally:
        connection.close()


# --- query_headcount -----------------------------------------------------


def test_query_headcount_filter_by_team(conn: sqlite3.Connection) -> None:
    result = db.query_headcount(conn, "team")
    groups = {g["team"]: g["headcount"] for g in result["groups"]}
    assert groups["Pricing"] == 2  # Dave is departed, excluded
    assert groups["Forecasting"] == 1


def test_query_headcount_filter_by_status(conn: sqlite3.Connection) -> None:
    result = db.query_headcount(conn, "status")
    groups = {g["employment_type"]: g["headcount"] for g in result["groups"]}
    assert groups["FTE"] == 2
    assert groups["Contractor"] == 1


def test_query_headcount_filter_by_vendor(conn: sqlite3.Connection) -> None:
    result = db.query_headcount(conn, "vendor")
    groups = {g["vendor"]: g["headcount"] for g in result["groups"]}
    assert groups["Sherpany"] == 1


def test_query_headcount_no_filter_returns_people_list(conn: sqlite3.Connection) -> None:
    result = db.query_headcount(conn, None)
    assert result["filter_by"] is None
    names = {p["full_name"] for p in result["people"]}
    assert names == {"Alice", "Bob", "Carol"}  # Dave (departed) excluded


def test_query_headcount_rejects_unknown_filter(conn: sqlite3.Connection) -> None:
    with pytest.raises(db.UnknownFilterError):
        db.query_headcount(conn, "bogus")


# --- query_budget ----------------------------------------------------------


def test_query_budget_team_filter(conn: sqlite3.Connection) -> None:
    result = db.query_budget(conn, team="Pricing")
    assert len(result["rows"]) == 1
    row = result["rows"][0]
    assert row["team"] == "Pricing"
    assert row["headcount"] == 2
    assert abs(row["cy_26_total"] - 380000.0) < 0.01


def test_query_budget_no_filters_returns_all_teams(conn: sqlite3.Connection) -> None:
    result = db.query_budget(conn)
    assert len(result["rows"]) == 3  # Pricing, Forecasting, Sales


def test_query_budget_category_filter(conn: sqlite3.Connection) -> None:
    result = db.query_budget(conn, category="GTM")
    assert len(result["rows"]) == 1
    assert result["rows"][0]["team"] == "Sales"


# --- query_risks -------------------------------------------------------


def test_query_risks_filters_by_severity(conn: sqlite3.Connection) -> None:
    result = db.query_risks(conn, "high")
    assert len(result["risks"]) == 1
    assert result["risks"][0]["entity_id"] == "r1"
    assert result["risks"][0]["severity"] == "high"


def test_query_risks_no_filter_returns_all(conn: sqlite3.Connection) -> None:
    result = db.query_risks(conn)
    assert len(result["risks"]) == 3


def test_query_risks_rejects_unknown_severity(conn: sqlite3.Connection) -> None:
    with pytest.raises(db.UnknownFilterError):
        db.query_risks(conn, "critical")


def test_query_risks_degrades_gracefully_when_view_missing() -> None:
    bare_conn = sqlite3.connect(":memory:")
    bare_conn.row_factory = sqlite3.Row
    try:
        result = db.query_risks(bare_conn, None)
        assert result["risks"] == []
        assert "unavailable" in result["source"]
    finally:
        bare_conn.close()


# --- query_work_classification ------------------------------------------


def test_query_work_classification_pod_filter(conn: sqlite3.Connection) -> None:
    result = db.query_work_classification(conn, "Pricing")
    work_types = {r["work_type"] for r in result["rows"]}
    assert work_types == {"feature", "bugfix"}
    assert all(r["pod"] == "Pricing" for r in result["rows"])


def test_query_work_classification_no_pod_returns_all(conn: sqlite3.Connection) -> None:
    result = db.query_work_classification(conn)
    pods = {r["pod"] for r in result["rows"]}
    assert pods == {"Pricing", "Forecasting"}


# --- dispatch ------------------------------------------------------------


def test_dispatch_routes_to_query_headcount(conn: sqlite3.Connection) -> None:
    result = db.dispatch(conn, "query_headcount", {"filter_by": "team"})
    assert "groups" in result


def test_dispatch_unknown_tool_raises(conn: sqlite3.Connection) -> None:
    with pytest.raises(ValueError):
        db.dispatch(conn, "nope", {})


# --- path resolution -------------------------------------------------------


def test_resolve_db_path_honours_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(db.ENV_CTO_DB_PATH, "/tmp/custom-cto.db")
    assert str(db.resolve_db_path()) == "/tmp/custom-cto.db"


# #4860: the fixture must never be the silent default — an unconfigured skill
# refuses rather than answering a headcount question from invented sample data.
def test_resolve_db_path_refuses_when_unconfigured(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(db.ENV_CTO_DB_PATH, raising=False)
    monkeypatch.delenv(db.ENV_CTO_DB_USE_FIXTURE, raising=False)
    with pytest.raises(db.UnconfiguredDatabaseError) as excinfo:
        db.resolve_db_path()
    message = str(excinfo.value)
    assert db.ENV_CTO_DB_PATH in message
    assert db.ENV_CTO_DB_USE_FIXTURE in message


def test_resolve_db_path_serves_fixture_only_on_explicit_opt_in(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv(db.ENV_CTO_DB_PATH, raising=False)
    monkeypatch.setenv(db.ENV_CTO_DB_USE_FIXTURE, "1")
    assert db.resolve_db_path() == db.DEFAULT_FIXTURE_DB_PATH


@pytest.mark.parametrize("value", ["", "0", "false", "no", "off", "  FALSE  "])
def test_fixture_opt_in_rejects_falsey_values(
    monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    monkeypatch.delenv(db.ENV_CTO_DB_PATH, raising=False)
    monkeypatch.setenv(db.ENV_CTO_DB_USE_FIXTURE, value)
    with pytest.raises(db.UnconfiguredDatabaseError):
        db.resolve_db_path()


def test_real_path_wins_over_the_fixture_opt_in(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(db.ENV_CTO_DB_PATH, "/tmp/custom-cto.db")
    monkeypatch.setenv(db.ENV_CTO_DB_USE_FIXTURE, "1")
    assert str(db.resolve_db_path()) == "/tmp/custom-cto.db"


def test_is_fixture_path_labels_the_bundled_fixture() -> None:
    assert db.is_fixture_path(db.DEFAULT_FIXTURE_DB_PATH) is True
    assert db.is_fixture_path(Path("/tmp/custom-cto.db")) is False


def test_open_readonly_rejects_missing_file(tmp_path) -> None:
    missing = tmp_path / "does-not-exist.db"
    with pytest.raises(sqlite3.OperationalError):
        db.open_readonly(missing)


def test_fixture_db_matches_expected_schema() -> None:
    """The committed fixture must actually load and expose the four tools'
    tables/view — a regression guard for `build_fixture.py` drifting out of
    sync with `db.py`'s expected schema."""
    connection = db.open_readonly(db.DEFAULT_FIXTURE_DB_PATH)
    try:
        result = db.query_headcount(connection, "team")
        assert result["groups"], "fixture db must seed at least one active team"
    finally:
        connection.close()

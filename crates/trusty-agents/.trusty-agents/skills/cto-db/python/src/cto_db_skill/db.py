"""Read-only query functions over the CTO ops SQLite database.

Why: Reimplements, in Python, the four query functions that previously
existed ONLY as an unwired Rust `ToolExecutor` cluster
(`crates/trusty-cto-db` + `crates/tc-services::cto_db` +
`crates/cto-assistant`, all still present on disk but never invoked by any
built binary — see the crate-level docstring for the DOC-41 rationale for
not resurrecting that Rust path). The schema below is NOT a fresh guess: it
matches the schema already documented (and unit-tested against, with an
in-memory fixture) in `crates/trusty-cto-db/src/lib.rs`, which is the closest
thing this repo has to a spec for "the db". That Rust module's own doc
comments describe the real file as `~/Duetto/cto/data/cto.db` — a
Duetto-internal SQLite file on the owner's machine, inaccessible to this
sandbox and unverifiable here.

What: Four functions — `query_headcount`, `query_budget`, `query_risks`,
`query_work_classification` — each taking a `sqlite3.Connection` plus
optional filters and returning a JSON-serialisable `dict`. `resolve_db_path`
/ `open_readonly` centralise path resolution and read-only connection
opening so `cli.py` (the subprocess entrypoint) and the test suite share one
code path. #4860: `resolve_db_path` refuses when nothing is configured — the
bundled fixture is served only when `$CTO_DB_USE_FIXTURE` asks for it.

Test: `tests/test_db.py` seeds an in-memory SQLite database with the same
shape as `fixtures/build_fixture.py` and exercises every function's filters,
including the unknown-filter/unknown-severity error paths.
"""

from __future__ import annotations

import os
import sqlite3
from pathlib import Path
from typing import Any

# Overrides the resolved DB path entirely. Named to match the pre-existing
# (unwired) Rust crate's `ENV_CTO_DB_PATH` constant
# (`crates/trusty-cto-db/src/lib.rs`) so an operator who later has access to
# the real `~/Duetto/cto/data/cto.db` can point this skill at it with zero
# code changes, matching the naming an operator might already know from that
# module's README/comments.
ENV_CTO_DB_PATH = "CTO_DB_PATH"

# Opt-in to the bundled fixture. #4860: the fixture used to be the silent
# default, so an unconfigured skill answered "how many people do we have?"
# with invented sample data and no way for the caller to tell. Serving it now
# requires asking for it by name.
ENV_CTO_DB_USE_FIXTURE = "CTO_DB_USE_FIXTURE"

# The bundled fixture, shipped alongside this package. Reachable only via
# `$CTO_DB_USE_FIXTURE` — see this directory's README.md for the loud "this is
# fixture data, not the real Duetto CTO ops DB" disclosure.
_SKILL_ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_FIXTURE_DB_PATH = _SKILL_ROOT / "fixtures" / "cto_fixture.db"

# Spellings that turn the fixture opt-in OFF. Anything else non-empty turns it
# on, so `CTO_DB_USE_FIXTURE=1` and `=true` both work.
_FIXTURE_OPT_OUT_VALUES = frozenset({"", "0", "false", "no", "off"})


class UnknownFilterError(ValueError):
    """Raised when a caller passes a filter value outside the documented enum."""


class UnconfiguredDatabaseError(RuntimeError):
    """Raised when no database is configured and the fixture was not opted into.

    Why: #4860 — a confident wrong answer is worse than an error. The caller
    must see a refusal it can surface, not sample data dressed as a real
    headcount.
    """


def _fixture_opt_in() -> bool:
    value = os.environ.get(ENV_CTO_DB_USE_FIXTURE, "").strip().lower()
    return value not in _FIXTURE_OPT_OUT_VALUES


def resolve_db_path() -> Path:
    """Resolve which SQLite file to open, or refuse.

    Why: #4860 — falling back to the bundled fixture with nothing configured
    let the four query tools return well-formed answers built from invented
    data, indistinguishable from a real one. The fixture stays reachable for
    tests and local development, but only when asked for explicitly.
    What: Returns `$CTO_DB_PATH` if set and non-empty; else
    `DEFAULT_FIXTURE_DB_PATH` when `$CTO_DB_USE_FIXTURE` opts in; else raises
    `UnconfiguredDatabaseError`.
    Test: `test_resolve_db_path_honours_env_override`,
    `test_resolve_db_path_refuses_when_unconfigured`,
    `test_resolve_db_path_serves_fixture_only_on_explicit_opt_in`,
    `test_fixture_opt_in_rejects_falsey_values`,
    `test_real_path_wins_over_the_fixture_opt_in`.
    """
    override = os.environ.get(ENV_CTO_DB_PATH, "").strip()
    if override:
        return Path(override)
    if _fixture_opt_in():
        return DEFAULT_FIXTURE_DB_PATH
    raise UnconfiguredDatabaseError(
        f"no CTO database configured: set {ENV_CTO_DB_PATH} to the real "
        f"database, or {ENV_CTO_DB_USE_FIXTURE}=1 to query the bundled "
        "fixture (invented sample data, not real Duetto figures)"
    )


def is_fixture_path(path: Path) -> bool:
    """Whether `path` is the bundled fixture rather than a real database.

    Why: #4860 — every response carries this so fixture output stays
    identifiable downstream, even once someone opts in.
    Test: `test_is_fixture_path_labels_the_bundled_fixture`.
    """
    return Path(path) == DEFAULT_FIXTURE_DB_PATH


def open_readonly(path: Path) -> sqlite3.Connection:
    """Open `path` in SQLite read-only (URI) mode.

    Why: This skill must never mutate the CTO database — read-only mode
    turns an accidental write into a hard `sqlite3.OperationalError` instead
    of silent corruption of data this skill doesn't own.
    What: Uses the `file:...?mode=ro` URI form so a missing file raises
    immediately rather than SQLite silently creating an empty one (the
    default behaviour of a plain `sqlite3.connect(path)`).
    Test: `test_open_readonly_rejects_missing_file`.
    """
    uri = f"file:{path}?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def _rows_to_dicts(rows: list[sqlite3.Row]) -> list[dict[str, Any]]:
    return [dict(row) for row in rows]


# =========================================================================
# Tool: query_headcount
# =========================================================================

_HEADCOUNT_FILTERS = ("team", "status", "vendor")


def query_headcount(conn: sqlite3.Connection, filter_by: str | None = None) -> dict[str, Any]:
    """Headcount summary from the `person` table.

    `filter_by` groups active-person counts: "team" | "status" (employment
    type) | "vendor" (contractor source). Omit it to get a flat, capped list
    of active people instead of an aggregate.
    """
    if filter_by is None:
        rows = conn.execute(
            """
            SELECT full_name, team, department, title, level,
                   employment_type, status, contractor_source
            FROM person
            WHERE status = 'active'
            ORDER BY department, team, full_name
            LIMIT 500
            """
        ).fetchall()
        return {"filter_by": None, "people": _rows_to_dicts(rows)}

    if filter_by == "team":
        rows = conn.execute(
            """
            SELECT COALESCE(team, '<unassigned>') AS team,
                   COUNT(*) AS headcount
            FROM person
            WHERE status = 'active'
            GROUP BY team
            ORDER BY headcount DESC
            """
        ).fetchall()
        return {"filter_by": "team", "groups": _rows_to_dicts(rows)}

    if filter_by == "status":
        rows = conn.execute(
            """
            SELECT COALESCE(employment_type, '<unknown>') AS employment_type,
                   COUNT(*) AS headcount
            FROM person
            WHERE status = 'active'
            GROUP BY employment_type
            ORDER BY headcount DESC
            """
        ).fetchall()
        return {"filter_by": "status", "groups": _rows_to_dicts(rows)}

    if filter_by == "vendor":
        rows = conn.execute(
            """
            SELECT COALESCE(contractor_source, '<unknown>') AS vendor,
                   COUNT(*) AS headcount
            FROM person
            WHERE status = 'active' AND employment_type = 'Contractor'
            GROUP BY contractor_source
            ORDER BY headcount DESC
            """
        ).fetchall()
        return {"filter_by": "vendor", "groups": _rows_to_dicts(rows)}

    raise UnknownFilterError(
        f"unknown filter_by '{filter_by}'; expected one of: {', '.join(_HEADCOUNT_FILTERS)}"
    )


# =========================================================================
# Tool: query_budget
# =========================================================================


def query_budget(
    conn: sqlite3.Connection,
    team: str | None = None,
    category: str | None = None,
) -> dict[str, Any]:
    """2026 R&D budget breakdown from `rd_budget_2026`.

    Optional `team` filters `rd_budget_2026.team`; optional `category`
    filters `rd_budget_2026.organization` (the budget table's answer to
    "category"). Active rows only.
    """
    sql = """
        SELECT COALESCE(team, '<unassigned>') AS team,
               COALESCE(organization, '<unassigned>') AS organization,
               COUNT(*) AS headcount,
               ROUND(SUM(annual_cost), 2) AS annual_cost_total,
               ROUND(SUM(cy_26_total), 2) AS cy_26_total
        FROM rd_budget_2026
        WHERE (status IS NULL OR status = 'Active' OR status = 'active')
    """
    params: list[str] = []
    if team:
        sql += " AND team = ?"
        params.append(team)
    if category:
        sql += " AND organization = ?"
        params.append(category)
    sql += " GROUP BY team, organization ORDER BY cy_26_total DESC"

    rows = conn.execute(sql, params).fetchall()
    return {"team": team, "category": category, "rows": _rows_to_dicts(rows)}


# =========================================================================
# Tool: query_risks
# =========================================================================

_RISK_SEVERITIES = ("high", "medium", "low")


def query_risks(conn: sqlite3.Connection, severity: str | None = None) -> dict[str, Any]:
    """Risk register proxy: `v_needs_review` bucketed into high/medium/low.

    Why: the schema has no first-class "risks" table; `v_needs_review`
    (low-confidence entity classifications) is the closest proxy available,
    same rationale as the original Rust implementation this mirrors.
    """
    if severity is not None and severity not in _RISK_SEVERITIES:
        raise UnknownFilterError(
            f"unknown severity '{severity}'; expected one of: {', '.join(_RISK_SEVERITIES)}"
        )

    try:
        rows = conn.execute(
            """
            SELECT entity_type, entity_id, classification, confidence,
                   classification_source, classified_at,
                   CASE
                     WHEN confidence < 0.50 THEN 'high'
                     WHEN confidence < 0.70 THEN 'medium'
                     ELSE 'low'
                   END AS severity
            FROM v_needs_review
            ORDER BY confidence ASC
            LIMIT 200
            """
        ).fetchall()
    except sqlite3.OperationalError as exc:
        # `v_needs_review` may not exist in every snapshot of cto.db (legacy
        # schemas). Degrade to an empty, clearly-labelled result rather than
        # raising, so the agent can still say "no risk data available".
        return {
            "source": "v_needs_review unavailable in this database snapshot",
            "severity": severity,
            "risks": [],
            "note": f"v_needs_review failed: {exc}",
        }

    results = _rows_to_dicts(rows)
    if severity is not None:
        results = [r for r in results if r["severity"] == severity]

    return {
        "source": "v_needs_review (classification-confidence proxy; no dedicated risk register)",
        "severity": severity,
        "risks": results,
    }


# =========================================================================
# Tool: query_work_classification
# =========================================================================


def query_work_classification(conn: sqlite3.Connection, pod: str | None = None) -> dict[str, Any]:
    """Work-type breakdown for the most recent month of `user_work_distribution`.

    Optional `pod` filters `person.team` (this schema uses "team" as the
    pod-level grouping, matching the original implementation).
    """
    base_sql = """
        WITH latest AS (
            SELECT uwd.person_id, MAX(uwd.year * 100 + uwd.month) AS ym
            FROM user_work_distribution uwd
            GROUP BY uwd.person_id
        )
        SELECT
            COALESCE(p.team, '<unassigned>') AS pod,
            uwd.work_type,
            ROUND(SUM(uwd.work_units), 2) AS work_units,
            ROUND(AVG(uwd.percentage), 2) AS avg_percentage,
            COUNT(DISTINCT uwd.person_id) AS people
        FROM user_work_distribution uwd
        JOIN person p ON p.person_id = uwd.person_id
        JOIN latest l
          ON l.person_id = uwd.person_id
         AND (uwd.year * 100 + uwd.month) = l.ym
        WHERE p.status = 'active'
    """
    params: list[str] = []
    if pod:
        sql = base_sql + " AND p.team = ? GROUP BY p.team, uwd.work_type ORDER BY work_units DESC"
        params.append(pod)
    else:
        sql = base_sql + " GROUP BY p.team, uwd.work_type ORDER BY p.team, work_units DESC"

    rows = conn.execute(sql, params).fetchall()
    return {"pod": pod, "rows": _rows_to_dicts(rows)}


# =========================================================================
# Dispatch
# =========================================================================

TOOL_NAMES = (
    "query_headcount",
    "query_budget",
    "query_risks",
    "query_work_classification",
)


def dispatch(conn: sqlite3.Connection, name: str, args: dict[str, Any]) -> dict[str, Any]:
    """Route a `{name, args}` call to the matching query function.

    Why: `cli.py` needs one call site shared with the test suite so both
    exercise identical dispatch behaviour.
    Test: `test_dispatch_unknown_tool_raises`, plus one smoke test per tool.
    """

    def opt_str(key: str) -> str | None:
        v = args.get(key)
        if isinstance(v, str) and v:
            return v
        return None

    if name == "query_headcount":
        return query_headcount(conn, opt_str("filter_by"))
    if name == "query_budget":
        return query_budget(conn, opt_str("team"), opt_str("category"))
    if name == "query_risks":
        return query_risks(conn, opt_str("severity"))
    if name == "query_work_classification":
        return query_work_classification(conn, opt_str("pod"))
    raise ValueError(f"unknown tool: {name}")

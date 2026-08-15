//! `trusty-cto-db` — read-only query tools over the CTO ops SQLite database.
//!
//! Why: The CTO assistant in `trusty-agents` needs structured access to the
//! headcount / budget / work-classification data that the original Python
//! CTO bot loaded out of `~/Duetto/cto/data/cto.db`. Exposing raw SQL to
//! the LLM is unsafe (injection, schema drift, expensive queries), so this
//! crate wraps a small set of curated, read-only queries behind a JSON
//! tool surface. The dispatcher (`handle_tool_call`) returns plain
//! `serde_json::Value` so the same code path drives both an MCP server
//! and direct in-process calls from trusty-agents's tool registry.
//! What: Opens `cto.db` in read-only mode (`SQLITE_OPEN_READ_ONLY`),
//! exposes four tools — `query_headcount`, `query_budget`, `query_risks`,
//! `query_work_classification` — each of which builds a parameterised
//! query and returns structured JSON. No table data is mutated.
//! Test: `cargo test -p trusty-cto-db` runs in-memory SQLite tests that
//! seed a minimal schema and assert that filters narrow rows correctly.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags, Row, types::Value as SqlValue};
use serde_json::{Map, Value, json};

/// Environment variable that overrides the default DB path.
pub const ENV_CTO_DB_PATH: &str = "CTO_DB_PATH";

/// Resolve the SQLite database path.
///
/// Why: Local-dev (`~/Duetto/cto/data/cto.db`) and CI (custom path via env)
/// both need to work without code changes. Centralising avoids subtle
/// path-handling bugs.
/// What: Honours `CTO_DB_PATH` first, then falls back to
/// `$HOME/Duetto/cto/data/cto.db`.
/// Test: `resolves_env_override` / `resolves_home_default`.
pub fn resolve_db_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_CTO_DB_PATH)
        && !p.is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve $HOME"))?;
    Ok(home.join("Duetto/cto/data/cto.db"))
}

/// Open `cto.db` read-only.
///
/// Why: This crate must never mutate the CTO database — it is the source
/// of truth for budget/headcount and is maintained out-of-band. Opening
/// with `READ_ONLY` makes accidental writes a hard error instead of silent
/// data corruption.
/// What: Returns a `rusqlite::Connection` with the read-only flag set.
/// Test: `open_readonly_rejects_writes`.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open cto.db at {}", path.display()))
}

/// Convert a single SQLite row into a JSON object using the connection's
/// column names.
///
/// Why: Every query in this crate returns its rows as JSON objects so the
/// agent gets self-describing payloads (no positional surprises if a
/// column is added to the underlying table/view).
/// What: Maps each column name to a JSON scalar matching its SQLite type
/// affinity (text → string, integer → number, real → number, null → null,
/// blob → base64-omitted-as-null).
/// Test: Exercised by every `query_*` integration test.
fn row_to_json(row: &Row<'_>, columns: &[String]) -> rusqlite::Result<Value> {
    let mut obj = Map::with_capacity(columns.len());
    for (idx, name) in columns.iter().enumerate() {
        let v: SqlValue = row.get(idx)?;
        let json_v = match v {
            SqlValue::Null => Value::Null,
            SqlValue::Integer(i) => Value::from(i),
            SqlValue::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
            SqlValue::Text(s) => Value::String(s),
            SqlValue::Blob(_) => Value::Null,
        };
        obj.insert(name.clone(), json_v);
    }
    Ok(Value::Object(obj))
}

/// Run `sql` with the given positional params and collect every row as a
/// JSON object.
fn query_all(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("prepare: {sql}"))?;
    let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_json(row, &columns)?);
    }
    Ok(out)
}

// =====================================================================
// Tool: query_headcount
// =====================================================================

/// Why: The CTO assistant frequently answers "how many ICs on $team?",
/// "who's a contractor?", "who is active right now?". Surfacing the
/// `person` table behind a filter parameter avoids exposing raw SQL.
/// What: Returns active people only (`status='active'`) with the columns
/// most useful for an agent. `filter_by` controls the grouping:
///   - `"team"`     → group by `team`, returning `{ team, headcount }`
///   - `"status"`   → group by `employment_type` (FTE / Contractor / Intern)
///   - `"vendor"`   → group by `contractor_source` (filtered to contractors)
///   - `None`       → return the row-level list of people (capped at 500)
///
/// Test: `query_headcount_filter_by_team`.
pub fn query_headcount(conn: &Connection, filter_by: Option<&str>) -> Result<Value> {
    match filter_by {
        Some("team") => {
            let rows = query_all(
                conn,
                "SELECT COALESCE(team, '<unassigned>') AS team, \
                        COUNT(*) AS headcount \
                 FROM person \
                 WHERE status = 'active' \
                 GROUP BY team \
                 ORDER BY headcount DESC",
                &[],
            )?;
            Ok(json!({ "filter_by": "team", "groups": rows }))
        }
        Some("status") => {
            let rows = query_all(
                conn,
                "SELECT COALESCE(employment_type, '<unknown>') AS employment_type, \
                        COUNT(*) AS headcount \
                 FROM person \
                 WHERE status = 'active' \
                 GROUP BY employment_type \
                 ORDER BY headcount DESC",
                &[],
            )?;
            Ok(json!({ "filter_by": "status", "groups": rows }))
        }
        Some("vendor") => {
            let rows = query_all(
                conn,
                "SELECT COALESCE(contractor_source, '<unknown>') AS vendor, \
                        COUNT(*) AS headcount \
                 FROM person \
                 WHERE status = 'active' AND employment_type = 'Contractor' \
                 GROUP BY contractor_source \
                 ORDER BY headcount DESC",
                &[],
            )?;
            Ok(json!({ "filter_by": "vendor", "groups": rows }))
        }
        None => {
            let rows = query_all(
                conn,
                "SELECT full_name, team, department, title, level, \
                        employment_type, status, contractor_source \
                 FROM person \
                 WHERE status = 'active' \
                 ORDER BY department, team, full_name \
                 LIMIT 500",
                &[],
            )?;
            Ok(json!({ "filter_by": null, "people": rows }))
        }
        Some(other) => Err(anyhow!(
            "unknown filter_by '{other}'; expected one of: team, status, vendor"
        )),
    }
}

// =====================================================================
// Tool: query_budget
// =====================================================================

/// Why: Budget questions ("what's eng spending in Q2?", "how much is
/// $team allocated?") are a primary CTO-bot use case. The 2026 R&D budget
/// lives in `rd_budget_2026` keyed by department / organization / team /
/// role.
/// What: Optional `team` filters `rd_budget_2026.team`; optional
/// `category` filters `rd_budget_2026.organization` (the budget table's
/// answer to "category"). Returns aggregated cy_26_total + monthly
/// breakdown by team. Active rows only (`status` in `'Active' | NULL`).
/// Test: `query_budget_team_filter`.
pub fn query_budget(
    conn: &Connection,
    team: Option<&str>,
    category: Option<&str>,
) -> Result<Value> {
    let mut sql = String::from(
        "SELECT COALESCE(team, '<unassigned>') AS team, \
                COALESCE(organization, '<unassigned>') AS organization, \
                COUNT(*) AS headcount, \
                ROUND(SUM(annual_cost), 2) AS annual_cost_total, \
                ROUND(SUM(cy_26_total), 2) AS cy_26_total \
         FROM rd_budget_2026 \
         WHERE (status IS NULL OR status = 'Active' OR status = 'active')",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(t) = team {
        sql.push_str(" AND team = ?");
        params.push(Box::new(t.to_string()));
    }
    if let Some(c) = category {
        sql.push_str(" AND organization = ?");
        params.push(Box::new(c.to_string()));
    }
    sql.push_str(" GROUP BY team, organization ORDER BY cy_26_total DESC");

    let param_refs: Vec<&dyn rusqlite::ToSql> = params
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let rows = query_all(conn, &sql, &param_refs)?;
    Ok(json!({
        "team": team,
        "category": category,
        "rows": rows,
    }))
}

// =====================================================================
// Tool: query_risks
// =====================================================================

/// Why: The original Python CTO bot exposed a "risk register" surface.
/// The current SQLite schema has no first-class `risks` table, but it
/// does carry a `v_needs_review` view that captures low-confidence
/// repo/commit classifications — a reasonable proxy for "things the org
/// should look at". Returning *something* (with a clear `source` field)
/// is more useful to the agent than a hard error.
/// What: Reads `v_needs_review` and buckets rows into severity bands
/// based on `confidence`:
///   - confidence < 0.50 → `"high"`
///   - 0.50 ≤ confidence < 0.70 → `"medium"`
///   - confidence ≥ 0.70 → `"low"`
///
/// If `severity` is provided, only rows in that band are returned.
/// Test: `query_risks_filters_by_severity`.
pub fn query_risks(conn: &Connection, severity: Option<&str>) -> Result<Value> {
    // Guard: refuse unknown severity values up-front for a clear error.
    if let Some(s) = severity
        && !matches!(s, "high" | "medium" | "low")
    {
        return Err(anyhow!(
            "unknown severity '{s}'; expected one of: high, medium, low"
        ));
    }

    let sql = "SELECT entity_type, entity_id, classification, confidence, \
                      classification_source, classified_at, \
                      CASE \
                        WHEN confidence < 0.50 THEN 'high' \
                        WHEN confidence < 0.70 THEN 'medium' \
                        ELSE 'low' \
                      END AS severity \
               FROM v_needs_review \
               ORDER BY confidence ASC \
               LIMIT 200";
    // The `v_needs_review` view depends on tables that may not exist in
    // every snapshot of cto.db (legacy `commit_classification__old`). If
    // the view fails to resolve, return an empty result with a note rather
    // than propagating a SQL error — the agent should still be able to say
    // "no risk data available" instead of crashing.
    let rows = match query_all(conn, sql, &[]) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("query_risks: v_needs_review unavailable: {e:#}");
            return Ok(json!({
                "source": "v_needs_review unavailable in this database snapshot",
                "severity": severity,
                "risks": Vec::<Value>::new(),
                "note": format!("v_needs_review failed: {e}"),
            }));
        }
    };
    let filtered: Vec<Value> = match severity {
        Some(s) => rows
            .into_iter()
            .filter(|r| r.get("severity").and_then(|v| v.as_str()) == Some(s))
            .collect(),
        None => rows,
    };
    Ok(json!({
        "source": "v_needs_review (classification-confidence proxy; no dedicated risk register)",
        "severity": severity,
        "risks": filtered,
    }))
}

// =====================================================================
// Tool: query_work_classification
// =====================================================================

/// Why: "What is $pod actually working on?" — answered by aggregating
/// `user_work_distribution` (monthly per-person work-type / product
/// breakdown) up to the team level.
/// What: If `pod` is provided, filters `person.team` (this DB uses "team"
/// as the pod-level grouping). Aggregates the *most recent* (year, month)
/// snapshot per person, then sums work-units and averages percentages
/// across the chosen pod's people, grouped by `work_type`.
/// Test: `query_work_classification_pod_filter`.
pub fn query_work_classification(conn: &Connection, pod: Option<&str>) -> Result<Value> {
    // The view already joins to `person`; we filter on `department` /
    // `title` only when a pod is supplied. We approximate "pod" as the
    // `person.team` field. The view lacks `team`, so we join manually.
    let base_sql = "WITH latest AS ( \
            SELECT uwd.person_id, MAX(uwd.year * 100 + uwd.month) AS ym \
            FROM user_work_distribution uwd \
            GROUP BY uwd.person_id \
        ) \
        SELECT \
            COALESCE(p.team, '<unassigned>') AS pod, \
            uwd.work_type, \
            ROUND(SUM(uwd.work_units), 2) AS work_units, \
            ROUND(AVG(uwd.percentage), 2) AS avg_percentage, \
            COUNT(DISTINCT uwd.person_id) AS people \
        FROM user_work_distribution uwd \
        JOIN person p ON p.person_id = uwd.person_id \
        JOIN latest l \
          ON l.person_id = uwd.person_id \
         AND (uwd.year * 100 + uwd.month) = l.ym \
        WHERE p.status = 'active'";
    let (sql, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match pod {
        Some(p) => (
            format!(
                "{base_sql} AND p.team = ? GROUP BY p.team, uwd.work_type ORDER BY work_units DESC"
            ),
            vec![Box::new(p.to_string())],
        ),
        None => (
            format!("{base_sql} GROUP BY p.team, uwd.work_type ORDER BY p.team, work_units DESC"),
            Vec::new(),
        ),
    };
    let param_refs: Vec<&dyn rusqlite::ToSql> = params
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let rows = query_all(conn, &sql, &param_refs)?;
    Ok(json!({ "pod": pod, "rows": rows }))
}

// =====================================================================
// Tool dispatch (matches the OMPM-RPC/1 contract used by trusty-agents)
// =====================================================================

/// Static JSON-Schema for every tool this crate exposes.
///
/// Why: trusty-agents's tool registry consumes a list of `{name, description,
/// inputSchema}` objects identical to the MCP `tools/list` contract.
/// What: Returns one entry per query function with documented optional
/// parameters.
/// Test: `tool_list_has_four_tools`.
pub fn tool_list_response() -> Value {
    json!({
        "tools": [
            {
                "name": "query_headcount",
                "description": "Headcount summary from the CTO ops DB. \
                    `filter_by` groups counts: 'team' | 'status' | 'vendor'. \
                    Omit to get a flat list of active people (capped at 500).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter_by": {
                            "type": "string",
                            "enum": ["team", "status", "vendor"],
                        },
                    },
                },
            },
            {
                "name": "query_budget",
                "description": "2026 R&D budget breakdown. Optional `team` filters \
                    rd_budget_2026.team; optional `category` filters \
                    rd_budget_2026.organization.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "team":     { "type": "string" },
                        "category": { "type": "string" },
                    },
                },
            },
            {
                "name": "query_risks",
                "description": "Risk register proxy: low-confidence classifications \
                    from v_needs_review, bucketed into high/medium/low by confidence. \
                    Filter with `severity` ∈ {high, medium, low}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "severity": {
                            "type": "string",
                            "enum": ["high", "medium", "low"],
                        },
                    },
                },
            },
            {
                "name": "query_work_classification",
                "description": "Work-type breakdown for the most recent month of \
                    user_work_distribution data. Optional `pod` filters person.team.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pod": { "type": "string" },
                    },
                },
            },
        ],
    })
}

/// Single entry point that routes a `{name, args}` call to the right
/// query function.
///
/// Why: trusty-agents's tool dispatcher calls one Rust function per tool;
/// centralising the match here keeps the surface auditable.
/// What: Opens (and re-uses, per call) a read-only connection, dispatches
/// by tool name, and returns the JSON the tool produced.
/// Test: `dispatch_query_headcount_smoke`.
pub fn handle_tool_call(name: &str, args: &Value) -> Result<Value> {
    let db_path = resolve_db_path()?;
    let conn = open_readonly(&db_path)?;
    dispatch(&conn, name, args)
}

/// Pure dispatch — used by tests with an in-memory `Connection`.
pub fn dispatch(conn: &Connection, name: &str, args: &Value) -> Result<Value> {
    let opt_str = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    match name {
        "query_headcount" => query_headcount(conn, opt_str("filter_by")),
        "query_budget" => query_budget(conn, opt_str("team"), opt_str("category")),
        "query_risks" => query_risks(conn, opt_str("severity")),
        "query_work_classification" => query_work_classification(conn, opt_str("pod")),
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

// =====================================================================
// Tests — seed an in-memory SQLite and exercise each tool
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-mem");
        conn.execute_batch(
            "CREATE TABLE person (
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
            ",
        )
        .expect("seed");
        conn
    }

    #[test]
    fn resolves_env_override() {
        // Save existing value to restore so other tests in this binary
        // aren't affected.
        let prev = std::env::var(ENV_CTO_DB_PATH).ok();
        unsafe {
            std::env::set_var(ENV_CTO_DB_PATH, "/tmp/custom-cto.db");
        }
        assert_eq!(
            resolve_db_path().unwrap(),
            PathBuf::from("/tmp/custom-cto.db")
        );
        unsafe {
            match prev {
                Some(p) => std::env::set_var(ENV_CTO_DB_PATH, p),
                None => std::env::remove_var(ENV_CTO_DB_PATH),
            }
        }
    }

    #[test]
    fn query_headcount_filter_by_team() {
        let conn = seed();
        let v = query_headcount(&conn, Some("team")).unwrap();
        let groups = v["groups"].as_array().unwrap();
        // Pricing=2 active, Forecasting=1 active (Dave is departed)
        let pricing = groups
            .iter()
            .find(|g| g["team"] == "Pricing")
            .expect("Pricing");
        assert_eq!(pricing["headcount"], 2);
        let forecasting = groups
            .iter()
            .find(|g| g["team"] == "Forecasting")
            .expect("Forecasting");
        assert_eq!(forecasting["headcount"], 1);
    }

    #[test]
    fn query_headcount_filter_by_status() {
        let conn = seed();
        let v = query_headcount(&conn, Some("status")).unwrap();
        let groups = v["groups"].as_array().unwrap();
        let fte = groups
            .iter()
            .find(|g| g["employment_type"] == "FTE")
            .unwrap();
        assert_eq!(fte["headcount"], 2);
    }

    #[test]
    fn query_headcount_rejects_unknown_filter() {
        let conn = seed();
        assert!(query_headcount(&conn, Some("bogus")).is_err());
    }

    #[test]
    fn query_budget_team_filter() {
        let conn = seed();
        let v = query_budget(&conn, Some("Pricing"), None).unwrap();
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["team"], "Pricing");
        assert_eq!(rows[0]["headcount"], 2);
        assert!((rows[0]["cy_26_total"].as_f64().unwrap() - 380000.0).abs() < 0.01);
    }

    #[test]
    fn query_budget_no_filters_returns_all_teams() {
        let conn = seed();
        let v = query_budget(&conn, None, None).unwrap();
        assert_eq!(v["rows"].as_array().unwrap().len(), 3); // Pricing, Forecasting, Sales
    }

    #[test]
    fn query_risks_filters_by_severity() {
        let conn = seed();
        let v = query_risks(&conn, Some("high")).unwrap();
        let risks = v["risks"].as_array().unwrap();
        assert_eq!(risks.len(), 1);
        assert_eq!(risks[0]["entity_id"], "r1");
        assert_eq!(risks[0]["severity"], "high");
    }

    #[test]
    fn query_risks_rejects_unknown_severity() {
        let conn = seed();
        assert!(query_risks(&conn, Some("critical")).is_err());
    }

    #[test]
    fn query_work_classification_pod_filter() {
        let conn = seed();
        let v = query_work_classification(&conn, Some("Pricing")).unwrap();
        let rows = v["rows"].as_array().unwrap();
        // Pricing has feature + bugfix
        assert!(rows.iter().any(|r| r["work_type"] == "feature"));
        assert!(rows.iter().any(|r| r["work_type"] == "bugfix"));
        // Forecasting should NOT appear
        assert!(rows.iter().all(|r| r["pod"] == "Pricing"));
    }

    #[test]
    fn tool_list_has_four_tools() {
        let v = tool_list_response();
        assert_eq!(v["tools"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn dispatch_query_headcount_smoke() {
        let conn = seed();
        let v = dispatch(&conn, "query_headcount", &json!({ "filter_by": "team" })).unwrap();
        assert!(v["groups"].is_array());
    }

    #[test]
    fn dispatch_unknown_tool_errors() {
        let conn = seed();
        assert!(dispatch(&conn, "nope", &json!({})).is_err());
    }
}

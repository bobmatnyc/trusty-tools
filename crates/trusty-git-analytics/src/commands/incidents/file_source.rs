//! `tga incidents collect --source file` — generic JSON/CSV incident
//! ingestion (issue #2204).
//!
//! Why: cto-reports writes `fact_incidents` rows directly into `tga.db`
//! via raw `sqlite3` because its incident export (a custom
//! `{"investigations": [...], "days_analyzed": N}` shape) matches none of
//! the three Datadog envelope shapes `ingest_datadog` recognises. This
//! module gives it (and any other downstream consumer) a documented,
//! generic incident-file contract instead of a Datadog-specific one.
//!
//! What: accepts a JSON or CSV file (dispatched by extension) with a
//! generic incident record schema:
//!
//! ```text
//! incident_id, detected_at, resolved_at, severity, repo,
//! triggering_deploy, jira_ticket
//! ```
//!
//! JSON shape — a top-level object with an `incidents` array:
//!
//! ```json
//! {
//!   "incidents": [
//!     {
//!       "incident_id": "INC-1234",
//!       "detected_at": "2026-01-01T00:00:00Z",
//!       "resolved_at": "2026-01-01T02:30:00Z",
//!       "severity": "P1",
//!       "repo": "myrepo"
//!     }
//!   ]
//! }
//! ```
//!
//! CSV shape — a header row with those 7 column names (any order), with
//! optional columns left blank rather than omitted:
//!
//! ```text
//! incident_id,detected_at,resolved_at,severity,repo,triggering_deploy,jira_ticket
//! ```
//!
//! `incident_id` and `detected_at` are required; `detected_at` and
//! `resolved_at` (when present) must be RFC3339 timestamps. `mttr_hours`
//! is derived the same way the JIRA/Datadog paths do (hours between
//! `detected_at` and `resolved_at`, when both are present). `source` is
//! always recorded as `"file"`. Malformed rows fail the whole ingest with
//! a row-numbered error rather than being silently skipped.

use std::path::Path;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Deserialize;

use tga::core::db::Database;

/// One parsed incident record from a `--source file` JSON/CSV document.
///
/// Why: JSON and CSV parsing converge on the same flat shape before
/// validation/insertion, so the ingest loop only has to know one type.
/// What: a generic incident record — deliberately smaller than
/// `fact_incidents`'s full column set (no `source`, which is always
/// `"file"` for this path).
/// Test: covered by every `parse_json_*` / `parse_csv_*` test in
/// `file_source_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct FileIncidentRecord {
    pub incident_id: String,
    pub detected_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub triggering_deploy: Option<String>,
    #[serde(default)]
    pub jira_ticket: Option<String>,
}

/// JSON document wrapper: `{ "incidents": [ ... ] }`.
#[derive(Debug, Deserialize)]
struct FileIncidentsDoc {
    incidents: Vec<FileIncidentRecord>,
}

/// Ingest a `--source file` incident export into `fact_incidents`.
///
/// Why: the single entry point `incidents/mod.rs::run` dispatches to when
/// `--source file` is requested, mirroring the `(scanned, inserted)`
/// return shape of `ingest_jira_sre` / `ingest_datadog` so the CLI summary
/// line stays consistent across sources.
/// What: parses `path` (JSON or CSV, chosen by extension), validates
/// every record (required fields present, timestamps parse as RFC3339),
/// derives `mttr_hours` when both `detected_at`/`resolved_at` are
/// present, then `INSERT OR REPLACE`s each row inside one transaction —
/// matching the idempotent-by-replace semantics the JIRA and Datadog
/// paths already use for `fact_incidents`. A malformed document or
/// record aborts the whole ingest with a descriptive error.
/// Test: `ingest_file_*` tests in `file_source_tests.rs`.
///
/// # Errors
///
/// Returns an error if the file cannot be read, does not parse as
/// JSON/CSV, has an unsupported extension, or any record fails
/// validation (missing required field or an unparseable timestamp).
pub(super) fn ingest_file(db: &mut Database, path: &Path) -> anyhow::Result<(usize, usize)> {
    let records = load_records(path)?;
    let scanned = records.len();
    let mut inserted = 0usize;

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO fact_incidents \
             (incident_id, source, detected_at, resolved_at, mttr_hours, severity, \
              triggering_deploy, repo, jira_ticket) \
             VALUES (?1, 'file', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (idx, rec) in records.iter().enumerate() {
            let row_ctx = format!("record #{} (incident_id={:?})", idx + 1, rec.incident_id);
            if rec.incident_id.trim().is_empty() {
                anyhow::bail!("{row_ctx}: incident_id must not be empty");
            }
            let detected_at = parse_rfc3339(&rec.detected_at, "detected_at", &row_ctx)?;
            let resolved_at = rec
                .resolved_at
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| parse_rfc3339(s, "resolved_at", &row_ctx))
                .transpose()?;
            let mttr_hours = resolved_at.map(|r: DateTime<Utc>| {
                (r.signed_duration_since(detected_at).num_seconds() as f64) / 3600.0
            });

            insert.execute(params![
                rec.incident_id,
                detected_at.to_rfc3339(),
                resolved_at.map(|d: DateTime<Utc>| d.to_rfc3339()),
                mttr_hours,
                rec.severity,
                rec.triggering_deploy,
                rec.repo,
                rec.jira_ticket,
            ])?;
            inserted += 1;
        }
    }
    tx.commit()?;
    Ok((scanned, inserted))
}

/// Parse an RFC3339 timestamp with a row-numbered, field-named error on
/// failure — the "clear errors on malformed input" contract issue #2204
/// asks for.
fn parse_rfc3339(s: &str, field: &str, row_ctx: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| anyhow!("{row_ctx}: {field} {s:?} is not a valid RFC3339 timestamp: {e}"))
}

/// Dispatch to the JSON or CSV loader based on the file extension.
fn load_records(path: &Path) -> anyhow::Result<Vec<FileIncidentRecord>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => load_json(path),
        Some("csv") => load_csv(path),
        other => anyhow::bail!(
            "unsupported incidents file extension {other:?} for {}: expected .json or .csv",
            path.display()
        ),
    }
}

/// Parse the `{ "incidents": [...] }` JSON document.
fn load_json(path: &Path) -> anyhow::Result<Vec<FileIncidentRecord>> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read incidents file: {}", path.display()))?;
    let doc: FileIncidentsDoc = serde_json::from_str(&body).with_context(|| {
        format!(
            "failed to parse {} as an incidents JSON document \
             (expected {{\"incidents\": [...]}})",
            path.display()
        )
    })?;
    Ok(doc.incidents)
}

/// Required CSV column names, in the order the writer function binds them.
const CSV_COLUMNS: [&str; 7] = [
    "incident_id",
    "detected_at",
    "resolved_at",
    "severity",
    "repo",
    "triggering_deploy",
    "jira_ticket",
];

/// Parse the incidents CSV export.
///
/// Why: hand-rolled (rather than `csv`'s serde struct derive) so every
/// error names the exact row and column, and so blank cells map cleanly
/// to `None` regardless of column position.
fn load_csv(path: &Path) -> anyhow::Result<Vec<FileIncidentRecord>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .with_context(|| format!("failed to open incidents CSV file: {}", path.display()))?;
    let headers = rdr.headers()?.clone();

    let mut col_idx = std::collections::HashMap::new();
    for name in CSV_COLUMNS {
        if let Some(pos) = headers.iter().position(|h| h == name) {
            col_idx.insert(name, pos);
        }
    }
    for required in ["incident_id", "detected_at"] {
        if !col_idx.contains_key(required) {
            anyhow::bail!(
                "{}: CSV header is missing required column '{required}' \
                 (expected columns: {})",
                path.display(),
                CSV_COLUMNS.join(", ")
            );
        }
    }

    let get = |record: &csv::StringRecord, col: &str| -> Option<String> {
        col_idx
            .get(col)
            .and_then(|&i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let mut out = Vec::new();
    for (row_num, result) in rdr.records().enumerate() {
        let record = result.with_context(|| {
            format!(
                "{}: malformed CSV at data row {}",
                path.display(),
                row_num + 1
            )
        })?;
        out.push(FileIncidentRecord {
            incident_id: get(&record, "incident_id").unwrap_or_default(),
            detected_at: get(&record, "detected_at").unwrap_or_default(),
            resolved_at: get(&record, "resolved_at"),
            severity: get(&record, "severity"),
            repo: get(&record, "repo"),
            triggering_deploy: get(&record, "triggering_deploy"),
            jira_ticket: get(&record, "jira_ticket"),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "file_source_tests.rs"]
mod tests;

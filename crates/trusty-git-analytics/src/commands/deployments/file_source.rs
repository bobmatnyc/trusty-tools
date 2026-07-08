//! `tga deployments collect --source file` — generic JSON/CSV deployment
//! ingestion (issue #2204).
//!
//! Why: cto-reports writes `fact_deployments` rows directly into `tga.db`
//! via raw `sqlite3` because none of the existing sources (`git_tags`,
//! `github_releases`, `github_actions`) accept its
//! `analytics.duckdb::fact_ci_pipelines`-derived export. This module gives
//! it (and any other downstream consumer) a documented, stable file
//! contract that goes through the same `INSERT OR IGNORE` idempotency
//! guarantee as every other source.
//!
//! What: accepts a JSON or CSV file (dispatched by extension) whose schema
//! is a flat mirror of the `fact_deployments` columns:
//!
//! ```text
//! deploy_id, repo, environment, triggered_at, completed_at, status,
//! git_sha, git_tag, triggered_by_pr, source
//! ```
//!
//! JSON shape — a top-level object with a `deployments` array:
//!
//! ```json
//! {
//!   "deployments": [
//!     {
//!       "deploy_id": "myrepo@2026-01-01T00:00:00Z",
//!       "repo": "myrepo",
//!       "environment": "production",
//!       "triggered_at": "2026-01-01T00:00:00Z",
//!       "completed_at": "2026-01-01T00:05:00Z",
//!       "status": "success",
//!       "git_sha": "abc123",
//!       "git_tag": "v1.2.3",
//!       "triggered_by_pr": 42,
//!       "source": "ci_pipeline"
//!     }
//!   ]
//! }
//! ```
//!
//! CSV shape — a header row with exactly those 10 column names (any order),
//! with optional columns left blank rather than omitted:
//!
//! ```text
//! deploy_id,repo,environment,triggered_at,completed_at,status,git_sha,git_tag,triggered_by_pr,source
//! ```
//!
//! `deploy_id`, `repo`, `triggered_at`, and `status` are required;
//! `triggered_at` and `completed_at` must be RFC3339 timestamps.
//! `environment` defaults to `"production"` and `source` defaults to
//! `"file"` when blank/absent. Malformed rows fail the whole ingest with a
//! row-numbered error rather than being silently skipped — this is an
//! explicit, operator-controlled file contract, not a best-effort external
//! API scrape.

use std::path::Path;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Deserialize;

use tga::core::db::Database;

use super::CollectStats;

/// One parsed deployment record from a `--source file` JSON/CSV document.
///
/// Why: JSON and CSV parsing converge on the same flat shape before
/// validation/insertion, so the ingest loop only has to know one type.
/// What: mirrors the `fact_deployments` columns 1:1; optional columns are
/// `Option<...>` and blank/absent in the source document.
/// Test: covered by every `parse_json_*` / `parse_csv_*` test in
/// `file_source_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct FileDeploymentRecord {
    pub deploy_id: String,
    pub repo: String,
    #[serde(default)]
    pub environment: Option<String>,
    pub triggered_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub git_tag: Option<String>,
    #[serde(default)]
    pub triggered_by_pr: Option<i64>,
    #[serde(default)]
    pub source: Option<String>,
}

/// JSON document wrapper: `{ "deployments": [ ... ] }`.
#[derive(Debug, Deserialize)]
struct FileDeploymentsDoc {
    deployments: Vec<FileDeploymentRecord>,
}

/// Ingest a `--source file` deployment export into `fact_deployments`.
///
/// Why: the single entry point `deployments/mod.rs::run` dispatches to,
/// mirroring the shape of `ingest_git_tags` so the CLI summary line stays
/// consistent across sources.
/// What: parses `path` (JSON or CSV, chosen by extension), validates every
/// record (required fields present, timestamps parse as RFC3339), then
/// `INSERT OR IGNORE`s each row inside one transaction. A malformed
/// document or record aborts the whole ingest with a descriptive error —
/// deliberately fail-fast since this is an operator-controlled file, not a
/// best-effort external scrape.
/// Test: `ingest_file_*` tests in `file_source_tests.rs`.
///
/// # Errors
///
/// Returns an error if the file cannot be read, does not parse as JSON/CSV,
/// has an unsupported extension, or any record fails validation (missing
/// required field or an unparseable timestamp).
pub(super) fn ingest_file(db: &mut Database, path: &Path) -> anyhow::Result<CollectStats> {
    let records = load_records(path)?;
    let mut stats = CollectStats::default();

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO fact_deployments \
             (deploy_id, repo, environment, triggered_at, completed_at, \
              status, git_sha, git_tag, triggered_by_pr, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (idx, rec) in records.iter().enumerate() {
            let row_ctx = format!("record #{} (deploy_id={:?})", idx + 1, rec.deploy_id);
            if rec.deploy_id.trim().is_empty() {
                anyhow::bail!("{row_ctx}: deploy_id must not be empty");
            }
            if rec.repo.trim().is_empty() {
                anyhow::bail!("{row_ctx}: repo must not be empty");
            }
            if rec.status.trim().is_empty() {
                anyhow::bail!("{row_ctx}: status must not be empty");
            }
            let triggered_at = parse_rfc3339(&rec.triggered_at, "triggered_at", &row_ctx)?;
            let completed_at = rec
                .completed_at
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| parse_rfc3339(s, "completed_at", &row_ctx))
                .transpose()?;
            let environment = rec
                .environment
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("production");
            let source = rec
                .source
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("file");

            let changed = insert.execute(params![
                rec.deploy_id,
                rec.repo,
                environment,
                triggered_at.to_rfc3339(),
                completed_at.map(|d: DateTime<Utc>| d.to_rfc3339()),
                rec.status,
                rec.git_sha,
                rec.git_tag,
                rec.triggered_by_pr,
                source,
            ])?;
            if changed > 0 {
                stats.inserted += 1;
            } else {
                stats.skipped += 1;
            }
        }
    }
    tx.commit()?;
    Ok(stats)
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
fn load_records(path: &Path) -> anyhow::Result<Vec<FileDeploymentRecord>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => load_json(path),
        Some("csv") => load_csv(path),
        other => anyhow::bail!(
            "unsupported deployments file extension {other:?} for {}: expected .json or .csv",
            path.display()
        ),
    }
}

/// Parse the `{ "deployments": [...] }` JSON document.
fn load_json(path: &Path) -> anyhow::Result<Vec<FileDeploymentRecord>> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read deployments file: {}", path.display()))?;
    let doc: FileDeploymentsDoc = serde_json::from_str(&body).with_context(|| {
        format!(
            "failed to parse {} as a deployments JSON document \
             (expected {{\"deployments\": [...]}})",
            path.display()
        )
    })?;
    Ok(doc.deployments)
}

/// Required CSV column names, in the order the writer function binds them.
const CSV_COLUMNS: [&str; 10] = [
    "deploy_id",
    "repo",
    "environment",
    "triggered_at",
    "completed_at",
    "status",
    "git_sha",
    "git_tag",
    "triggered_by_pr",
    "source",
];

/// Parse the deployments CSV export.
///
/// Why: hand-rolled (rather than `csv`'s serde struct derive) so every
/// error names the exact row and column, and so blank cells map cleanly to
/// `None` regardless of column position.
fn load_csv(path: &Path) -> anyhow::Result<Vec<FileDeploymentRecord>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .with_context(|| format!("failed to open deployments CSV file: {}", path.display()))?;
    let headers = rdr.headers()?.clone();

    let mut col_idx = std::collections::HashMap::new();
    for name in CSV_COLUMNS {
        if let Some(pos) = headers.iter().position(|h| h == name) {
            col_idx.insert(name, pos);
        }
    }
    for required in ["deploy_id", "repo", "triggered_at", "status"] {
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
        let triggered_by_pr = match get(&record, "triggered_by_pr") {
            Some(s) => Some(s.parse::<i64>().map_err(|e| {
                anyhow!(
                    "{}: data row {}: triggered_by_pr {s:?} is not an integer: {e}",
                    path.display(),
                    row_num + 1
                )
            })?),
            None => None,
        };
        out.push(FileDeploymentRecord {
            deploy_id: get(&record, "deploy_id").unwrap_or_default(),
            repo: get(&record, "repo").unwrap_or_default(),
            environment: get(&record, "environment"),
            triggered_at: get(&record, "triggered_at").unwrap_or_default(),
            completed_at: get(&record, "completed_at"),
            status: get(&record, "status").unwrap_or_default(),
            git_sha: get(&record, "git_sha"),
            git_tag: get(&record, "git_tag"),
            triggered_by_pr,
            source: get(&record, "source"),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "file_source_tests.rs"]
mod tests;

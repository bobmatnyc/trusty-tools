//! `tga incidents collect` — ingest production incidents into
//! `fact_incidents` for MTTR (issue #213).
//!
//! Two paths are supported:
//!
//! 1. **JIRA SRE quick-win** (default): query the `work_items` table for
//!    SRE-flagged issues (`project = 'SRE'` AND
//!    `type IN ('Bug', 'Incident')`) and project each into a
//!    `fact_incidents` row. This path requires no external services and
//!    is the recommended starting point for MTTR.
//!
//! 2. **Datadog directory** (stubbed): when `dora.datadog_dir` is set,
//!    walk every `.json` file in the directory and project Datadog's
//!    incident schema into `fact_incidents`. This path documents what's
//!    needed; the actual parsing is left to whoever has live Datadog
//!    data on hand because the JSON shape varies by export tool.

use chrono::{DateTime, Utc};
use clap::Args;
use rusqlite::params;
use tracing::{info, warn};

use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga incidents collect`.
#[derive(Args, Debug)]
pub struct IncidentsCollectArgs {
    /// Restrict ingestion to a single source (`jira`, `datadog`). When
    /// unset, every configured source is consulted.
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
}

/// Per-run counters surfaced on the CLI output.
#[derive(Debug, Default, Clone)]
struct CollectStats {
    jira_scanned: usize,
    jira_inserted: usize,
    datadog_files: usize,
    datadog_inserted: usize,
}

/// Dispatch entry point for `tga incidents collect`.
///
/// # Errors
///
/// Propagates DB errors from the underlying ingestors.
pub fn run(config: Config, db: &mut Database, args: IncidentsCollectArgs) -> anyhow::Result<()> {
    let mut stats = CollectStats::default();
    let restrict = args.source.as_deref();

    // MSRV is 1.75 — `Option::is_none_or` only stabilised in 1.82, so
    // expand the predicate manually.
    if matches!(restrict, None | Some("jira")) {
        let (scanned, inserted) = ingest_jira_sre(db)?;
        stats.jira_scanned = scanned;
        stats.jira_inserted = inserted;
    }
    if matches!(restrict, None | Some("datadog")) {
        let (files, inserted) = ingest_datadog(db, &config)?;
        stats.datadog_files = files;
        stats.datadog_inserted = inserted;
    }

    println!(
        "JIRA SRE: scanned {} work items, inserted {} incidents.",
        stats.jira_scanned, stats.jira_inserted,
    );
    println!(
        "Datadog: processed {} files, inserted {} incidents.",
        stats.datadog_files, stats.datadog_inserted,
    );
    Ok(())
}

/// Project SRE-flagged JIRA work items into `fact_incidents`.
///
/// Why: `work_items` already holds raw JIRA payloads imported via the
/// ADO/JIRA tickets path. Re-projecting them into `fact_incidents`
/// gives DORA a denormalised, MTTR-ready table without requiring a
/// second ingest.
/// What: filters `work_items` to JIRA-source rows whose project is
/// `SRE` and whose `item_type` is `Bug` or `Incident`, then INSERTs a
/// `fact_incidents` row per match. `detected_at` is the work item's
/// created date and `resolved_at` is the resolution date (when both
/// are available, `mttr_hours` is denormalised).
/// Test: the migration test covers schema; smoke-level integration is
/// future work.
fn ingest_jira_sre(db: &mut Database) -> anyhow::Result<(usize, usize)> {
    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    let mut scanned = 0usize;
    let mut inserted = 0usize;
    {
        // Pre-filter to JIRA-source SRE issues.
        // `work_items` stores `raw_json` so we extract detected/resolved
        // dates from the JSON envelope.
        //
        // `work_items` is created by migration 0005 with columns:
        //   id, source, title, status, item_type, tags, project, url, raw_json
        let mut q = tx.prepare(
            "SELECT id, status, raw_json FROM work_items \
             WHERE source = 'jira' \
               AND project = 'SRE' \
               AND (item_type = 'Bug' OR item_type = 'Incident')",
        )?;
        let mut rows = q.query([])?;
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO fact_incidents \
             (incident_id, source, detected_at, resolved_at, mttr_hours, severity, \
              triggering_deploy, repo, jira_ticket) \
             VALUES (?1, 'jira_sre', ?2, ?3, ?4, ?5, NULL, NULL, ?1)",
        )?;
        while let Some(r) = rows.next()? {
            scanned += 1;
            let id: String = r.get(0)?;
            let _status: Option<String> = r.get(1)?;
            let raw_json: Option<String> = r.get(2)?;

            // Best-effort field extraction from the raw JSON payload.
            // Missing fields surface as NULLs in fact_incidents — better
            // to record an under-specified row than to silently drop.
            let (detected, resolved, severity) = extract_jira_fields(raw_json.as_deref());
            let mttr_hours = match (&detected, &resolved) {
                (Some(d), Some(r)) => {
                    Some((r.signed_duration_since(*d).num_seconds() as f64) / 3600.0)
                }
                _ => None,
            };
            insert.execute(params![
                id,
                detected.map(|d| d.to_rfc3339()),
                resolved.map(|r| r.to_rfc3339()),
                mttr_hours,
                severity,
            ])?;
            inserted += 1;
        }
    }
    tx.commit()?;
    info!(
        scanned,
        inserted, "JIRA SRE incident ingestion complete (mttr quick-win path)"
    );
    Ok((scanned, inserted))
}

/// Datadog incident ingestion (issue #213 — schema-only path).
///
/// Why: the live Datadog dump format varies enough across export tools
/// that hard-coding a parser here would lock operators into one shape.
/// Instead, ship the table + the configuration hook (`dora.datadog_dir`)
/// and document the expected envelope so adopters can wire up their own
/// path in a follow-up.
/// What: when `dora.datadog_dir` is set and exists, lists the JSON
/// files inside and (currently) logs each path; no rows are inserted
/// because there is no agreed parse contract yet. When the directory
/// is unset or missing, this is a no-op.
/// Test: smoke check — call with no config and assert zero inserts.
fn ingest_datadog(db: &mut Database, config: &Config) -> anyhow::Result<(usize, usize)> {
    let _ = db; // reserved for the future parser implementation
    let Some(dir) = config.dora.as_ref().and_then(|d| d.datadog_dir.as_ref()) else {
        return Ok((0, 0));
    };
    if !dir.exists() {
        warn!(
            path = %dir.display(),
            "dora.datadog_dir does not exist; skipping Datadog ingest"
        );
        return Ok((0, 0));
    }

    let mut files = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        files += 1;
        info!(
            path = %path.display(),
            "Datadog incident file detected (parser is a stub; rows not inserted)"
        );
    }
    // TODO(issue-213-followup): parse Datadog JSON. The envelope we
    // expect (per Datadog's incidents API):
    //   { "data": { "id": "<incident-id>",
    //               "attributes": { "created":  "<iso8601>",
    //                               "resolved": "<iso8601>",
    //                               "severity": "<sev-N>" } } }
    // Project: incident_id, source='datadog', detected_at=created,
    // resolved_at=resolved, mttr_hours = (resolved - created) / 3600.
    Ok((files, 0))
}

/// Parse a JIRA-flavoured ISO8601 timestamp.
///
/// Why: JIRA Cloud emits offsets without a colon (e.g.
/// `2025-01-01T00:00:00.000+0000`); strict RFC3339 parsers reject
/// these. We try chrono's `%+` first (handles strict RFC3339), then
/// fall back to JIRA's `%Y-%m-%dT%H:%M:%S%.3f%z` shape.
/// What: returns `Some(DateTime<FixedOffset>)` on success, `None` on
/// any parse failure.
/// Test: covered by `extract_jira_fields_handles_full_payload`.
fn parse_jira_datetime(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(d);
    }
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z").ok()
}

/// Pull `detected_at`, `resolved_at`, and `severity` out of a JIRA work
/// item's raw_json envelope.
///
/// Why: the WorkItem `raw_json` payload is JIRA's REST shape — `fields`
/// has `created`, `resolutiondate`, and `priority.name`. Centralising
/// the extraction here keeps the ingest loop readable and lets future
/// JIRA Cloud / Server quirks live in one place.
/// What: best-effort serde parse; missing fields return `None` rather
/// than aborting the row.
/// Test: covered by `extract_jira_fields_*` unit tests.
fn extract_jira_fields(
    raw_json: Option<&str>,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>) {
    let Some(text) = raw_json else {
        return (None, None, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (None, None, None);
    };
    let fields = v.get("fields");
    let parse = |k: &str| -> Option<DateTime<Utc>> {
        fields
            .and_then(|f| f.get(k))
            .and_then(|v| v.as_str())
            .and_then(parse_jira_datetime)
            .map(|d| d.with_timezone(&Utc))
    };
    let severity = fields
        .and_then(|f| f.get("priority"))
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (parse("created"), parse("resolutiondate"), severity)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: regression guard for the JIRA payload extractor — if a
    /// future serde change drops the optional fields, MTTR will silently
    /// regress to zero rows.
    /// What: feed a representative payload and assert all three fields
    /// extract.
    /// Test: pure-string parser.
    #[test]
    fn extract_jira_fields_handles_full_payload() {
        let json = r#"{
            "fields": {
                "created":        "2025-01-01T00:00:00.000+0000",
                "resolutiondate": "2025-01-01T02:00:00.000+0000",
                "priority": { "name": "High" }
            }
        }"#;
        let (d, r, sev) = extract_jira_fields(Some(json));
        assert!(d.is_some());
        assert!(r.is_some());
        assert_eq!(sev.as_deref(), Some("High"));
        // MTTR is 2.0 hours.
        let mttr = (r.unwrap().signed_duration_since(d.unwrap()).num_seconds() as f64) / 3600.0;
        assert!((mttr - 2.0).abs() < 1e-6);
    }

    /// Missing fields must degrade gracefully to `None`.
    #[test]
    fn extract_jira_fields_handles_empty_payload() {
        let (d, r, sev) = extract_jira_fields(None);
        assert!(d.is_none() && r.is_none() && sev.is_none());

        let (d, r, sev) = extract_jira_fields(Some("{}"));
        assert!(d.is_none() && r.is_none() && sev.is_none());
    }

    /// Why: when no JIRA SRE rows exist, the ingestor must succeed with
    /// zero inserts rather than erroring.
    /// What: open an empty DB (migrations apply) and call `ingest_jira_sre`.
    /// Test: smoke-level integration.
    #[test]
    fn ingest_jira_sre_with_empty_db_inserts_nothing() {
        let mut db = Database::open_in_memory().expect("db");
        let (scanned, inserted) = ingest_jira_sre(&mut db).expect("ingest");
        assert_eq!(scanned, 0);
        assert_eq!(inserted, 0);
    }
}

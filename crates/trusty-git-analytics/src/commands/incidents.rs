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
//! 2. **Datadog directory**: when `dora.datadog_dir` is set, walk every
//!    `.json` file in the directory and project Datadog's incident
//!    schema into `fact_incidents`. Three envelope shapes are recognised
//!    (single-incident, list, monitor-trigger); unrecognised files are
//!    logged at WARN level and skipped.

use chrono::{DateTime, Utc};
use clap::Args;
use rusqlite::params;
use tracing::{info, warn};

use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga incidents collect`.
#[derive(Args, Debug)]
#[command(
    about = "Ingest production incidents into fact_incidents.",
    long_about = "Walk configured incident sources and persist incident records into\n\
`fact_incidents`. Supported sources:\n\n\
  jira     -- query work_items for SRE bugs and incidents (default; no credentials)\n\
  datadog  -- walk dora.datadog_dir for .json incident files\n\n\
Both sources can run in the same invocation when --source is omitted.\n\
Rows already present are skipped (idempotent UPSERT semantics).",
    after_help = "EXAMPLES:\n\
  # Ingest from all configured sources\n\
  tga incidents collect\n\n\
  # Ingest from Datadog JSON exports only\n\
  tga incidents collect --source datadog\n\n\
TIPS:\n\
  - Set `dora.datadog_dir` in config.yaml to point at your exported JSON files.\n\
  - After ingestion, run `tga dora` to compute MTTR and Change Failure Rate."
)]
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

/// Datadog incident ingestion (issue #213).
///
/// Why: the canonical incident-source besides JIRA SRE is a Datadog
/// dump dropped onto disk by the operator (their export tool varies but
/// the JSON envelopes converge on three shapes). Centralising the
/// parser here keeps every shape in one place and lets us add new ones
/// in a single PR.
/// What: when `dora.datadog_dir` is set and exists, walks every `.json`
/// file inside, parses it via [`parse_datadog_value`], and INSERTs OR
/// REPLACEs the resulting rows into `fact_incidents`. Unparseable files
/// are logged at WARN level and skipped — one bad file must never abort
/// the whole run.
/// Test: covered by `ingest_datadog_*` unit tests; live exports are
/// integration-tested out-of-band.
fn ingest_datadog(db: &mut Database, config: &Config) -> anyhow::Result<(usize, usize)> {
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
    let mut inserted = 0usize;

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO fact_incidents \
             (incident_id, source, detected_at, resolved_at, mttr_hours, severity, \
              triggering_deploy, repo, jira_ticket) \
             VALUES (?1, 'datadog', ?2, ?3, ?4, ?5, NULL, NULL, NULL)",
        )?;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            files += 1;
            let body = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "datadog file unreadable; skipping");
                    continue;
                }
            };
            let value = match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(v) => v,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "datadog file is not valid JSON; skipping");
                    continue;
                }
            };
            let rows = parse_datadog_value(&value);
            if rows.is_empty() {
                warn!(path = %path.display(), "datadog file did not match any known shape; skipping");
                continue;
            }
            for row in rows {
                let mttr_hours = match (&row.detected_at, &row.resolved_at) {
                    (Some(d), Some(r)) => {
                        Some((r.signed_duration_since(*d).num_seconds() as f64) / 3600.0)
                    }
                    _ => None,
                };
                insert.execute(params![
                    row.incident_id,
                    row.detected_at.map(|t| t.to_rfc3339()),
                    row.resolved_at.map(|t| t.to_rfc3339()),
                    mttr_hours,
                    row.severity,
                ])?;
                inserted += 1;
            }
        }
    }
    tx.commit()?;
    info!(files, inserted, "Datadog incident ingestion complete");
    Ok((files, inserted))
}

/// Best-effort normalisation of one parsed Datadog incident.
///
/// Why: the three known envelope shapes (single-incident `data` object,
/// list-of-`data` envelope, monitor-trigger payload) all eventually
/// reduce to the same four fields — keep a single struct so the writer
/// loop above stays uniform.
/// What: a flat row of fields ready to bind into the INSERT statement.
/// Test: indirectly covered by every `parse_datadog_*` test below.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DatadogRow {
    incident_id: String,
    detected_at: Option<DateTime<Utc>>,
    resolved_at: Option<DateTime<Utc>>,
    severity: Option<String>,
}

/// Dispatch a parsed Datadog JSON document to whichever shape it matches.
///
/// Why: Datadog exports come in at least three flavours and any given
/// `.json` file may carry one or many incidents. Centralising the
/// shape-detection lets the ingest loop treat every file as "yields N
/// rows".
/// What: tries shapes in this order:
///   1. `{ "data": [ ... ] }` — list envelope (most common API export).
///   2. `{ "data": { "id": ..., "attributes": { ... } } }` — single
///      incident envelope.
///   3. `{ "id": ..., "downtime": { ... }, "monitor": { ... } }` —
///      monitor-triggered downtime payload.
///
/// Returns `Vec::new()` when none of the shapes match.
///
/// Test: `parse_datadog_value_*` unit tests below.
fn parse_datadog_value(value: &serde_json::Value) -> Vec<DatadogRow> {
    // Shape 1: { "data": [ ... ] }
    if let Some(arr) = value.get("data").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(arr.len());
        for entry in arr {
            if let Some(row) = parse_datadog_incident_object(entry) {
                out.push(row);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Shape 2: { "data": { ... single incident ... } }
    if let Some(data) = value.get("data") {
        if data.is_object() {
            if let Some(row) = parse_datadog_incident_object(data) {
                return vec![row];
            }
        }
    }

    // Shape 3: monitor-triggered downtime payload.
    if let Some(row) = parse_datadog_monitor_payload(value) {
        return vec![row];
    }

    Vec::new()
}

/// Parse one `{ "id": ..., "attributes": { ... } }` object (the
/// canonical Datadog Incidents API shape).
///
/// Why: both the single-incident envelope and the list envelope wrap
/// the same inner object — sharing this helper keeps shape-handling
/// DRY.
/// What: extracts `id`, `attributes.created`, `attributes.resolved`,
/// `attributes.severity`. Returns `None` when there is no usable `id`.
/// Test: covered by `parse_datadog_value_handles_*` tests below.
fn parse_datadog_incident_object(obj: &serde_json::Value) -> Option<DatadogRow> {
    let raw_id = obj.get("id")?;
    let id = json_value_as_id(raw_id)?;
    let attrs = obj.get("attributes");
    let detected_at = attrs
        .and_then(|a| a.get("created"))
        .and_then(parse_unix_or_iso);
    let resolved_at = attrs
        .and_then(|a| a.get("resolved"))
        .and_then(parse_unix_or_iso);
    let severity = attrs
        .and_then(|a| a.get("severity"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(DatadogRow {
        incident_id: format!("datadog:{id}"),
        detected_at,
        resolved_at,
        severity,
    })
}

/// Parse the monitor-triggered downtime payload shape:
/// `{ "id": 123, "downtime": { "start": ..., "end": ... }, "monitor": { ... } }`.
///
/// Why: Datadog's monitor `/triggered_downtime` API returns a
/// different envelope than the incidents API — operators sometimes
/// drop both kinds of dumps into the same directory.
/// What: extracts the top-level `id`, `downtime.start` / `downtime.end`,
/// and derives severity from `monitor.priority` (1→P0, ..., 5→P4).
/// Returns `None` if `downtime` is absent.
/// Test: `parse_datadog_value_handles_monitor_shape`.
fn parse_datadog_monitor_payload(value: &serde_json::Value) -> Option<DatadogRow> {
    let id = value.get("id").and_then(json_value_as_id)?;
    let downtime = value.get("downtime")?;
    let detected_at = downtime.get("start").and_then(parse_unix_or_iso);
    let resolved_at = downtime.get("end").and_then(parse_unix_or_iso);
    let severity = value
        .get("monitor")
        .and_then(|m| m.get("priority"))
        .and_then(|p| p.as_u64())
        .map(|prio| {
            // Datadog priorities are 1..=5; map 1→P0 ... 5→P4 (one less
            // than the Datadog priority, which mirrors SEV→P numbering
            // conventions adopted by most on-call rotations).
            format!("P{}", prio.saturating_sub(1))
        });
    Some(DatadogRow {
        incident_id: format!("datadog:{id}"),
        detected_at,
        resolved_at,
        severity,
    })
}

/// Stringify a JSON value that's expected to be a Datadog incident
/// identifier. Datadog inconsistently emits string ids
/// (incidents API) and integer ids (monitor API), so we normalise both
/// into a string.
fn json_value_as_id(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        if s.is_empty() {
            return None;
        }
        return Some(s.to_string());
    }
    if let Some(n) = v.as_i64() {
        return Some(n.to_string());
    }
    v.as_u64().map(|n| n.to_string())
}

/// Parse a JSON value that may be either a Unix epoch integer (Datadog
/// monitor API) or an ISO8601 string (Datadog incidents API).
///
/// Why: a single helper means the downstream extractors don't have to
/// branch on JSON value kind per field. Returns `None` for missing /
/// malformed values rather than panicking.
/// What: tries u64/i64 epoch seconds first; then falls back to
/// RFC3339 / JIRA-style offsets via [`parse_jira_datetime`].
/// Test: covered by `parse_unix_or_iso_*` unit tests.
fn parse_unix_or_iso(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(n) = v.as_i64() {
        return chrono::DateTime::<Utc>::from_timestamp(n, 0);
    }
    if let Some(n) = v.as_u64() {
        // Cast is safe: u64 ≤ i64::MAX for any realistic epoch second.
        let n = i64::try_from(n).ok()?;
        return chrono::DateTime::<Utc>::from_timestamp(n, 0);
    }
    if let Some(s) = v.as_str() {
        // Some exports stringify the epoch — be lenient.
        if let Ok(n) = s.parse::<i64>() {
            return chrono::DateTime::<Utc>::from_timestamp(n, 0);
        }
        return parse_jira_datetime(s).map(|d| d.with_timezone(&Utc));
    }
    None
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
#[path = "incidents_tests.rs"]
mod tests;

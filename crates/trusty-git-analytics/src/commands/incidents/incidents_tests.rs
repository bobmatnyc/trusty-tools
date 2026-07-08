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

// -----------------------------------------------------------------
// Datadog ingestion — #213
// -----------------------------------------------------------------

use tga::core::config::DoraConfig;

/// Build a uniquely-named temp directory and return its path.
/// Mirrors the in-tree pattern from `core::config::aliases::tests`
/// — no `tempfile` dep available in this crate.
fn unique_tmp_dir(label: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("tga-datadog-{label}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Why: `parse_unix_or_iso` is the shared time-parsing seam for
/// every Datadog shape. An epoch integer must produce a UTC
/// timestamp matching the input second-for-second.
#[test]
fn parse_unix_or_iso_handles_epoch_integer() {
    let v = serde_json::json!(1_700_000_000_i64);
    let dt = parse_unix_or_iso(&v).expect("parses epoch");
    assert_eq!(dt.timestamp(), 1_700_000_000);
}

/// Why: ISO8601 strings must also flow through the same helper so
/// the incidents-API path produces the same `DateTime<Utc>` shape.
#[test]
fn parse_unix_or_iso_handles_iso_string() {
    let v = serde_json::json!("2025-01-01T02:00:00Z");
    let dt = parse_unix_or_iso(&v).expect("parses iso");
    assert_eq!(dt.to_rfc3339(), "2025-01-01T02:00:00+00:00");
}

/// Why: regression guard against an exporter that stringifies the
/// epoch second — `"1700000000"` must round-trip as a timestamp.
#[test]
fn parse_unix_or_iso_handles_stringified_epoch() {
    let v = serde_json::json!("1700000000");
    let dt = parse_unix_or_iso(&v).expect("parses stringified epoch");
    assert_eq!(dt.timestamp(), 1_700_000_000);
}

/// Why: null / missing values must degrade to `None` rather than
/// panic. Production payloads routinely omit `resolved` for ongoing
/// incidents.
#[test]
fn parse_unix_or_iso_returns_none_for_unparseable_inputs() {
    assert!(parse_unix_or_iso(&serde_json::json!(null)).is_none());
    assert!(parse_unix_or_iso(&serde_json::json!("not-a-date")).is_none());
    assert!(parse_unix_or_iso(&serde_json::json!(true)).is_none());
}

/// Why: when `dora.datadog_dir` points at a non-existent directory,
/// the ingestor must return `(0, 0)` and emit a warning rather than
/// surfacing a hard error — operators may have a stale config.
#[test]
fn ingest_datadog_skips_missing_dir() {
    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(std::path::PathBuf::from(
                "/definitely/does/not/exist/dd-xyz-zzz",
            )),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    assert_eq!(files, 0);
    assert_eq!(inserted, 0);
}

/// Why: when the directory is unset (no DORA config), the ingestor
/// must early-return cleanly.
#[test]
fn ingest_datadog_skips_unset_dir() {
    let mut db = Database::open_in_memory().expect("db");
    let config = Config::default();
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    assert_eq!(files, 0);
    assert_eq!(inserted, 0);
}

/// Why: the canonical incidents API single-object envelope is the
/// reference shape — it must produce exactly one row with the
/// `datadog:` id namespace, parsed ISO timestamps, and computed
/// MTTR.
#[test]
fn ingest_datadog_parses_incident_api_shape() {
    let dir = unique_tmp_dir("incident-shape");
    let file = dir.join("incident-001.json");
    std::fs::write(
        &file,
        r#"{
                "data": {
                    "id": "abc-123",
                    "attributes": {
                        "created":  "2025-01-01T00:00:00Z",
                        "resolved": "2025-01-01T02:00:00Z",
                        "severity": "SEV-1"
                    }
                }
            }"#,
    )
    .expect("write file");

    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(dir.clone()),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    assert_eq!(files, 1);
    assert_eq!(inserted, 1);

    let conn = db.connection();
    let (id, severity, mttr): (String, Option<String>, Option<f64>) = conn
        .query_row(
            "SELECT incident_id, severity, mttr_hours FROM fact_incidents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(id, "datadog:abc-123");
    assert_eq!(severity.as_deref(), Some("SEV-1"));
    assert!((mttr.expect("mttr") - 2.0).abs() < 1e-6);

    // Cleanup is best-effort.
    let _ = std::fs::remove_dir_all(&dir);
}

/// Why: the list-envelope export shape ships multiple incidents per
/// file; the parser must yield one row per array element.
#[test]
fn ingest_datadog_parses_list_shape() {
    let dir = unique_tmp_dir("list-shape");
    let file = dir.join("incidents-bulk.json");
    std::fs::write(
        &file,
        r#"{
                "data": [
                    {
                        "id": "i-001",
                        "attributes": {
                            "created":  "2025-01-01T00:00:00Z",
                            "resolved": "2025-01-01T01:00:00Z",
                            "severity": "SEV-2"
                        }
                    },
                    {
                        "id": "i-002",
                        "attributes": {
                            "created":  "2025-01-02T00:00:00Z",
                            "severity": "SEV-3"
                        }
                    }
                ]
            }"#,
    )
    .expect("write file");

    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(dir.clone()),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    assert_eq!(files, 1);
    assert_eq!(inserted, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Why: the monitor-trigger payload uses an integer id and Unix
/// epoch timestamps under `downtime`. The parser must produce a row
/// with the `datadog:<int>` id, second-precision timestamps, and a
/// severity derived from `monitor.priority`.
#[test]
fn ingest_datadog_parses_monitor_shape() {
    let dir = unique_tmp_dir("monitor-shape");
    let file = dir.join("monitor-trip.json");
    std::fs::write(
        &file,
        r#"{
                "id": 42,
                "downtime": { "start": 1700000000, "end": 1700003600 },
                "monitor": { "name": "API error rate", "priority": 3 }
            }"#,
    )
    .expect("write file");

    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(dir.clone()),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    assert_eq!(files, 1);
    assert_eq!(inserted, 1);

    let conn = db.connection();
    let (id, severity, mttr): (String, Option<String>, Option<f64>) = conn
        .query_row(
            "SELECT incident_id, severity, mttr_hours FROM fact_incidents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(id, "datadog:42");
    // Datadog priority 3 → P2.
    assert_eq!(severity.as_deref(), Some("P2"));
    // 3600s / 3600 = 1.0 hour MTTR.
    assert!((mttr.expect("mttr") - 1.0).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Why: a single unparseable file must not abort the whole run —
/// the loop must skip it (warn) and continue with the next file.
#[test]
fn ingest_datadog_skips_unparseable_files() {
    let dir = unique_tmp_dir("bad-files");
    std::fs::write(dir.join("garbage.json"), "this is not json at all").expect("write garbage");
    std::fs::write(
        dir.join("good.json"),
        r#"{
                "data": {
                    "id": "ok-1",
                    "attributes": {
                        "created":  "2025-03-01T00:00:00Z",
                        "resolved": "2025-03-01T00:30:00Z",
                        "severity": "SEV-2"
                    }
                }
            }"#,
    )
    .expect("write good");

    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(dir.clone()),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let (files, inserted) = ingest_datadog(&mut db, &config).expect("ingest");
    // Both files counted as encountered; only the good one inserts.
    assert_eq!(files, 2);
    assert_eq!(inserted, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Why: `INSERT OR REPLACE` is the idempotency contract — re-ingest
/// of the same payload must update the existing row in place rather
/// than producing a duplicate.
#[test]
fn ingest_datadog_replaces_on_reingest() {
    let dir = unique_tmp_dir("idempotent");
    let path = dir.join("incident.json");
    let payload = r#"{
            "data": {
                "id": "dup-1",
                "attributes": {
                    "created":  "2025-01-01T00:00:00Z",
                    "resolved": "2025-01-01T01:00:00Z",
                    "severity": "SEV-3"
                }
            }
        }"#;
    std::fs::write(&path, payload).expect("write");

    let mut db = Database::open_in_memory().expect("db");
    let config = Config {
        dora: Some(DoraConfig {
            datadog_dir: Some(dir.clone()),
            ..DoraConfig::default()
        }),
        ..Config::default()
    };
    let _ = ingest_datadog(&mut db, &config).expect("ingest 1");
    let _ = ingest_datadog(&mut db, &config).expect("ingest 2");
    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_incidents", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1, "INSERT OR REPLACE must dedupe on incident_id PK");
    let _ = std::fs::remove_dir_all(&dir);
}

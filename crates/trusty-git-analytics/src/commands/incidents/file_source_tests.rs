use super::*;
use tga::core::db::Database;

/// Why: the primary happy-path contract for issue #2204 — a generic
/// incident JSON export (matching the shape the issue itself proposes)
/// must ingest cleanly into `fact_incidents` with `mttr_hours` derived.
/// What: writes a two-record `{"incidents": [...]}` JSON file (one fully
/// resolved, one still open) and asserts both rows land with the right
/// `source` and `mttr_hours` handling.
/// Test: this test itself.
#[test]
fn ingest_file_json_happy_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.json");
    std::fs::write(
        &path,
        r#"{
            "incidents": [
                {
                    "incident_id": "INC-1",
                    "detected_at": "2026-01-01T00:00:00Z",
                    "resolved_at": "2026-01-01T02:00:00Z",
                    "severity": "P1",
                    "repo": "myrepo"
                },
                {
                    "incident_id": "INC-2",
                    "detected_at": "2026-01-02T00:00:00Z"
                }
            ]
        }"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let (scanned, inserted) = ingest_file(&mut db, &path).expect("ingest");
    assert_eq!(scanned, 2);
    assert_eq!(inserted, 2);

    let conn = db.connection();
    let (source, mttr): (String, Option<f64>) = conn
        .query_row(
            "SELECT source, mttr_hours FROM fact_incidents WHERE incident_id = 'INC-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 1");
    assert_eq!(source, "file");
    assert!((mttr.expect("mttr") - 2.0).abs() < 1e-9);

    let (source2, mttr2): (String, Option<f64>) = conn
        .query_row(
            "SELECT source, mttr_hours FROM fact_incidents WHERE incident_id = 'INC-2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 2");
    assert_eq!(source2, "file");
    assert!(mttr2.is_none(), "unresolved incident must have no mttr");
}

/// Why: `INSERT OR REPLACE` is the idempotency contract the JIRA/Datadog
/// paths already use for `fact_incidents`; the file source must match so
/// a re-run updates rather than duplicates.
/// What: ingests the same JSON file twice and asserts row count stays 1.
#[test]
fn ingest_file_json_reingest_replaces_not_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.json");
    std::fs::write(
        &path,
        r#"{"incidents": [{"incident_id": "INC-1", "detected_at": "2026-01-01T00:00:00Z"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    ingest_file(&mut db, &path).expect("first ingest");
    ingest_file(&mut db, &path).expect("second ingest");

    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_incidents", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1, "re-ingest must replace, not duplicate");
}

/// Why: CSV is the other documented shape (issue #2204); it must ingest
/// identically to JSON including blank-cell -> None defaulting for the
/// optional trailing columns.
#[test]
fn ingest_file_csv_happy_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.csv");
    std::fs::write(
        &path,
        "incident_id,detected_at,resolved_at,severity,repo,triggering_deploy,jira_ticket\n\
         INC-1,2026-01-01T00:00:00Z,2026-01-01T01:00:00Z,P2,myrepo,myrepo@v1,SRE-42\n\
         INC-2,2026-01-02T00:00:00Z,,,,, \n",
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    // `triggering_deploy` is a real FK to fact_deployments.deploy_id — seed
    // a matching row so the happy-path CSV (which sets it) doesn't trip the
    // FK constraint.
    db.connection()
        .execute(
            "INSERT INTO fact_deployments (deploy_id, repo, triggered_at) \
             VALUES ('myrepo@v1', 'myrepo', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("seed fact_deployments");

    let (scanned, inserted) = ingest_file(&mut db, &path).expect("ingest");
    assert_eq!(scanned, 2);
    assert_eq!(inserted, 2);

    let conn = db.connection();
    let (severity, jira_ticket): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT severity, jira_ticket FROM fact_incidents WHERE incident_id = 'INC-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 1");
    assert_eq!(severity.as_deref(), Some("P2"));
    assert_eq!(jira_ticket.as_deref(), Some("SRE-42"));

    let (severity2, repo2): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT severity, repo FROM fact_incidents WHERE incident_id = 'INC-2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 2");
    assert!(severity2.is_none(), "blank cell must default to None");
    assert!(repo2.is_none(), "blank cell must default to None");
}

/// Why: malformed input must fail loudly with a useful message (issue
/// #2204's "clear errors" requirement) rather than silently dropping or
/// panicking.
#[test]
fn ingest_file_json_rejects_bad_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.json");
    std::fs::write(
        &path,
        r#"{"incidents": [{"incident_id": "INC-1", "detected_at": "not-a-date"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("bad timestamp must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("detected_at"),
        "error should name field: {msg}"
    );
    assert!(msg.contains("INC-1"), "error should name the record: {msg}");
}

/// Why: an empty required field (`incident_id`) must be rejected.
#[test]
fn ingest_file_json_rejects_empty_required_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.json");
    std::fs::write(
        &path,
        r#"{"incidents": [{"incident_id": "", "detected_at": "2026-01-01T00:00:00Z"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("empty incident_id must error");
    assert!(format!("{err}").contains("incident_id"));
}

/// Why: a CSV missing a required column header must fail before any row
/// is processed, with a message naming the missing column.
#[test]
fn ingest_file_csv_rejects_missing_required_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.csv");
    std::fs::write(&path, "severity,repo\nP1,myrepo\n").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("missing incident_id column must error");
    assert!(format!("{err}").contains("incident_id"));
}

/// Why: malformed JSON (not even valid JSON syntax) must produce a clear
/// parse error rather than a confusing panic.
#[test]
fn ingest_file_json_rejects_invalid_json_syntax() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.json");
    std::fs::write(&path, "{ not valid json").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("invalid JSON must error");
    assert!(format!("{err}").contains("incidents"));
}

/// Why: an unsupported extension is a common operator mistake; the error
/// must say so rather than silently no-op'ing.
#[test]
fn ingest_file_rejects_unsupported_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("incidents.txt");
    std::fs::write(&path, "irrelevant").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("unsupported extension must error");
    assert!(format!("{err}").contains(".json or .csv"));
}

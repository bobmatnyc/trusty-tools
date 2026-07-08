use super::*;
use tga::core::db::Database;

/// Why: the primary happy-path contract for issue #2204 — cto-reports'
/// JSON export must ingest cleanly into `fact_deployments`.
/// What: writes a two-record `{"deployments": [...]}` JSON file and
/// asserts both rows land with the documented defaults applied
/// (`environment` -> "production", `source` kept as given).
/// Test: this test itself.
#[test]
fn ingest_file_json_happy_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.json");
    std::fs::write(
        &path,
        r#"{
            "deployments": [
                {
                    "deploy_id": "myrepo@v1.0.0",
                    "repo": "myrepo",
                    "triggered_at": "2026-01-01T00:00:00Z",
                    "completed_at": "2026-01-01T00:05:00Z",
                    "status": "success",
                    "git_sha": "abc123",
                    "git_tag": "v1.0.0",
                    "triggered_by_pr": 42,
                    "source": "ci_pipeline"
                },
                {
                    "deploy_id": "myrepo@v1.0.1",
                    "repo": "myrepo",
                    "triggered_at": "2026-01-02T00:00:00Z",
                    "status": "failed"
                }
            ]
        }"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let stats = ingest_file(&mut db, &path).expect("ingest");
    assert_eq!(stats.inserted, 2);
    assert_eq!(stats.skipped, 0);

    let conn = db.connection();
    let (environment, source): (String, String) = conn
        .query_row(
            "SELECT environment, source FROM fact_deployments WHERE deploy_id = 'myrepo@v1.0.0'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 1");
    assert_eq!(environment, "production");
    assert_eq!(source, "ci_pipeline");

    // Second record used defaults for environment/source and had no
    // optional fields at all.
    let (environment2, source2): (String, String) = conn
        .query_row(
            "SELECT environment, source FROM fact_deployments WHERE deploy_id = 'myrepo@v1.0.1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 2");
    assert_eq!(environment2, "production");
    assert_eq!(source2, "file");
}

/// Why: idempotency (`deploy_id` PRIMARY KEY + INSERT OR IGNORE) must hold
/// for the file source exactly as it does for `git_tags` — cto-reports
/// will re-run ingestion on every ETL cycle.
/// What: ingests the same JSON file twice and asserts the second run
/// reports 0 inserted / 1 skipped, with no duplicate row.
/// Test: this test itself.
#[test]
fn ingest_file_json_is_idempotent_on_reingest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.json");
    std::fs::write(
        &path,
        r#"{"deployments": [{"deploy_id": "r@v1", "repo": "r", "triggered_at": "2026-01-01T00:00:00Z", "status": "success"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let first = ingest_file(&mut db, &path).expect("first ingest");
    assert_eq!(first.inserted, 1);
    let second = ingest_file(&mut db, &path).expect("second ingest");
    assert_eq!(second.inserted, 0);
    assert_eq!(second.skipped, 1);

    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fact_deployments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1, "re-ingest must not duplicate rows");
}

/// Why: CSV is the other documented shape (issue #2204); it must ingest
/// identically to JSON including blank-cell -> None defaulting.
/// What: writes a CSV with all 10 documented columns, one row with every
/// optional field populated and one with them blank, and asserts both
/// insert with the correct default handling.
/// Test: this test itself.
#[test]
fn ingest_file_csv_happy_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.csv");
    std::fs::write(
        &path,
        "deploy_id,repo,environment,triggered_at,completed_at,status,git_sha,git_tag,triggered_by_pr,source\n\
         r@v1,r,staging,2026-01-01T00:00:00Z,2026-01-01T00:05:00Z,success,sha1,v1,7,ci_pipeline\n\
         r@v2,r,,2026-01-02T00:00:00Z,,failed,,,,\n",
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let stats = ingest_file(&mut db, &path).expect("ingest");
    assert_eq!(stats.inserted, 2);

    let conn = db.connection();
    let (environment, triggered_by_pr): (String, Option<i64>) = conn
        .query_row(
            "SELECT environment, triggered_by_pr FROM fact_deployments WHERE deploy_id = 'r@v1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 1");
    assert_eq!(environment, "staging");
    assert_eq!(triggered_by_pr, Some(7));

    let (environment2, source2): (String, String) = conn
        .query_row(
            "SELECT environment, source FROM fact_deployments WHERE deploy_id = 'r@v2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row 2");
    assert_eq!(environment2, "production", "blank cell must default");
    assert_eq!(source2, "file", "blank cell must default");
}

/// Why: malformed input must fail loudly with a useful message, not
/// silently drop rows or panic (issue #2204's "clear errors" requirement).
/// What: an invalid RFC3339 `triggered_at` value must produce an error
/// naming the record and the offending field.
/// Test: this test itself.
#[test]
fn ingest_file_json_rejects_bad_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.json");
    std::fs::write(
        &path,
        r#"{"deployments": [{"deploy_id": "r@v1", "repo": "r", "triggered_at": "not-a-date", "status": "success"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("bad timestamp must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("triggered_at"),
        "error should name field: {msg}"
    );
    assert!(msg.contains("r@v1"), "error should name the record: {msg}");
}

/// Why: an empty required field (`repo`) must be rejected the same way a
/// missing one is — blank strings are a common malformed-CSV artifact.
#[test]
fn ingest_file_json_rejects_empty_required_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.json");
    std::fs::write(
        &path,
        r#"{"deployments": [{"deploy_id": "r@v1", "repo": "", "triggered_at": "2026-01-01T00:00:00Z", "status": "success"}]}"#,
    )
    .expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("empty repo must error");
    assert!(format!("{err}").contains("repo"));
}

/// Why: a CSV missing a required column header must fail before any row
/// is processed, with a message naming the missing column.
#[test]
fn ingest_file_csv_rejects_missing_required_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.csv");
    std::fs::write(&path, "repo,status\nr,success\n").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("missing deploy_id column must error");
    assert!(format!("{err}").contains("deploy_id"));
}

/// Why: malformed JSON (not even valid JSON syntax) must produce a clear
/// parse error rather than a confusing panic.
#[test]
fn ingest_file_json_rejects_invalid_json_syntax() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.json");
    std::fs::write(&path, "{ not valid json").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("invalid JSON must error");
    assert!(format!("{err}").contains("deployments"));
}

/// Why: an unsupported extension is a common operator mistake (e.g. a
/// `.txt` export); the error must say so rather than silently no-op'ing.
#[test]
fn ingest_file_rejects_unsupported_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deployments.txt");
    std::fs::write(&path, "irrelevant").expect("write fixture");

    let mut db = Database::open_in_memory().expect("db");
    let err = ingest_file(&mut db, &path).expect_err("unsupported extension must error");
    assert!(format!("{err}").contains(".json or .csv"));
}

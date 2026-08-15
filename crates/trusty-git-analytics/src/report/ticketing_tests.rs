//! Tests for the ticketing artifact (#5405).

use rusqlite::params;

use super::{build_ticketing_summary, TICKETING_SCHEMA_VERSION};
use crate::collect::correlate::correlate_commits;
use crate::core::db::work_items::{upsert_work_item, WorkItemRow};
use crate::core::db::Database;
use crate::core::progress::ProgressBus;

fn work_item(id: &str, source: &str) -> WorkItemRow {
    WorkItemRow {
        id: id.into(),
        source: source.into(),
        title: format!("Item {id}"),
        status: "Open".into(),
        item_type: "Task".into(),
        tags: None,
        project: None,
        url: None,
        raw_json: None,
    }
}

fn insert_commit(db: &Database, sha: &str, message: &str, ticket: Option<&str>) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                  repository, ticket_id) \
             VALUES (?1, 'A', 'a@x', '2026-01-01T00:00:00Z', ?2, 'repo', ?3)",
            params![sha, message, ticket],
        )
        .expect("insert commit");
}

/// The figures the report states must be the database's own, not a recount.
#[test]
fn summary_counts_match_the_database() {
    let mut db = Database::open_in_memory().expect("open");
    insert_commit(&db, "aaa", "PROJ-1 ship", Some("PROJ-1"));
    insert_commit(&db, "bbb", "ENG-7 also ship", Some("ENG-7"));
    insert_commit(&db, "ccc", "chore: no ticket", None);
    upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("jira");
    upsert_work_item(db.connection(), &work_item("ENG-7", "linear")).expect("linear");
    // Synced but never cited by a commit — counted in `work_items`, absent from
    // `sources`, which is the distinction the source list exists to draw.
    upsert_work_item(db.connection(), &work_item("PROJ-99", "jira")).expect("orphan");

    correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("correlate");

    let summary = build_ticketing_summary(db.connection()).expect("summary");
    assert_eq!(summary.schema_version, TICKETING_SCHEMA_VERSION);
    assert_eq!(summary.commits, 3);
    assert_eq!(summary.commits_linked, 2);
    assert_eq!(summary.work_items, 3);
    assert_eq!(summary.work_items_linked, 2);
    assert_eq!(summary.sources, vec!["jira".to_string(), "linear".into()]);
    assert!(!summary.is_empty());
}

/// #5405's anti-fail-open half at the data layer: a run that correlated nothing
/// still produces a summary. The report states the zero; it never mistakes an
/// empty board for an absent artifact.
#[test]
fn an_empty_database_still_produces_a_summary() {
    let db = Database::open_in_memory().expect("open");

    let summary = build_ticketing_summary(db.connection()).expect("summary");
    assert_eq!(summary.commits, 0);
    assert_eq!(summary.commits_linked, 0);
    assert_eq!(summary.work_items, 0);
    assert!(summary.sources.is_empty());
    assert!(
        summary.is_empty(),
        "an uncorrelated run must be distinguishable from a correlated one"
    );
}

/// The cross-process contract: the JSON tga writes must carry exactly the keys
/// trusty-review's loader reads. Its mirror lives in
/// `trusty-review/src/report/ticketing_tests.rs`.
#[test]
fn round_trips_through_the_review_schema() {
    let mut db = Database::open_in_memory().expect("open");
    insert_commit(&db, "aaa", "PROJ-1 ship", Some("PROJ-1"));
    upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("jira");
    correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("correlate");

    let json = build_ticketing_summary(db.connection())
        .expect("summary")
        .to_json()
        .expect("serialize");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    for key in [
        "schema_version",
        "commits",
        "commits_linked",
        "work_items",
        "work_items_linked",
        "sources",
    ] {
        assert!(parsed.get(key).is_some(), "missing key {key} in {json}");
    }
    assert_eq!(parsed["commits_linked"], 1);
    assert_eq!(parsed["sources"][0], "jira");
}

/// Two builds over an unchanged database produce identical bytes — the same
/// determinism property `dd_manifest` holds, and what makes a re-render of the
/// same audit reproducible.
#[test]
fn two_builds_are_byte_identical() {
    let mut db = Database::open_in_memory().expect("open");
    insert_commit(&db, "aaa", "PROJ-1 ship", Some("PROJ-1"));
    upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("jira");
    correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("correlate");

    let first = build_ticketing_summary(db.connection()).expect("first");
    let second = build_ticketing_summary(db.connection()).expect("second");
    assert_eq!(first, second);
    assert_eq!(first.to_json().expect("a"), second.to_json().expect("b"));
}

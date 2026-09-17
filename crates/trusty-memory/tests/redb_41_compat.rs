//! redb 4.1 -> 4.3 on-disk compatibility for the activity log (#8254).
//!
//! Why: the workspace `redb` dependency moved from 4.1.0 to 4.3.0, and 4.3.0
//! changes how variable-length keys store separators and how a database is
//! recovered. A database written by 4.1 must still open under 4.3 with every
//! record intact. Creating a database under 4.3 and reading it back would pass
//! even if that were false, so the fixture here is committed BYTES written by
//! redb 4.1.0 — see `tests/testdata/activity-redb41.redb`.
//!
//! The production opener (`ActivityLog::open` ->
//! `trusty_common::memory_core::store::open_or_recreate`) RECREATES a database
//! it classifies as an incompatible format. That is exactly the silent-data-loss
//! path this test guards: a 4.3 build that could not read 4.1 bytes would wipe
//! the file and report success, so asserting the rows are still there is the
//! assertion that matters.
//!
//! The fixture's extension is `.redb-fixture`, not `.redb`: the repository
//! `.gitignore` excludes `*.redb`, so a fixture named that way would never be
//! committed. `stage_41_fixture` copies it under the name the opener expects.

use trusty_memory::activity::{ActivityFilter, ActivityLog, ActivitySource};

/// Copy the committed 4.1-era fixture into a scratch dir under the filename
/// `ActivityLog::open` expects, and return the dir (kept alive by the caller).
fn stage_41_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("testdata")
        .join("activity-redb41.redb-fixture");
    assert!(
        fixture.is_file(),
        "the redb 4.1 fixture must be committed at {}",
        fixture.display()
    );
    std::fs::copy(&fixture, tmp.path().join("activity.redb")).expect("stage fixture");
    tmp
}

/// Why: proves the 4.1 -> 4.3 bump preserves existing user data rather than
/// silently recreating the store. This is the core compatibility guarantee of
/// #8254.
/// What: stages a database written by redb 4.1.0, opens it with the workspace's
/// 4.3.0 build through the production opener, and asserts every one of the five
/// fixture rows survives with its id, source, palace, event type and payload.
/// Test: this test.
#[test]
fn redb_41_activity_database_reopens_under_43_with_records_intact() {
    let tmp = stage_41_fixture();
    let log = ActivityLog::open(tmp.path()).expect("a 4.1-written database must open under 4.3");

    assert_eq!(
        log.count().expect("count"),
        5,
        "all five rows written by redb 4.1 must survive the 4.3 open"
    );

    let rows = log
        .list(&ActivityFilter::default(), 100, 0)
        .expect("list the 4.1-written rows");
    assert_eq!(rows.len(), 5, "list must return every preserved row");

    // `list` is newest-first, so ids descend 5..=1.
    let ids: Vec<u64> = rows.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![5, 4, 3, 2, 1], "ids and ordering preserved");

    for entry in &rows {
        let n = entry.id;
        assert_eq!(
            entry.event_type,
            format!("fixture_event_{n}"),
            "event_type preserved for row {n}"
        );
        assert_eq!(
            entry.payload,
            format!("{{\"n\":{n}}}"),
            "payload bytes preserved for row {n}"
        );
        assert_eq!(
            entry.palace_id.as_deref(),
            Some("fixture-palace"),
            "palace_id preserved for row {n}"
        );
        let want = match n % 3 {
            0 => ActivitySource::Hook,
            1 => ActivitySource::Http,
            _ => ActivitySource::Mcp,
        };
        assert_eq!(entry.source, want, "source preserved for row {n}");
    }
}

/// Why: a preserved database must also be WRITABLE under 4.3 — reading old
/// bytes is not enough if the next append corrupts or is rejected. 4.3.0's
/// `Key::separator()` change alters how new keys are stored in a tree whose
/// existing pages were written by 4.1.
/// What: opens the 4.1 fixture, appends a fresh row, and asserts the new row and
/// all five old rows read back together, with the id continuing from the
/// fixture's maximum.
/// Test: this test.
#[test]
fn redb_41_database_accepts_new_writes_under_43() {
    let tmp = stage_41_fixture();
    let log = ActivityLog::open(tmp.path()).expect("open 4.1 fixture");

    let id = log
        .append(
            ActivitySource::Mcp,
            Some("fixture-palace".to_string()),
            "appended_under_43",
            serde_json::json!({"n": 6}),
        )
        .expect("append into a 4.1-written database");
    assert_eq!(id, 6, "next_id must resume from the fixture's max key");

    assert_eq!(log.count().expect("count"), 6, "old rows plus the new one");
    let rows = log
        .list(&ActivityFilter::default(), 100, 0)
        .expect("list after append");
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].event_type, "appended_under_43");
    assert_eq!(
        rows[5].event_type, "fixture_event_1",
        "the oldest 4.1 row is still readable after a 4.3 write"
    );
}

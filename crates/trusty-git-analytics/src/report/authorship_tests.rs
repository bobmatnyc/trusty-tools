//! Tests for the authorship artifact (#5453, #6004).

use rusqlite::params;

use super::{build_authorship_summary, AUTHORSHIP_SCHEMA_VERSION};
use crate::core::db::Database;

/// Insert one commit (with its file touches) into the seeded database.
#[allow(clippy::too_many_arguments)]
fn insert_commit(
    db: &Database,
    sha: &str,
    author_name: &str,
    author_email: &str,
    timestamp: &str,
    repository: &str,
    is_merge: bool,
    paths: &[&str],
) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                  repository, is_merge) \
             VALUES (?1, ?2, ?3, ?4, 'msg', ?5, ?6)",
            params![
                sha,
                author_name,
                author_email,
                timestamp,
                repository,
                is_merge as i64
            ],
        )
        .expect("insert commit");
    let commit_id = db.connection().last_insert_rowid();
    for path in paths {
        db.connection()
            .execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'modified')",
                params![commit_id, path],
            )
            .expect("insert file");
    }
}

/// A single-author repository has bus factor 1 and 100% concentration.
#[test]
fn builds_from_seeded_commits() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "a2",
        "Alice",
        "alice@x.com",
        "2026-02-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.schema_version, AUTHORSHIP_SCHEMA_VERSION);
    assert_eq!(summary.repository, "repo");
    assert_eq!(summary.distinct_authors, 1);
    assert_eq!(summary.bus_factor, 1);
    assert!((summary.top_author_share_pct - 100.0).abs() < f64::EPSILON);
    assert_eq!(summary.single_author_subsystems, vec!["src".to_string()]);
    assert_eq!(summary.monthly_trajectory.len(), 2);
    assert!(!summary.caveats.is_empty());
}

/// (#5453) Bot commits and merge commits are excluded from every figure.
#[test]
fn bots_and_merges_are_excluded() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "h1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "b1",
        "dependabot[bot]",
        "dependabot[bot]@users.noreply.github.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["deps/lock.json"],
    );
    insert_commit(
        &db,
        "m1",
        "Bob",
        "bob@x.com",
        "2026-01-17T00:00:00Z",
        "repo",
        true,
        &["src/merged.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 1, "bot and merge author excluded");
    assert_eq!(summary.single_author_subsystems, vec!["src".to_string()]);
}

/// A subsystem touched by two distinct authors is never listed as
/// single-author.
#[test]
fn shared_subsystem_is_not_single_author() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "b1",
        "Bob",
        "bob@x.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/other.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 2);
    assert!(summary.single_author_subsystems.is_empty());
    assert_eq!(summary.bus_factor, 1); // 50%-of-touches threshold, 1 of 2 needed
}

/// A repository with no commits at all still produces a (zeroed) summary
/// rather than an error — the caller decides how to render zero data.
#[test]
fn empty_repository_yields_zeroed_summary() {
    let db = Database::open_in_memory().expect("open");
    let summary = build_authorship_summary(db.connection(), "nonexistent").expect("summary");
    assert_eq!(summary.distinct_authors, 0);
    assert_eq!(summary.bus_factor, 0);
    assert!(summary.monthly_trajectory.is_empty());
}

/// The monthly trajectory keeps at most the most recent 12 active months,
/// even when the database spans more than a year of history.
#[test]
fn trajectory_caps_at_twelve_months() {
    let db = Database::open_in_memory().expect("open");
    // 14 distinct months: 2024-11 .. 2025-12.
    let months = [
        "2024-11", "2024-12", "2025-01", "2025-02", "2025-03", "2025-04", "2025-05", "2025-06",
        "2025-07", "2025-08", "2025-09", "2025-10", "2025-11", "2025-12",
    ];
    for (i, month) in months.iter().enumerate() {
        insert_commit(
            &db,
            &format!("c{i}"),
            "Alice",
            "alice@x.com",
            &format!("{month}-01T00:00:00Z"),
            "repo",
            false,
            &["src/lib.rs"],
        );
    }
    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.monthly_trajectory.len(), 12);
    // Oldest-first, and only the most recent 12 survive — 2024-11/12 dropped.
    assert_eq!(summary.monthly_trajectory.first().unwrap().month, "2025-01");
    assert_eq!(summary.monthly_trajectory.last().unwrap().month, "2025-12");
}

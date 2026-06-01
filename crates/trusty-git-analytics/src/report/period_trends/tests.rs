//! Unit tests for N-week period trend roll-ups.
//!
//! Why: co-located with the submodule so tests are easy to find and run
//! independently with `cargo test -p tga period_trends`.
//! What: covers basic windowing, date filters, category aggregation, label
//! formatting, ticketed-pct, empty-author edge cases, and the week-helper.
//! Test: this file is the test itself — run with `cargo test -p tga`.

use crate::core::db::Database;
use crate::report::period_trends::query::query_author_period_trends;
use chrono::{Datelike, NaiveDate};
use rusqlite::params;

use super::query::weeks_in_range;

fn seed_author(db: &Database, name: &str, email: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO authors (canonical_name, canonical_email, aliases) \
             VALUES (?1, ?2, '[]')",
            params![name, email],
        )
        .expect("insert author");
    db.connection().last_insert_rowid()
}

fn seed_commit(
    db: &Database,
    sha: &str,
    author_id: i64,
    timestamp: &str,
    ticketed: i64,
    category: Option<&str>,
) {
    // Insert classification if needed.
    let cls_id: Option<i64> = if let Some(cat) = category {
        db.connection()
            .execute(
                "INSERT OR IGNORE INTO classifications \
                 (id, category, confidence, method) \
                 VALUES (NULL, ?1, 0.9, 'exact_rule')",
                params![cat],
            )
            .expect("insert classification");
        let id: i64 = db
            .connection()
            .query_row(
                "SELECT id FROM classifications WHERE category = ?1 LIMIT 1",
                params![cat],
                |r| r.get(0),
            )
            .expect("get cls id");
        Some(id)
    } else {
        None
    };

    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_id, author_name, author_email, timestamp, message, \
              repository, insertions, deletions, ticketed, classification_id) \
             VALUES (?1, ?2, 'n', 'e', ?3, 'm', 'repo-a', 5, 2, ?4, ?5)",
            params![sha, author_id, timestamp, ticketed, cls_id],
        )
        .expect("insert commit");
}

/// Why: baseline test — two 4-week windows should be produced for 8 weeks
/// of commits, with correct commit counts per window.
/// What: seeds 8 weeks of commits, calls `query_author_period_trends` with
/// `window_weeks = 4`, asserts two periods with correct commit counts.
/// Test: this test itself.
#[test]
fn period_trends_basic_windowing() {
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Alice", "alice@example.com");

    // 8 commits, one per week starting 2024-W01.
    let weeks = [
        "2024-01-01T00:00:00Z", // W01
        "2024-01-08T00:00:00Z", // W02
        "2024-01-15T00:00:00Z", // W03
        "2024-01-22T00:00:00Z", // W04
        "2024-01-29T00:00:00Z", // W05
        "2024-02-05T00:00:00Z", // W06
        "2024-02-12T00:00:00Z", // W07
        "2024-02-19T00:00:00Z", // W08
    ];
    for (i, ts) in weeks.iter().enumerate() {
        seed_commit(&db, &format!("sha{i}"), aid, ts, 0, None);
    }

    let trends =
        query_author_period_trends(&db, "alice@example.com", 4, None, None).expect("query");

    assert_eq!(trends.len(), 2, "expected 2 period windows for 8 weeks");
    assert_eq!(
        trends[0].commit_count, 4,
        "first window: 4 commits, got {}",
        trends[0].commit_count
    );
    assert_eq!(
        trends[1].commit_count, 4,
        "second window: 4 commits, got {}",
        trends[1].commit_count
    );
}

/// Why: period labels must follow the `"YYYY-Www..Www"` format and the
/// since/until fields must be valid ISO dates.
/// What: calls `query_author_period_trends`, inspects the first period's
/// metadata fields.
/// Test: this test itself.
#[test]
fn period_trends_label_and_date_format() {
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Bob", "bob@example.com");
    seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 0, None);
    seed_commit(&db, "sha2", aid, "2024-01-08T00:00:00Z", 0, None);

    let trends = query_author_period_trends(&db, "bob@example.com", 4, None, None).expect("query");
    assert!(!trends.is_empty());
    let p = &trends[0];
    assert!(
        p.period_label.contains("-W"),
        "period_label must contain '-W': {}",
        p.period_label
    );
    assert_eq!(p.since.len(), 10, "since must be YYYY-MM-DD: {}", p.since);
    assert_eq!(p.until.len(), 10, "until must be YYYY-MM-DD: {}", p.until);
}

/// Why: ticketed_pct must reflect the fraction of ticketed commits correctly.
/// What: seeds 4 commits (2 ticketed), asserts `ticketed_pct ≈ 0.5`.
/// Test: this test itself.
#[test]
fn period_trends_ticketed_pct() {
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Carol", "carol@example.com");
    seed_commit(&db, "s1", aid, "2024-01-01T00:00:00Z", 1, None);
    seed_commit(&db, "s2", aid, "2024-01-02T00:00:00Z", 1, None);
    seed_commit(&db, "s3", aid, "2024-01-03T00:00:00Z", 0, None);
    seed_commit(&db, "s4", aid, "2024-01-04T00:00:00Z", 0, None);

    let trends =
        query_author_period_trends(&db, "carol@example.com", 4, None, None).expect("query");
    assert!(!trends.is_empty());
    let pct = trends[0].ticketed_pct;
    assert!(
        (pct - 0.5).abs() < 1e-9,
        "ticketed_pct should be 0.5, got {pct}"
    );
}

/// Why: an author with no commits must return an empty Vec, not an error.
/// What: seeds an author but no commits, asserts the result is empty.
/// Test: this test itself.
#[test]
fn period_trends_empty_for_no_commits() {
    let db = Database::open_in_memory().expect("open");
    seed_author(&db, "Dave", "dave@example.com");
    let trends = query_author_period_trends(&db, "dave@example.com", 4, None, None).expect("query");
    assert!(trends.is_empty(), "no commits → empty Vec, got: {trends:?}");
}

/// Why: an unknown email must return an empty Vec (not an error), consistent
/// with the design contract.
/// What: queries for an email not in `authors`, asserts empty result.
/// Test: this test itself.
#[test]
fn period_trends_empty_for_unknown_email() {
    let db = Database::open_in_memory().expect("open");
    let trends =
        query_author_period_trends(&db, "nobody@example.com", 4, None, None).expect("query");
    assert!(trends.is_empty(), "unknown email → empty Vec");
}

/// Why: categories must be aggregated correctly per window.
/// What: seeds 3 feature + 1 bugfix commits in one window, asserts the
/// `categories` map reflects those counts.
/// Test: this test itself.
#[test]
fn period_trends_category_aggregation() {
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Eve", "eve@example.com");
    seed_commit(&db, "e1", aid, "2024-01-01T00:00:00Z", 0, Some("feature"));
    seed_commit(&db, "e2", aid, "2024-01-02T00:00:00Z", 0, Some("feature"));
    seed_commit(&db, "e3", aid, "2024-01-03T00:00:00Z", 0, Some("feature"));
    seed_commit(&db, "e4", aid, "2024-01-04T00:00:00Z", 0, Some("bugfix"));

    let trends = query_author_period_trends(&db, "eve@example.com", 4, None, None).expect("query");
    assert!(!trends.is_empty());
    let cats = &trends[0].categories;
    assert_eq!(
        cats.get("feature").copied(),
        Some(3),
        "expected 3 feature commits"
    );
    assert_eq!(
        cats.get("bugfix").copied(),
        Some(1),
        "expected 1 bugfix commit"
    );
}

/// Why: the `since`/`until` filter must constrain the data to the requested
/// date range only.
/// What: seeds commits spanning two months; queries with a 1-month filter;
/// asserts only commits in that range are counted.
/// Test: this test itself.
#[test]
fn period_trends_date_filter() {
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Frank", "frank@example.com");
    seed_commit(&db, "f1", aid, "2024-01-10T00:00:00Z", 0, None);
    seed_commit(&db, "f2", aid, "2024-01-20T00:00:00Z", 0, None);
    seed_commit(&db, "f3", aid, "2024-02-10T00:00:00Z", 0, None); // out of window

    let trends = query_author_period_trends(
        &db,
        "frank@example.com",
        4,
        Some("2024-01-01"),
        Some("2024-01-31"),
    )
    .expect("query");

    let total: u64 = trends.iter().map(|p| p.commit_count).sum();
    assert_eq!(
        total, 2,
        "filter to Jan 2024 should yield 2 commits, got {total}"
    );
}

/// Why: the helper `weeks_in_range` must produce the correct count and order.
/// What: passes a known 4-week range, asserts 4 Mondays are returned.
/// Test: this test itself.
#[test]
fn weeks_in_range_produces_correct_mondays() {
    let start = NaiveDate::from_ymd_opt(2024, 1, 1).expect("date");
    let end = NaiveDate::from_ymd_opt(2024, 1, 28).expect("date");
    let weeks = weeks_in_range(start, end);
    assert_eq!(weeks.len(), 4, "should be 4 Mondays in the range");
    for w in &weeks {
        assert_eq!(w.weekday(), chrono::Weekday::Mon, "{w} must be a Monday");
    }
}

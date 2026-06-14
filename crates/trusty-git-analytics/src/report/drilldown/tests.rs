//! Unit tests for the drilldown module.
//!
//! Why: co-locating tests with the code they exercise while staying in a
//! separate file lets the 1500-SLOC test cap apply instead of the 500-SLOC
//! production cap.
//! What: tests for DB query functions, formatter output, and the data model.
//! Test: this file IS the test suite.

use std::collections::HashMap;

use rusqlite::params;

use crate::core::db::Database;

use super::format::{format_json, format_markdown};
use super::model::{AuthorDrilldownData, CommitSection, EffortSection, PrSection, ReportPeriod};
use super::queries::{
    extract_provider_logins, query_commit_summary, query_effort_histogram, query_pr_metrics,
};

fn seed_author(db: &Database, name: &str, email: &str, aliases_json: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO authors (canonical_name, canonical_email, aliases) \
             VALUES (?1, ?2, ?3)",
            params![name, email, aliases_json],
        )
        .expect("insert author");
    db.connection().last_insert_rowid()
}

fn seed_commit(db: &Database, sha: &str, author_id: i64, timestamp: &str, ticketed: i64) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, \
             message, repository, insertions, deletions) \
             VALUES (?1, ?2, 'n', 'e', ?3, 'm', 'repo-a', 10, 5)",
            params![sha, author_id, timestamp],
        )
        .expect("insert commit");
    if ticketed != 0 {
        db.connection()
            .execute(
                "UPDATE commits SET ticketed = 1 WHERE sha = ?1",
                params![sha],
            )
            .expect("set ticketed");
    }
}

fn seed_effort(db: &Database, sha: &str, size: &str) {
    db.connection()
        .execute(
            "INSERT INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, computed_at) \
             VALUES (?1, 'repo-a', ?2, 1.0, 10, 1, 0, 1.0, 0)",
            params![sha, size],
        )
        .expect("insert effort");
}

/// Global PR counter so each test gets unique `pr_number` values even when
/// sharing the same `repository` + `provider` (which would otherwise trip
/// the UNIQUE constraint added in migration v12).
static PR_COUNTER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

fn seed_pr(db: &Database, author: &str, state: &str, created_at: &str, merged_at: Option<&str>) {
    let pr_num = PR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    db.connection()
        .execute(
            "INSERT INTO pull_requests (pr_number, title, author, state, created_at, merged_at, commit_shas) \
             VALUES (?1, 'title', ?2, ?3, ?4, ?5, '[]')",
            params![pr_num, author, state, created_at, merged_at],
        )
        .expect("insert pr");
}

#[test]
fn effort_histogram_counts() {
    // Why: verifies the size → count grouping with a mix of bucket sizes.
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
    seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 0);
    seed_commit(&db, "sha2", aid, "2024-01-02T00:00:00Z", 0);
    seed_commit(&db, "sha3", aid, "2024-01-03T00:00:00Z", 0);
    seed_effort(&db, "sha1", "S");
    seed_effort(&db, "sha2", "S");
    seed_effort(&db, "sha3", "L");

    let h = query_effort_histogram(&db, "alice@example.com", None, None).expect("query");
    assert_eq!(h.total_commits, 3);
    assert_eq!(h.scored_commits, 3);
    assert_eq!(h.histogram.get("S").copied(), Some(2));
    assert_eq!(h.histogram.get("L").copied(), Some(1));
}

#[test]
fn effort_histogram_empty_when_no_effort_rows() {
    // Why: if backfill effort has not been run, histogram should show 0 scored.
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
    seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 0);

    let h = query_effort_histogram(&db, "alice@example.com", None, None).expect("query");
    assert_eq!(h.total_commits, 1);
    assert_eq!(h.scored_commits, 0);
    assert!(h.histogram.is_empty());
}

#[test]
fn pr_metrics_basic() {
    // Why: validates total/merged counts and cycle-time computation.
    let db = Database::open_in_memory().expect("open");
    // 2 merged PRs, each ~24h cycle time.
    seed_pr(
        &db,
        "alice-gh",
        "merged",
        "2024-01-01T00:00:00Z",
        Some("2024-01-02T00:00:00Z"),
    );
    seed_pr(
        &db,
        "alice-gh",
        "merged",
        "2024-01-03T00:00:00Z",
        Some("2024-01-04T00:00:00Z"),
    );
    seed_pr(&db, "alice-gh", "open", "2024-01-05T00:00:00Z", None);

    let logins = vec!["alice-gh".to_string()];
    let m = query_pr_metrics(&db, &logins, None, None).expect("query");
    assert_eq!(m.total, 3);
    assert_eq!(m.merged, 2);
    assert!(m.avg_cycle_time_hours.is_some());
    let avg = m.avg_cycle_time_hours.unwrap();
    assert!((avg - 24.0).abs() < 0.01, "avg should be ~24h, got {avg}");
    assert!(m.median_cycle_time_hours.is_some());
    // p95 requires >= 20 merged PRs; should be None here.
    assert!(m.p95_cycle_time_hours.is_none());
}

#[test]
fn pr_metrics_no_prs() {
    // Why: when no logins match, all metrics should be None.
    let db = Database::open_in_memory().expect("open");
    let logins = vec!["nobody".to_string()];
    let m = query_pr_metrics(&db, &logins, None, None).expect("query");
    assert_eq!(m.total, 0);
    assert_eq!(m.merged, 0);
    assert!(m.avg_cycle_time_hours.is_none());
}

#[test]
fn pr_metrics_p95_requires_20_prs() {
    // Why: p95 is a misleading statistic on small samples; gate at 20.
    let db = Database::open_in_memory().expect("open");
    // Seed exactly 20 merged PRs.
    for i in 0..20u32 {
        let created = format!("2024-01-{:02}T00:00:00Z", (i % 28) + 1);
        let merged = format!("2024-01-{:02}T12:00:00Z", (i % 28) + 1);
        seed_pr(&db, "alice-gh", "merged", &created, Some(&merged));
    }
    let logins = vec!["alice-gh".to_string()];
    let m = query_pr_metrics(&db, &logins, None, None).expect("query");
    assert_eq!(m.merged, 20);
    assert!(
        m.p95_cycle_time_hours.is_some(),
        "p95 should appear at n=20"
    );
}

#[test]
fn commit_summary_basic() {
    // Why: validates total, ticketed, repository list, and timestamp fields.
    let db = Database::open_in_memory().expect("open");
    let aid = seed_author(&db, "Alice", "alice@example.com", "[]");
    seed_commit(&db, "sha1", aid, "2024-01-01T00:00:00Z", 1);
    seed_commit(&db, "sha2", aid, "2024-01-02T00:00:00Z", 0);

    let s = query_commit_summary(&db, "alice@example.com", None, None).expect("query");
    assert_eq!(s.total_commits, 2);
    assert_eq!(s.ticketed_commits, 1);
    assert_eq!(s.repositories, vec!["repo-a"]);
    assert!(s.first_commit.is_some());
    assert!(s.last_commit.is_some());
}

#[test]
fn commit_summary_no_commits() {
    // Why: an author with no commits in scope should return zeros, not panic.
    let db = Database::open_in_memory().expect("open");
    seed_author(&db, "Alice", "alice@example.com", "[]");

    let s = query_commit_summary(&db, "alice@example.com", None, None).expect("query");
    assert_eq!(s.total_commits, 0);
    assert!(s.first_commit.is_none());
    assert!(s.repositories.is_empty());
}

#[test]
fn extract_logins_from_aliases() {
    // Why: the login-extraction logic must skip email aliases and return only logins.
    let json = r#"["alice@example.com","alice-old@example.com","alice-dev","alice-gh"]"#;
    let logins = extract_provider_logins(json);
    assert_eq!(logins, vec!["alice-dev", "alice-gh"]);
}

fn make_sample_drilldown() -> AuthorDrilldownData {
    let mut histogram = HashMap::new();
    histogram.insert("XS".to_string(), 5u32);
    histogram.insert("S".to_string(), 10u32);
    histogram.insert("M".to_string(), 3u32);

    let mut categories = HashMap::new();
    categories.insert("feature".to_string(), 8usize);
    categories.insert("bugfix".to_string(), 4usize);

    AuthorDrilldownData {
        generated_at: "2026-05-28T10:00:00Z".to_string(),
        email: "alice@example.com".to_string(),
        name: "Alice Smith".to_string(),
        period: ReportPeriod {
            since: Some("2025-01-01".to_string()),
            until: Some("2026-05-28".to_string()),
        },
        commits: CommitSection {
            total: 18,
            ticketed: 7,
            ticket_coverage: Some(7.0 / 18.0),
            repositories: vec!["acme/api".to_string()],
            first_commit: Some("2025-01-07T09:12:00Z".to_string()),
            last_commit: Some("2026-05-22T16:44:00Z".to_string()),
            insertions: 500,
            deletions: 200,
        },
        effort: EffortSection {
            scored_commits: 18,
            total_commits: 18,
            histogram,
        },
        pull_requests: PrSection {
            total: 10,
            merged: 9,
            avg_cycle_time_hours: Some(14.3),
            median_cycle_time_hours: Some(9.1),
            p95_cycle_time_hours: None,
        },
        categories,
    }
}

#[test]
fn format_markdown_contains_headers() {
    // Why: asserts the mandatory section headers and key values appear in output.
    let data = make_sample_drilldown();
    let md = format_markdown(&data);
    assert!(md.contains("# Engineer Report: Alice Smith <alice@example.com>"));
    assert!(md.contains("## Summary"));
    assert!(md.contains("## Effort Histogram"));
    assert!(md.contains("## Pull Request Metrics"));
    assert!(md.contains("## Category Breakdown"));
    assert!(md.contains("feature"));
    assert!(md.contains("acme/api"));
    assert!(md.contains("18")); // total commits
}

#[test]
fn format_json_parses() {
    // Why: the JSON output must round-trip through serde without loss.
    let data = make_sample_drilldown();
    let json_str = format_json(&data).expect("json");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid json");
    assert_eq!(parsed["email"].as_str(), Some("alice@example.com"));
    assert_eq!(parsed["commits"]["total"].as_u64(), Some(18));
    assert_eq!(
        parsed["effort"]["histogram"]["XS"].as_u64(),
        Some(5),
        "XS bucket should be 5"
    );
    assert!(
        parsed["pull_requests"]["p95_cycle_time_hours"].is_null(),
        "p95 should be null when None"
    );
}

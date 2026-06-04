//! GitHub PR reviewer persistence for the `pr_reviewers` table.
//!
//! Why: the ADO PR fetcher populates `pr_reviewers` via `upsert_pr_reviewer`
//! (in `azdo::pr_fetcher`), but no equivalent existed for GitHub reviews.
//! This module provides the missing GitHub side, writing `(reviewer_id,
//! review_state, submitted_at)` rows for each `GitHubReview` returned by the
//! reviews API. Closes issue #742.
//!
//! What: one free function, [`upsert_github_pr_reviewer`], that maps a
//! `GitHubReview` to a `pr_reviewers` row with `provider='github'` and
//! `vote=NULL`. Row identity is the UNIQUE(pr_id, provider, reviewer_id)
//! index — re-running is idempotent.
//!
//! Test: `upsert_and_roundtrip_github_reviewer` and friends below.

use rusqlite::{params, Connection};
use tracing::debug;

use crate::collect::github::client::GitHubReview;
use crate::core::errors::{Result, TgaError};

/// Upsert a single GitHub review into `pr_reviewers`.
///
/// Why: mirrors `azdo::pr_fetcher::upsert_pr_reviewer` for the GitHub
/// provider; uses INSERT OR REPLACE so re-running collection refreshes the
/// review state without duplicating rows (idempotent by design).
/// What: inserts `provider='github'`, `reviewer_id` = login,
/// `review_state` = state string, `submitted_at` from the review, and
/// `vote=0` (schema default; GitHub has no numeric vote; `review_state`
/// distinguishes GitHub rows from ADO rows), `is_required=0`,
/// `is_container=0` (ADO fields not applicable to GitHub).
/// Test: `upsert_and_roundtrip_github_reviewer` in this module.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn upsert_github_pr_reviewer(
    conn: &Connection,
    pr_db_id: i64,
    review: &GitHubReview,
) -> Result<()> {
    // Reviews from deleted accounts have no user; skip them.
    let login = match review.user.as_ref().map(|u| u.login.as_str()) {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => {
            debug!(
                pr_id = pr_db_id,
                review_id = review.id,
                "skipping GitHub review with missing/empty user login"
            );
            return Ok(());
        }
    };

    // `vote` is INTEGER NOT NULL DEFAULT 0 (the ADO schema constraint);
    // GitHub has no numeric vote — we store 0 (the schema default) and
    // use `review_state` to distinguish GitHub rows from ADO rows.
    conn.execute(
        "INSERT OR REPLACE INTO pr_reviewers \
         (pr_id, provider, reviewer_id, display_name, vote, is_required, is_container, \
          review_state, submitted_at) \
         VALUES (?1, 'github', ?2, NULL, 0, 0, 0, ?3, ?4)",
        params![pr_db_id, login, review.state, review.submitted_at,],
    )
    .map_err(TgaError::from)?;
    Ok(())
}

/// Look up the `id` of a GitHub PR row by `(repository, pr_number)`.
///
/// Why: reviewer storage needs the `pr_reviewers.pr_id` FK, which is the
/// auto-incremented primary key of `pull_requests`. This helper abstracts
/// the query so the caller (reviewer-ingestion loop) is clean.
/// What: queries `pull_requests` for provider='github', returns the rowid.
///       Returns `Ok(None)` when the PR isn't in the DB yet (skip gracefully).
/// Test: `lookup_github_pr_id_returns_none_when_absent` below.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure (e.g. table missing).
pub fn lookup_github_pr_id(
    conn: &Connection,
    repository: &str,
    pr_number: u64,
) -> Result<Option<i64>> {
    let result: rusqlite::Result<i64> = conn.query_row(
        "SELECT id FROM pull_requests \
         WHERE provider = 'github' AND repository = ?1 AND pr_number = ?2",
        params![repository, pr_number as i64],
        |row| row.get(0),
    );
    match result {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(TgaError::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::github::client::{GhUser, GitHubReview};
    use crate::core::db::Database;
    use rusqlite::params;

    fn open_db() -> Database {
        Database::open_in_memory().expect("open db")
    }

    /// Seed a pull_requests row so reviewer FK constraints are satisfied.
    fn seed_pr(conn: &Connection, repository: &str, pr_number: i64) -> i64 {
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES ('github', ?1, ?2, 'T', 'u', 'open', '2024-01-01T00:00:00Z', '[]')",
            params![repository, pr_number],
        )
        .expect("seed pr");
        conn.last_insert_rowid()
    }

    /// Why: basic round-trip ensures `upsert_github_pr_reviewer` writes all
    /// columns and the UNIQUE index allows idempotent re-insertion.
    /// What: upsert a review, read it back, assert all columns, then upsert
    /// again with a different state and confirm the row is replaced (not
    /// duplicated).
    /// Test: this test.
    #[test]
    fn upsert_and_roundtrip_github_reviewer() {
        let db = open_db();
        let conn = db.connection();
        let pr_id = seed_pr(conn, "acme/widget", 42);

        let review = GitHubReview {
            id: 999,
            state: "APPROVED".to_string(),
            user: Some(GhUser {
                login: "octocat".to_string(),
            }),
            submitted_at: Some("2024-06-01T12:00:00Z".to_string()),
        };

        upsert_github_pr_reviewer(conn, pr_id, &review).expect("upsert");

        // Read back and verify all columns.
        let (provider, reviewer_id, review_state, submitted_at, vote): (
            String,
            String,
            String,
            Option<String>,
            i64,
        ) = conn
            .query_row(
                "SELECT provider, reviewer_id, review_state, submitted_at, vote \
                 FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("read back");

        assert_eq!(provider, "github");
        assert_eq!(reviewer_id, "octocat");
        assert_eq!(review_state, "APPROVED");
        assert_eq!(submitted_at.as_deref(), Some("2024-06-01T12:00:00Z"));
        // vote is stored as 0 (schema default) — GitHub has no numeric vote;
        // review_state distinguishes GitHub rows from ADO rows.
        assert_eq!(vote, 0, "GitHub reviewer vote must be 0 (schema default)");

        // Idempotent upsert: update state, confirm single row.
        let review2 = GitHubReview {
            id: 999,
            state: "CHANGES_REQUESTED".to_string(),
            user: Some(GhUser {
                login: "octocat".to_string(),
            }),
            submitted_at: Some("2024-06-02T12:00:00Z".to_string()),
        };
        upsert_github_pr_reviewer(conn, pr_id, &review2).expect("re-upsert");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "UPSERT must not duplicate the reviewer row");

        let new_state: String = conn
            .query_row(
                "SELECT review_state FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_id],
                |r| r.get(0),
            )
            .expect("read state");
        assert_eq!(new_state, "CHANGES_REQUESTED");
    }

    /// Why: reviews from deleted GitHub accounts have no user object; we must
    /// skip them gracefully rather than inserting a NULL reviewer_id that
    /// violates the NOT NULL constraint.
    /// What: upsert a review with `user: None`, assert no row is written.
    /// Test: this test.
    #[test]
    fn upsert_skips_review_with_missing_user() {
        let db = open_db();
        let conn = db.connection();
        let pr_id = seed_pr(conn, "acme/widget", 99);

        let review = GitHubReview {
            id: 1,
            state: "COMMENTED".to_string(),
            user: None,
            submitted_at: None,
        };
        upsert_github_pr_reviewer(conn, pr_id, &review).expect("should not error");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 0,
            "no row should be written for a review with no user"
        );
    }

    /// Why: the lookup helper must return None when the PR isn't in the DB,
    /// rather than propagating a rusqlite::Error::QueryReturnedNoRows.
    /// What: query against an empty DB.
    /// Test: this test.
    #[test]
    fn lookup_github_pr_id_returns_none_when_absent() {
        let db = open_db();
        let result = lookup_github_pr_id(db.connection(), "acme/widget", 42).expect("no sql error");
        assert!(result.is_none());
    }

    /// Why: after a PR is seeded, lookup_github_pr_id must return its rowid.
    /// What: seed a PR, then call the helper.
    /// Test: this test.
    #[test]
    fn lookup_github_pr_id_finds_seeded_pr() {
        let db = open_db();
        let conn = db.connection();
        let pr_id = seed_pr(conn, "org/repo", 7);
        let found = lookup_github_pr_id(conn, "org/repo", 7).expect("no sql error");
        assert_eq!(found, Some(pr_id));
    }
}

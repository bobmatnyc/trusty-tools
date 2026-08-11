//! Period-batch assembly for the contributor-profile pipeline.
//!
//! Why: the profile reasons in periods, not commits — "how did Q1 compare to
//! Q2" is the question it answers. This module turns tga's period-trend rows
//! into the batch shape the rest of the pipeline consumes.
//! What: [`assemble_period_batches`] calls
//! [`query_author_period_trends`](crate::report::period_trends::query_author_period_trends)
//! at the requested window size and wraps each summary in a [`PeriodBatch`].
//! Diff population is a separate pass — see [`crate::profile::diff_sampler`].
//! Test: the `tests` module seeds an in-memory database with weekly commits and
//! asserts the bucketing for every window variant.

use tracing::debug;

use crate::core::db::Database;
use crate::report::period_trends::query_author_period_trends;

use super::error::{ProfileError, Result};
use super::types::PeriodBatch;

// ─── Window enum ─────────────────────────────────────────────────────────────

/// Granularity of the period windows used for batching.
///
/// Why: the useful granularity depends on the reader — quarterly for a
/// management summary, weekly for a detailed audit.
/// What: maps to the `window_weeks` integer `query_author_period_trends` takes.
/// Test: `tests::window_to_weeks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// 13-week (roughly quarterly) periods.
    Quarterly,
    /// 4-week (roughly monthly) periods.
    Monthly,
    /// 1-week periods.
    Weekly,
    /// Custom window size in weeks.
    Custom(u32),
}

impl Window {
    /// Map the variant to its `window_weeks` value.
    ///
    /// Why: keeps 13/4/1 out of every call site.
    /// What: returns 13/4/1 for Quarterly/Monthly/Weekly; `Custom(n)` returns
    /// `n` with a floor of 1, since a zero-week window would divide by zero
    /// downstream.
    /// Test: `tests::window_to_weeks`.
    pub fn window_weeks(self) -> u32 {
        match self {
            Window::Quarterly => 13,
            Window::Monthly => 4,
            Window::Weekly => 1,
            Window::Custom(n) => n.max(1),
        }
    }
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Assemble period batches for one contributor.
///
/// Why: every later stage — diff sampling, trend tagging, rendering — operates
/// per period, so the period split is the pipeline's first real step.
/// What: queries the period trends for `canonical_email` at `window`
/// granularity within the optional `[since, until]` bounds, and wraps each
/// summary in a diff-less [`PeriodBatch`]. A contributor with no commits in
/// scope yields an empty `Vec`, not an error — an unproductive window is a
/// valid answer.
///
/// # Parameters
///
/// - `db` — open tga database (read-only use)
/// - `canonical_email` — as stored in `authors.canonical_email`
/// - `window` — period granularity
/// - `since` / `until` — optional inclusive ISO 8601 date bounds
///
/// # Errors
///
/// [`ProfileError::Report`] when the underlying trend query fails.
///
/// Test: `tests::assemble_quarterly_batches`, `tests::assemble_monthly_batches`,
/// `tests::assemble_with_date_filter`, `tests::assemble_empty_for_no_commits`.
pub fn assemble_period_batches(
    db: &Database,
    canonical_email: &str,
    window: Window,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<PeriodBatch>> {
    let window_weeks = window.window_weeks();
    debug!(
        canonical_email,
        window_weeks, since, until, "assembling period batches"
    );

    let summaries = query_author_period_trends(db, canonical_email, window_weeks, since, until)
        .map_err(ProfileError::Report)?;

    Ok(summaries.into_iter().map(PeriodBatch::from_stats).collect())
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

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

    fn seed_commit(db: &Database, sha: &str, author_id: i64, timestamp: &str) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_id, author_name, author_email, \
                 timestamp, message, repository, insertions, deletions) \
                 VALUES (?1, ?2, 'n', 'e', ?3, 'm', 'repo-a', 5, 2)",
                params![sha, author_id, timestamp],
            )
            .expect("insert commit");
    }

    /// One commit per week for `n` weeks starting 2024-01-01.
    fn weekly_timestamps(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let day = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date")
                    + chrono::Duration::weeks(i as i64);
                format!("{day}T00:00:00Z")
            })
            .collect()
    }

    /// Why: the window-to-weeks mapping is what every batch boundary depends
    /// on, and a zero-week custom window would divide by zero downstream.
    /// What: asserts 13/4/1 for the named variants and that `Custom(0)` floors
    /// to 1.
    /// Test: this test itself.
    #[test]
    fn window_to_weeks() {
        assert_eq!(Window::Quarterly.window_weeks(), 13);
        assert_eq!(Window::Monthly.window_weeks(), 4);
        assert_eq!(Window::Weekly.window_weeks(), 1);
        assert_eq!(Window::Custom(8).window_weeks(), 8);
        assert_eq!(Window::Custom(0).window_weeks(), 1, "Custom(0) floors to 1");
    }

    /// Why: a quarterly window must collect a full 13 weeks into one batch —
    /// splitting them would misreport the contributor's cadence.
    /// What: seeds 13 weekly commits and asserts one batch holding all 13, with
    /// no diffs attached yet.
    /// Test: this test itself.
    #[test]
    fn assemble_quarterly_batches() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");
        for (i, ts) in weekly_timestamps(13).iter().enumerate() {
            seed_commit(&db, &format!("sha{i}"), aid, ts);
        }

        let batches =
            assemble_period_batches(&db, "alice@example.com", Window::Quarterly, None, None)
                .expect("assemble");

        assert_eq!(batches.len(), 1, "13 weeks in one quarterly bucket");
        assert_eq!(batches[0].stats.commit_count, 13);
        assert!(
            batches[0].sampled_diffs.is_empty(),
            "sampled_diffs must stay empty until the diff sampler runs"
        );
    }

    /// Why: the same commits must split differently at a finer granularity —
    /// that is the whole point of the window parameter.
    /// What: seeds 8 weekly commits and asserts two monthly batches summing to
    /// 8 commits.
    /// Test: this test itself.
    #[test]
    fn assemble_monthly_batches() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Bob", "bob@example.com");
        for (i, ts) in weekly_timestamps(8).iter().enumerate() {
            seed_commit(&db, &format!("bsha{i}"), aid, ts);
        }

        let batches = assemble_period_batches(&db, "bob@example.com", Window::Monthly, None, None)
            .expect("assemble");

        assert_eq!(batches.len(), 2, "8 weeks → 2 monthly batches");
        assert_eq!(
            batches[0].stats.commit_count + batches[1].stats.commit_count,
            8
        );
    }

    /// Why: a profile is always bounded to a window the caller asked for;
    /// commits outside it must not leak in.
    /// What: seeds commits in January and February, filters to January, and
    /// asserts only the two January commits are counted.
    /// Test: this test itself.
    #[test]
    fn assemble_with_date_filter() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Carol", "carol@example.com");
        seed_commit(&db, "c1", aid, "2024-01-08T00:00:00Z");
        seed_commit(&db, "c2", aid, "2024-01-15T00:00:00Z");
        seed_commit(&db, "c3", aid, "2024-02-05T00:00:00Z");

        let batches = assemble_period_batches(
            &db,
            "carol@example.com",
            Window::Monthly,
            Some("2024-01-01"),
            Some("2024-01-31"),
        )
        .expect("assemble");

        let total: u64 = batches.iter().map(|b| b.stats.commit_count).sum();
        assert_eq!(total, 2, "only the two January commits are in range");
    }

    /// Why: profiling someone with no commits in the window is a normal
    /// request; erroring would abort a whole-org sweep on the first quiet
    /// month.
    /// What: seeds an author with no commits and asserts an empty `Vec`.
    /// Test: this test itself.
    #[test]
    fn assemble_empty_for_no_commits() {
        let db = Database::open_in_memory().expect("open");
        seed_author(&db, "Dave", "dave@example.com");

        let batches =
            assemble_period_batches(&db, "dave@example.com", Window::Quarterly, None, None)
                .expect("assemble");

        assert!(batches.is_empty(), "no commits → empty Vec");
    }

    /// Why: the diff sampler queries commits using the batch's `since`/`until`,
    /// so malformed period metadata would silently sample nothing.
    /// What: asserts the propagated label carries a week marker and both bounds
    /// are `YYYY-MM-DD`.
    /// Test: this test itself.
    #[test]
    fn assemble_period_label_propagated() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Eve", "eve@example.com");
        seed_commit(&db, "e1", aid, "2024-01-08T00:00:00Z");

        let batches = assemble_period_batches(&db, "eve@example.com", Window::Weekly, None, None)
            .expect("assemble");

        assert!(!batches.is_empty());
        let p = &batches[0].stats;
        assert!(
            p.period_label.contains("-W"),
            "period_label must contain '-W': {}",
            p.period_label
        );
        assert_eq!(p.since.len(), 10, "since must be YYYY-MM-DD: {}", p.since);
        assert_eq!(p.until.len(), 10, "until must be YYYY-MM-DD: {}", p.until);
    }

    /// Why: a custom window has to reach the query unchanged, otherwise the
    /// variant is decorative.
    /// What: seeds 4 weekly commits with `Custom(2)` and asserts 2 batches.
    /// Test: this test itself.
    #[test]
    fn assemble_custom_window() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Frank", "frank@example.com");
        for (i, ts) in weekly_timestamps(4).iter().enumerate() {
            seed_commit(&db, &format!("fsha{i}"), aid, ts);
        }

        let batches =
            assemble_period_batches(&db, "frank@example.com", Window::Custom(2), None, None)
                .expect("assemble");

        assert_eq!(batches.len(), 2, "4 weeks with Custom(2) → 2 batches");
    }
}

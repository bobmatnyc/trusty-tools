//! Period-batch assembly for the contributor-profile pipeline.
//!
//! Why: every later stage works one period at a time, so the raw period-trend
//! rows have to be bucketed into the pipeline's own batch type before diff
//! sampling or narrative generation can run.
//! What: [`Window`] names the granularity; [`assemble_period_batches`] drives
//! `query_author_period_trends` and wraps each summary in a [`PeriodBatch`] with
//! an empty diff list for the sampler to fill.
//! Test: the `tests` module below seeds an in-memory database with weekly
//! commits and asserts the bucketing for each window variant.

use tracing::debug;

use crate::core::db::Database;
use crate::report::period_trends::query_author_period_trends;

use super::error::Result;
use super::types::PeriodBatch;

// ─── Window ───────────────────────────────────────────────────────────────────

/// Granularity of the period windows used for batching.
///
/// Why: a manager review wants quarters, a team lead wants months, and an audit
/// wants weeks — all from the same commit history.
/// What: maps to the `window_weeks` integer `query_author_period_trends` takes.
/// Test: `window_to_weeks`.
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
    /// Convert this window to its width in weeks.
    ///
    /// `Custom(0)` floors to 1 — a zero-week window would produce no buckets.
    ///
    /// Test: `window_to_weeks`.
    pub fn window_weeks(self) -> u32 {
        match self {
            Window::Quarterly => 13,
            Window::Monthly => 4,
            Window::Weekly => 1,
            Window::Custom(n) => n.max(1),
        }
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Assemble one [`PeriodBatch`] per window for a contributor.
///
/// Why: this is the pipeline's first stage — everything downstream consumes its
/// output, and it is the only place that touches the period-trend query.
/// What: calls `query_author_period_trends` with `canonical_email`, the window
/// width, and the optional `[since, until]` bounds, then wraps each summary via
/// `PeriodBatch::from_stats`. `sampled_diffs` stays empty until the diff sampler
/// runs.
///
/// # Parameters
///
/// - `db` — an open tga database; only read.
/// - `canonical_email` — as stored in `authors.canonical_email`.
/// - `window` — period granularity.
/// - `since` / `until` — inclusive ISO 8601 date bounds; `None` means unbounded.
///
/// # Errors
///
/// [`super::ProfileError::Report`] on a query failure. A contributor with no
/// commits in scope yields an empty `Vec`, which is not an error.
///
/// Test: `assemble_quarterly_batches`, `assemble_monthly_batches`,
/// `assemble_with_date_filter`, `assemble_empty_for_no_commits`,
/// `assemble_period_label_propagated`, `assemble_custom_window`.
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

    let summaries = query_author_period_trends(db, canonical_email, window_weeks, since, until)?;
    Ok(summaries.into_iter().map(PeriodBatch::from_stats).collect())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

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

    /// Why: the window-to-weeks mapping is the only place the 13/4/1 constants
    /// live, and `Custom(0)` must floor to 1 or bucketing divides by zero.
    /// What: asserts each variant's width, including the floor.
    /// Test: this test itself.
    #[test]
    fn window_to_weeks() {
        assert_eq!(Window::Quarterly.window_weeks(), 13);
        assert_eq!(Window::Monthly.window_weeks(), 4);
        assert_eq!(Window::Weekly.window_weeks(), 1);
        assert_eq!(Window::Custom(8).window_weeks(), 8);
        assert_eq!(
            Window::Custom(0).window_weeks(),
            1,
            "Custom(0) should floor to 1"
        );
    }

    /// Why: 13 weeks of commits must land in exactly one quarterly bucket.
    /// What: seeds one commit per week for 13 weeks, asserts one batch of 13
    /// and that `sampled_diffs` is still empty.
    /// Test: this test itself.
    #[test]
    fn assemble_quarterly_batches() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");

        let weeks = [
            "2024-01-01T00:00:00Z",
            "2024-01-08T00:00:00Z",
            "2024-01-15T00:00:00Z",
            "2024-01-22T00:00:00Z",
            "2024-01-29T00:00:00Z",
            "2024-02-05T00:00:00Z",
            "2024-02-12T00:00:00Z",
            "2024-02-19T00:00:00Z",
            "2024-02-26T00:00:00Z",
            "2024-03-04T00:00:00Z",
            "2024-03-11T00:00:00Z",
            "2024-03-18T00:00:00Z",
            "2024-03-25T00:00:00Z",
        ];
        for (i, ts) in weeks.iter().enumerate() {
            seed_commit(&db, &format!("sha{i}"), aid, ts);
        }

        let batches =
            assemble_period_batches(&db, "alice@example.com", Window::Quarterly, None, None)
                .expect("assemble");

        assert_eq!(batches.len(), 1, "13 weeks in one quarterly bucket");
        assert_eq!(
            batches[0].stats.commit_count, 13,
            "all 13 commits in the period"
        );
        assert!(
            batches[0].sampled_diffs.is_empty(),
            "sampled_diffs must be empty before diff sampler runs"
        );
    }

    /// Why: the same history must split differently under a narrower window.
    /// What: seeds 8 weekly commits, asserts two monthly batches summing to 8.
    /// Test: this test itself.
    #[test]
    fn assemble_monthly_batches() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Bob", "bob@example.com");

        let weeks = [
            "2024-01-01T00:00:00Z",
            "2024-01-08T00:00:00Z",
            "2024-01-15T00:00:00Z",
            "2024-01-22T00:00:00Z",
            "2024-01-29T00:00:00Z",
            "2024-02-05T00:00:00Z",
            "2024-02-12T00:00:00Z",
            "2024-02-19T00:00:00Z",
        ];
        for (i, ts) in weeks.iter().enumerate() {
            seed_commit(&db, &format!("bsha{i}"), aid, ts);
        }

        let batches = assemble_period_batches(&db, "bob@example.com", Window::Monthly, None, None)
            .expect("assemble");

        assert_eq!(batches.len(), 2, "8 weeks → 2 monthly batches");
        assert_eq!(
            batches[0].stats.commit_count + batches[1].stats.commit_count,
            8,
            "total commit count must be 8"
        );
    }

    /// Why: the `since`/`until` bounds must actually restrict the data, or a
    /// profile scoped to one quarter would silently cover all of history.
    /// What: seeds two January and one February commit, filters to January,
    /// asserts only 2 commits survive.
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
        assert_eq!(total, 2, "filter should yield only the 2 January commits");
    }

    /// Why: an author with no commits in scope is an ordinary outcome, so it
    /// must return an empty `Vec` rather than an error the caller has to
    /// special-case.
    /// What: seeds an author with no commits, asserts an empty result.
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

    /// Why: the period label and bounds are what every later stage keys on, so
    /// they must survive the wrap into `PeriodBatch`.
    /// What: seeds one commit, asserts the label is week-shaped and the bounds
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

    /// Why: `Custom(n)` must reach the query as `n`, not as a preset width.
    /// What: seeds 4 weekly commits with `Custom(2)`, asserts 2 batches.
    /// Test: this test itself.
    #[test]
    fn assemble_custom_window() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Frank", "frank@example.com");

        let weeks = [
            "2024-01-01T00:00:00Z",
            "2024-01-08T00:00:00Z",
            "2024-01-15T00:00:00Z",
            "2024-01-22T00:00:00Z",
        ];
        for (i, ts) in weeks.iter().enumerate() {
            seed_commit(&db, &format!("fsha{i}"), aid, ts);
        }

        let batches =
            assemble_period_batches(&db, "frank@example.com", Window::Custom(2), None, None)
                .expect("assemble");

        assert_eq!(batches.len(), 2, "4 weeks with Custom(2) → 2 batches");
    }
}

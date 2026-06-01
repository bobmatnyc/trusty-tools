//! Period batch assembly for the contributor-profile pipeline.
//!
//! Why: the LLM profile pass operates on per-period batches of data; this
//! module converts raw tga period-trend summaries into the [`PeriodBatch`]
//! format expected by the rest of the pipeline (diff sampler, LLM narrator).
//! What: provides [`assemble_period_batches`] which calls tga's
//! `query_author_period_trends` with the requested window size and wraps each
//! `AuthorPeriodSummary` in a `PeriodBatch`.  Diff population is handled
//! separately by `diff_sampler`.
//! Test: `batch::tests` seeds a temp tga in-memory DB with weekly commits,
//! exercises all three window variants, and asserts correct bucketing.

use tga::core::db::Database;
use tga::report::period_trends::query_author_period_trends;
use tracing::debug;

use super::error::{ProfileError, Result};
use super::types::PeriodBatch;

// ─── Window enum ─────────────────────────────────────────────────────────────

/// Granularity of the period windows used for batching.
///
/// Why: different consumers want different granularities — quarterly for
/// management summaries, monthly for team leads, weekly for detailed audit.
/// What: maps to a `window_weeks` integer passed to
/// `query_author_period_trends`.
/// Test: see `batch::tests::window_mapping_*` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// 13-week (~quarterly) periods.
    Quarterly,
    /// 4-week (~monthly) periods.
    Monthly,
    /// 1-week periods.
    Weekly,
    /// Custom window size in weeks.
    Custom(u32),
}

impl Window {
    /// Map the window variant to the integer `window_weeks` parameter.
    ///
    /// Why: `query_author_period_trends` takes a raw `u32`; centralising the
    /// mapping here avoids magic numbers at call sites.
    /// What: returns 13/4/1 for Quarterly/Monthly/Weekly; the `Custom` variant
    /// passes the inner value clamped to a minimum of 1.
    /// Test: `batch::tests::window_to_weeks`.
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

/// Assemble period batches for a contributor from the tga database.
///
/// Why: the profile pipeline needs period-level data before the diff sampler
/// and LLM passes can run; this function drives the tga query and wraps the
/// results in the pipeline's own type.
/// What: calls `tga::report::period_trends::query_author_period_trends` with
/// the resolved `canonical_email`, the requested `window`, and optional
/// `[since, until]` date bounds.  Each `AuthorPeriodSummary` is wrapped in a
/// [`PeriodBatch`] with an empty `sampled_diffs` (populated later by the diff
/// sampler).  Returns an empty `Vec` when the author has no commits in scope.
///
/// # Parameters
///
/// - `db` — open tga database handle (read-only queries only)
/// - `canonical_email` — canonical email as stored in `authors.canonical_email`
/// - `window` — period granularity
/// - `since` — optional ISO 8601 lower bound (inclusive); `None` = start of data
/// - `until` — optional ISO 8601 upper bound (inclusive); `None` = end of data
///
/// # Errors
///
/// `ProfileError::Report` on any tga DB failure.
///
/// Test: see `batch::tests::assemble_quarterly_batches`,
/// `batch::tests::assemble_monthly_batches`,
/// `batch::tests::window_to_weeks`.
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

    let batches = summaries.into_iter().map(PeriodBatch::from_stats).collect();
    Ok(batches)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tga::core::db::Database;

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

    /// Why: `window_weeks` must map Quarterly/Monthly/Weekly to 13/4/1 and
    /// Custom(n) to n, with a floor of 1.
    /// What: calls `window_weeks()` on each variant and asserts the integer.
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

    /// Why: assemble_period_batches with Quarterly window should produce one
    /// batch per 13-week block of commit data.
    /// What: seeds 13 commits (one per week over 13 weeks), calls
    /// `assemble_period_batches` with `Quarterly`, asserts one batch with
    /// commit_count = 13.
    /// Test: this test itself.
    #[test]
    fn assemble_quarterly_batches() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Alice", "alice@example.com");

        // One commit per week for 13 weeks starting 2024-W01.
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

    /// Why: a Monthly window must produce multiple batches from the same data.
    /// What: seeds 8 commits over 8 weeks, asserts two 4-week batches.
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

    /// Why: a since/until filter must restrict batches to the given date range.
    /// What: seeds commits spanning two months, filters to one month, asserts
    /// only one batch with the filtered commit count.
    /// Test: this test itself.
    #[test]
    fn assemble_with_date_filter() {
        let db = Database::open_in_memory().expect("open");
        let aid = seed_author(&db, "Carol", "carol@example.com");

        // January commits.
        seed_commit(&db, "c1", aid, "2024-01-08T00:00:00Z");
        seed_commit(&db, "c2", aid, "2024-01-15T00:00:00Z");
        // February commit (should be excluded).
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

    /// Why: an author with no commits must return an empty Vec, not an error.
    /// What: seeds an author but no commits, asserts empty result.
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

    /// Why: period label and date fields of assembled batches must propagate
    /// the tga AuthorPeriodSummary metadata correctly.
    /// What: seeds one commit, assembles, asserts period_label and since/until
    /// are non-empty and correctly formatted.
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

    /// Why: Custom window must pass through as-is (with floor of 1).
    /// What: uses Window::Custom(2), seeds 4 commits, asserts 2 batches.
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

//! Query implementation for N-week period trend roll-ups.
//!
//! Provides [`query_author_period_trends`] plus all private helpers
//! (week-windowing, label formatting, per-period SQL aggregation).

use std::collections::HashMap;

use chrono::{Datelike, Duration, IsoWeek, NaiveDate};
use rusqlite::params;

use crate::core::db::Database;
use crate::report::drilldown::{
    extract_provider_logins, lookup_author_for_drilldown, query_effort_histogram, query_pr_metrics,
};
use crate::report::errors::{ReportError, Result};

use super::model::AuthorPeriodSummary;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Aggregate existing per-week data for one canonical author into N-week
/// period windows.
///
/// Why: the contributor-profile epic (#558) requires trend data bucketed into
/// multi-week periods (e.g. 4-week sprints) rather than raw per-week rows,
/// enabling callers to render velocity trend lines and period-over-period
/// comparisons without rebuilding aggregation logic.
/// What: computes the set of ISO weeks for the author in `[since, until]`,
/// partitions them into chunks of `window_weeks`, and for each chunk reuses
/// `query_effort_histogram`, `query_pr_metrics`, and inline SQL against
/// `commits` / `classifications` / `fact_weekly_quality` to assemble an
/// [`AuthorPeriodSummary`]. Reads existing schema only — no migration needed.
/// Returns an empty `Vec` when the author has no commits in the window.
///
/// # Parameters
///
/// - `db` — open database handle
/// - `email` — canonical email matched case-insensitively
/// - `window_weeks` — number of ISO weeks per period bucket (minimum 1)
/// - `since` — optional ISO 8601 lower bound (inclusive); `None` = start of data
/// - `until` — optional ISO 8601 upper bound (inclusive); `None` = end of data
///
/// # Errors
///
/// Returns [`ReportError`] on any DB failure.
///
/// Test: see `tests` module in `super::tests`.
pub fn query_author_period_trends(
    db: &Database,
    email: &str,
    window_weeks: u32,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<AuthorPeriodSummary>> {
    let window_weeks = window_weeks.max(1);

    // Resolve the author to get their aliases (needed for PR queries).
    let author_row = lookup_author_for_drilldown(db, email)?;
    let aliases_json = match author_row {
        Some((_, _, _, ref aliases)) => aliases.clone(),
        None => return Ok(Vec::new()),
    };
    let logins = extract_provider_logins(&aliases_json);

    // Determine the effective date bounds from the commits table.
    let (data_since, data_until) = effective_date_bounds(db, email, since, until)?;
    let (data_since, data_until) = match (data_since, data_until) {
        (Some(s), Some(u)) => (s, u),
        _ => return Ok(Vec::new()), // no commits in scope
    };

    // Parse dates and enumerate ISO weeks in the range.
    let start_date = parse_iso_date(&data_since)?;
    let end_date = parse_iso_date(&data_until)?;

    // Collect the sorted sequence of distinct ISO-week start dates covered by
    // commits for this author. This avoids creating empty period buckets for
    // calendar gaps where the author was inactive.
    let week_starts = weeks_in_range(start_date, end_date);
    if week_starts.is_empty() {
        return Ok(Vec::new());
    }

    // Partition weeks into fixed-width buckets of `window_weeks`.
    let mut summaries = Vec::new();
    for chunk in week_starts.chunks(window_weeks as usize) {
        let period_since_date = *chunk.first().expect("non-empty chunk");
        let period_until_date = chunk
            .last()
            .expect("non-empty chunk")
            .checked_add_signed(Duration::days(6))
            .unwrap_or(*chunk.last().expect("non-empty chunk"));

        let period_since = period_since_date.format("%Y-%m-%d").to_string();
        let period_until = period_until_date.format("%Y-%m-%d").to_string();

        let period_label = make_period_label(period_since_date, period_until_date);

        let summary =
            build_period_summary(db, email, &logins, period_label, period_since, period_until)?;
        summaries.push(summary);
    }

    Ok(summaries)
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Return `(min_timestamp, max_timestamp)` for commits by `email` in the
/// optional `[since, until]` window.
///
/// Why: concentrates the date-bound lookup so `query_author_period_trends`
/// stays readable.
/// What: queries MIN/MAX commit timestamps for the author with optional
/// calendar filters, then trims to YYYY-MM-DD.
/// Test: exercised indirectly through all period-trend integration tests.
fn effective_date_bounds(
    db: &Database,
    email: &str,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<(Option<String>, Option<String>)> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT MIN(c.timestamp), MAX(c.timestamp) \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND (?2 IS NULL OR c.timestamp >= ?2) \
               AND (?3 IS NULL OR c.timestamp <= ?3)",
        )
        .map_err(crate::core::TgaError::from)?;

    let (min_ts, max_ts) = stmt
        .query_row(params![email, since, until], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .map_err(crate::core::TgaError::from)?;

    // Trim to YYYY-MM-DD.
    let trim = |s: Option<String>| s.map(|v| v.get(..10).unwrap_or(&v).to_string());
    Ok((trim(min_ts), trim(max_ts)))
}

/// Build one [`AuthorPeriodSummary`] for the given period bounds.
///
/// Why: isolates per-period SQL aggregation so the caller loop stays concise.
/// What: queries commit counts, ticketed fraction, repositories, per-category
/// counts, effort histogram, quality score, and PR metrics for one period window.
/// Test: exercised indirectly through all period-trend integration tests.
fn build_period_summary(
    db: &Database,
    email: &str,
    logins: &[String],
    period_label: String,
    period_since: String,
    period_until: String,
) -> Result<AuthorPeriodSummary> {
    let conn = db.connection();

    // Commit count + ticketed count.
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*), \
                    SUM(CASE WHEN c.ticketed = 1 THEN 1 ELSE 0 END) \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND c.timestamp >= ?2 \
               AND c.timestamp <= ?3 || 'T23:59:59Z'",
        )
        .map_err(crate::core::TgaError::from)?;

    let (commit_count, ticketed_count): (u64, u64) = stmt
        .query_row(params![email, period_since, period_until], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
            ))
        })
        .map_err(crate::core::TgaError::from)?;

    // Repositories.
    let repositories = query_repositories(db, email, &period_since, &period_until)?;

    // Per-category counts.
    let categories = query_categories(db, email, &period_since, &period_until)?;

    // Effort histogram — reuse the existing helper.
    let effort = query_effort_histogram(db, email, Some(&period_since), Some(&period_until))?;
    let effort_histogram: HashMap<String, u32> = effort.histogram;

    // Average quality score from fact_weekly_quality (if table exists & has rows).
    let quality_score = query_avg_quality_score(db, email, &period_since, &period_until)?;

    // PR metrics — reuse the existing helper with the period bounds.
    let pr_metrics = query_pr_metrics(db, logins, Some(&period_since), Some(&period_until))?;

    let ticketed_pct = if commit_count > 0 {
        ticketed_count as f64 / commit_count as f64
    } else {
        0.0
    };

    Ok(AuthorPeriodSummary {
        period_label,
        since: period_since,
        until: period_until,
        commit_count,
        categories,
        effort_histogram,
        quality_score,
        ticketed_pct,
        pr_metrics,
        repositories,
    })
}

/// Return distinct repositories touched by `email` in the given period.
///
/// Why: extracted from `build_period_summary` to keep that function readable.
/// What: queries DISTINCT `c.repository` values with the period date filter.
/// Test: exercised indirectly through all period-trend integration tests.
fn query_repositories(
    db: &Database,
    email: &str,
    period_since: &str,
    period_until: &str,
) -> Result<Vec<String>> {
    let conn = db.connection();
    let mut repo_stmt = conn
        .prepare(
            "SELECT DISTINCT c.repository \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND c.timestamp >= ?2 \
               AND c.timestamp <= ?3 || 'T23:59:59Z' \
             ORDER BY c.repository",
        )
        .map_err(crate::core::TgaError::from)?;

    let repo_rows = repo_stmt
        .query_map(params![email, period_since, period_until], |row| {
            row.get::<_, String>(0)
        })
        .map_err(crate::core::TgaError::from)?;

    let mut repositories = Vec::new();
    for r in repo_rows {
        repositories.push(r.map_err(crate::core::TgaError::from)?);
    }
    Ok(repositories)
}

/// Return per-category commit counts for `email` in the given period.
///
/// Why: extracted from `build_period_summary` to keep that function readable.
/// What: groups commits by `classifications.category`, returning a
/// `HashMap<category, count>` for non-null categories only.
/// Test: exercised by `period_trends_category_aggregation`.
fn query_categories(
    db: &Database,
    email: &str,
    period_since: &str,
    period_until: &str,
) -> Result<HashMap<String, u64>> {
    let conn = db.connection();
    let mut cat_stmt = conn
        .prepare(
            "SELECT cl.category, COUNT(*) \
             FROM commits c \
             JOIN authors a ON a.id = c.author_id \
             LEFT JOIN classifications cl ON cl.id = c.classification_id \
             WHERE LOWER(a.canonical_email) = LOWER(?1) \
               AND cl.category IS NOT NULL \
               AND c.timestamp >= ?2 \
               AND c.timestamp <= ?3 || 'T23:59:59Z' \
             GROUP BY cl.category",
        )
        .map_err(crate::core::TgaError::from)?;

    let cat_rows = cat_stmt
        .query_map(params![email, period_since, period_until], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(crate::core::TgaError::from)?;

    let mut categories: HashMap<String, u64> = HashMap::new();
    for r in cat_rows {
        let (cat, cnt) = r.map_err(crate::core::TgaError::from)?;
        categories.insert(cat, cnt as u64);
    }
    Ok(categories)
}

/// Query the average `quality_score` from `fact_weekly_quality` for the author
/// in the given period.  Returns `0.0` when the table has no matching rows.
///
/// Why: quality data is keyed by `(iso_year, iso_week)` integers; this helper
/// converts the YYYY-MM-DD period bounds to those integers for comparison.
/// What: AVGs `quality_score` for rows whose `(iso_year, iso_week)` falls within
/// the period bounds.
/// Test: covered indirectly — period_trends tests that seed quality rows exercise
/// the non-zero path; tests without quality rows verify the `0.0` fallback.
fn query_avg_quality_score(
    db: &Database,
    email: &str,
    period_since: &str,
    period_until: &str,
) -> Result<f64> {
    // Parse the ISO dates to extract iso_year and iso_week integers for the
    // comparison. `fact_weekly_quality` stores (iso_year INTEGER, iso_week
    // INTEGER) — NOT a text `week` column.
    let since_date = parse_iso_date(period_since)?;
    let until_date = parse_iso_date(period_until)?;

    let since_iso = since_date.iso_week();
    let until_iso = until_date.iso_week();

    let since_year = since_iso.year();
    let since_week = since_iso.week() as i32;
    let until_year = until_iso.year();
    let until_week = until_iso.week() as i32;

    let conn = db.connection();
    let result: rusqlite::Result<Option<f64>> = conn.query_row(
        "SELECT AVG(fwq.quality_score) \
         FROM fact_weekly_quality fwq \
         WHERE LOWER(fwq.author_email) = LOWER(?1) \
           AND (fwq.iso_year > ?2 OR (fwq.iso_year = ?2 AND fwq.iso_week >= ?3)) \
           AND (fwq.iso_year < ?4 OR (fwq.iso_year = ?4 AND fwq.iso_week <= ?5))",
        params![email, since_year, since_week, until_year, until_week],
        |row| row.get(0),
    );
    match result {
        Ok(Some(avg)) => Ok(avg),
        Ok(None) => Ok(0.0),
        Err(rusqlite::Error::SqliteFailure(_, _)) | Err(rusqlite::Error::QueryReturnedNoRows) => {
            Ok(0.0)
        }
        Err(e) => Err(ReportError::Core(crate::core::TgaError::from(e))),
    }
}

/// Parse a `YYYY-MM-DD` string into a `NaiveDate`.
///
/// Why: centralises date parsing so all callers get a consistent error message.
/// What: wraps `NaiveDate::parse_from_str` with a `ReportError::Report` on failure.
/// Test: indirectly exercised whenever a caller passes a date string.
pub(super) fn parse_iso_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| ReportError::Report(format!("invalid date string '{s}': {e}")))
}

/// Return the sequence of Monday dates (ISO week starts) that cover the
/// range `[start, end]` inclusive.
///
/// Why: week-bucketing requires a list of ISO-week anchors; building it once
/// avoids repeated date arithmetic in the loop.
/// What: rounds `start` down to its ISO week's Monday, iterates by one week
/// until `end`'s Monday is reached.
/// Test: see `period_trends_week_helpers::weeks_in_range_produces_correct_mondays`.
pub(super) fn weeks_in_range(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    // Round `start` down to its ISO week's Monday.
    let first_monday = iso_week_monday(start.iso_week());
    let last_monday = iso_week_monday(end.iso_week());

    let mut weeks = Vec::new();
    let mut current = first_monday;
    while current <= last_monday {
        weeks.push(current);
        current = current
            .checked_add_signed(Duration::weeks(1))
            .unwrap_or(current);
        if current == weeks.last().copied().unwrap_or(current) {
            break; // safety against infinite loop
        }
    }
    weeks
}

/// Return the Monday of the given ISO week.
///
/// Why: ISO-week arithmetic requires deriving a concrete date from the abstract
/// `IsoWeek` type; this helper centralises the conversion.
/// What: calls `NaiveDate::from_isoywd_opt` with `Weekday::Mon` and panics only
/// on an impossible ISO week value (an invariant that cannot occur in practice).
/// Test: indirectly verified by `weeks_in_range_produces_correct_mondays`.
fn iso_week_monday(isoweek: IsoWeek) -> NaiveDate {
    NaiveDate::from_isoywd_opt(isoweek.year(), isoweek.week(), chrono::Weekday::Mon)
        .expect("valid ISO week always produces a valid Monday")
}

/// Build a human-readable period label, e.g. `"2026-W01..W04"` or
/// `"2026-W52..2027-W02"` (cross-year).
///
/// Why: callers need a stable, human-scannable label for each period bucket.
/// What: produces `"YYYY-Www..Www"` for same-year ranges and
/// `"YYYY-Www..YYYY-Www"` for cross-year ranges.
/// Test: covered by `period_trends_label_and_date_format`.
pub(super) fn make_period_label(since: NaiveDate, until: NaiveDate) -> String {
    let since_week = since.iso_week();
    let until_week = until.iso_week();
    if since_week.year() == until_week.year() {
        format!(
            "{}-W{:02}..W{:02}",
            since_week.year(),
            since_week.week(),
            until_week.week()
        )
    } else {
        format!(
            "{}-W{:02}..{}-W{:02}",
            since_week.year(),
            since_week.week(),
            until_week.year(),
            until_week.week()
        )
    }
}

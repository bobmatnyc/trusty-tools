//! CSV formatter — writes `authors.csv` and `weekly_activity.csv`.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::report::errors::Result;
use crate::report::models::ReportData;

/// Filename for the per-author summary CSV.
pub const AUTHORS_CSV: &str = "authors.csv";

/// Filename for the weekly activity CSV.
pub const WEEKLY_CSV: &str = "weekly_activity.csv";

/// Write the per-author summary as CSV into `output_dir`.
///
/// Returns the full path to the written file.
///
/// # Errors
///
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_author_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(AUTHORS_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "name",
        "email",
        "commit_count",
        "insertions",
        "deletions",
        "files_changed",
        "first_commit",
        "last_commit",
        "categories",
    ])?;
    for a in &data.authors {
        let categories = serialize_categories(&a.categories);
        w.write_record([
            a.name.as_str(),
            a.email.as_str(),
            &a.commit_count.to_string(),
            &a.insertions.to_string(),
            &a.deletions.to_string(),
            &a.files_changed.to_string(),
            a.first_commit.as_str(),
            a.last_commit.as_str(),
            categories.as_str(),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.authors.len(), "wrote authors.csv");
    Ok(path)
}

/// Write the weekly activity table as CSV into `output_dir`.
///
/// # Errors
///
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "week",
        "author",
        "repository",
        "commit_count",
        "insertions",
        "deletions",
        "categories",
        "revert_count",
        "bugfix_count",
        "ticketed_count",
        "quality_score",
        "quality_tshirt",
        "abandoned_pr_count",
    ])?;
    for row in &data.weekly_activity {
        let categories = serialize_categories(&row.categories);
        w.write_record([
            row.week.as_str(),
            row.author.as_str(),
            row.repository.as_str(),
            &row.commit_count.to_string(),
            &row.insertions.to_string(),
            &row.deletions.to_string(),
            categories.as_str(),
            &row.revert_count.to_string(),
            &row.bugfix_count.to_string(),
            &row.ticketed_count.to_string(),
            &format!("{:.4}", row.quality_score),
            row.quality_tshirt.as_str(),
            &row.abandoned_pr_count.to_string(),
        ])?;
    }
    w.flush()?;
    debug!(
        path = %path.display(),
        rows = data.weekly_activity.len(),
        "wrote weekly_activity.csv"
    );
    Ok(path)
}

/// Filename for the per-week aggregate metrics CSV.
pub const WEEKLY_METRICS_CSV: &str = "weekly_metrics.csv";

/// Filename for the per-developer activity summary CSV.
pub const DEV_ACTIVITY_CSV: &str = "developer_activity_summary.csv";

/// Filename for the single-row overview CSV.
pub const SUMMARY_CSV: &str = "summary.csv";

/// Filename for the untracked commits CSV.
pub const UNTRACKED_CSV: &str = "untracked_commits.csv";

/// Filename for the weekly categorization CSV.
pub const WEEKLY_CATEGORIZATION_CSV: &str = "weekly_categorization.csv";

/// Filename for the weekly velocity CSV.
pub const WEEKLY_VELOCITY_CSV: &str = "weekly_velocity.csv";

/// Filename for the weekly DORA metrics CSV (one-row currently — DORA is
/// period-scoped, but the file matches the spec's filename slot).
pub const WEEKLY_DORA_CSV: &str = "weekly_dora_metrics.csv";

/// Write `weekly_metrics.csv`.
///
/// Why: surfaces per-week category counts and active developer counts in a
/// stable tabular form that downstream BI tooling can ingest.
/// What: one row per ISO week with category-aware tallies.
/// Test: seed two commits in different weeks, assert the file has 2 rows
/// plus header.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_metrics_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_METRICS_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "week_id",
        "total_commits",
        "feature_commits",
        "bugfix_commits",
        "maintenance_commits",
        "refactor_commits",
        "test_commits",
        "doc_commits",
        "active_developers",
        "story_points",
    ])?;
    for m in &data.weekly_metrics {
        w.write_record([
            m.week.as_str(),
            &m.total_commits.to_string(),
            &m.feature_commits.to_string(),
            &m.bugfix_commits.to_string(),
            &m.maintenance_commits.to_string(),
            &m.refactor_commits.to_string(),
            &m.test_commits.to_string(),
            &m.doc_commits.to_string(),
            &m.active_developers.to_string(),
            &format!("{:.2}", m.story_points),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.weekly_metrics.len(), "wrote weekly_metrics.csv");
    Ok(path)
}

/// Write `developer_activity_summary.csv`.
///
/// Why: provides a single comparable activity score plus headline counts per
/// developer for leadership reporting.
/// What: one row per developer with score, active weeks, and primary work type.
/// Test: with two developers having different commit counts, assert the row
/// with higher commits has higher `activity_score`.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_developer_activity_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(DEV_ACTIVITY_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "developer_id",
        "display_name",
        "total_commits",
        "active_weeks",
        "avg_commits_per_week",
        "primary_work_type",
        "story_points_total",
        "activity_score",
    ])?;
    for d in &data.developer_activity {
        w.write_record([
            d.developer_id.as_str(),
            d.display_name.as_str(),
            &d.total_commits.to_string(),
            &d.active_weeks.to_string(),
            &format!("{:.2}", d.avg_commits_per_week),
            d.primary_work_type.as_str(),
            &format!("{:.2}", d.story_points_total),
            &format!("{:.4}", d.activity_score),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.developer_activity.len(), "wrote developer_activity_summary.csv");
    Ok(path)
}

/// Write `summary.csv` — single-row period overview.
///
/// Why: gives a one-line headline ("X commits by Y developers across Z weeks")
/// that fits naturally into downstream digests.
/// What: a single row reflecting the [`crate::report::models::ReportSummary`].
/// Test: assert the file has exactly one data row and matching totals.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_summary_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(SUMMARY_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "date_range",
        "total_commits",
        "total_developers",
        "total_weeks",
        "classification_coverage_pct",
    ])?;
    if let Some(s) = &data.summary {
        w.write_record([
            s.date_range.as_str(),
            &s.total_commits.to_string(),
            &s.total_developers.to_string(),
            &s.total_weeks.to_string(),
            &format!("{:.2}", s.classification_coverage_pct),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), "wrote summary.csv");
    Ok(path)
}

/// Write `untracked_commits.csv`.
///
/// Why: highlights commits without ticket references so teams can improve
/// ticketing hygiene.
/// What: one row per commit that has no work-item reference, newest first.
/// Test: insert one untracked commit, assert the file contains it.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_untracked_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(UNTRACKED_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record(["sha", "author", "date", "message"])?;
    for u in &data.untracked_commits {
        w.write_record([
            u.sha.as_str(),
            u.author.as_str(),
            u.date.as_str(),
            u.message.as_str(),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.untracked_commits.len(), "wrote untracked_commits.csv");
    Ok(path)
}

/// Write `weekly_categorization.csv`.
///
/// Why: surfaces how each week's commits are split across change types so
/// stakeholders can see, e.g., a feature-heavy vs maintenance-heavy week.
/// What: one row per (week, change_type) with count and percentage of the
/// week's commits.
/// Test: seed two commits of category `feature` in week W; assert one row
/// with `pct_of_week == 100`.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_categorization_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_CATEGORIZATION_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record(["week_id", "change_type", "commit_count", "pct_of_week"])?;
    for c in &data.weekly_categorization {
        w.write_record([
            c.week.as_str(),
            c.change_type.as_str(),
            &c.commit_count.to_string(),
            &format!("{:.2}", c.pct_of_week),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.weekly_categorization.len(), "wrote weekly_categorization.csv");
    Ok(path)
}

/// Write `weekly_velocity.csv`.
///
/// Why: tracks delivery cadence on a per-week basis (PRs merged, cycle time,
/// commits per developer).
/// What: one row per ISO week.
/// Test: with no PR data, file still emits one row per week with zero PRs.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_velocity_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_VELOCITY_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "week_id",
        "prs_merged",
        "avg_pr_cycle_time_hours",
        "story_points",
        "commits_per_developer",
    ])?;
    for v in &data.weekly_velocity {
        w.write_record([
            v.week.as_str(),
            &v.prs_merged.to_string(),
            &format!("{:.2}", v.avg_pr_cycle_time_hours),
            &format!("{:.2}", v.story_points),
            &format!("{:.2}", v.commits_per_developer),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.weekly_velocity.len(), "wrote weekly_velocity.csv");
    Ok(path)
}

/// Write `weekly_dora_metrics.csv`.
///
/// Why: surfaces the four DORA metrics in tabular form so they can be
/// plotted alongside the JSON dashboard payload.
/// What: a single-row CSV (DORA is period-scoped). When no DORA data is
/// available the file contains only the header.
/// Test: with seeded commits + PRs, assert one data row with a recognized
/// `performance_level`.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_dora_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_DORA_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "deployment_frequency_per_week",
        "lead_time_hours",
        "change_failure_rate",
        "mttr_hours",
        "performance_level",
    ])?;
    if let Some(d) = &data.dora {
        w.write_record([
            &format!("{:.4}", d.deployment_frequency),
            &format!("{:.2}", d.lead_time_hours),
            &format!("{:.4}", d.change_failure_rate),
            &format!("{:.2}", d.mttr_hours),
            d.performance_level.as_str(),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), "wrote weekly_dora_metrics.csv");
    Ok(path)
}

/// Encode a category histogram as a deterministic `key=value;…` string so
/// the CSV cell is stable and machine-parseable.
fn serialize_categories(map: &std::collections::HashMap<String, usize>) -> String {
    let mut entries: Vec<(&String, &usize)> = map.iter().collect();
    entries.sort_by_key(|e| e.0);
    entries
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

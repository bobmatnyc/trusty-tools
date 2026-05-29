//! Report-specific aggregated data structures.
//!
//! These structs are populated by [`crate::report::aggregator::Aggregator`] from
//! database queries and then consumed by the formatters in
//! [`crate::report::formatters`]. They are `serde`-friendly so that the JSON
//! formatter can emit [`ReportData`] directly.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Aggregated per-author commit summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorSummary {
    /// Canonical author display name.
    pub name: String,
    /// Canonical author email.
    pub email: String,
    /// Number of commits attributed to this author.
    pub commit_count: usize,
    /// Total insertions across all of this author's commits.
    pub insertions: i64,
    /// Total deletions across all of this author's commits.
    pub deletions: i64,
    /// Total files changed across all of this author's commits.
    pub files_changed: i64,
    /// Per-category commit counts for this author.
    pub categories: HashMap<String, usize>,
    /// ISO 8601 timestamp of the author's earliest observed commit.
    pub first_commit: String,
    /// ISO 8601 timestamp of the author's latest observed commit.
    pub last_commit: String,
}

/// Aggregated per-repository summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositorySummary {
    /// Repository name (matches `commits.repository`).
    pub name: String,
    /// Total commits in this repository.
    pub commit_count: usize,
    /// Distinct author count in this repository.
    pub author_count: usize,
    /// Total insertions across all commits in this repository.
    pub insertions: i64,
    /// Total deletions across all commits in this repository.
    pub deletions: i64,
    /// Top categories by commit count, sorted descending.
    pub top_categories: Vec<(String, usize)>,
}

/// Per-week-per-author-per-repository activity row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyActivity {
    /// ISO week label, e.g. `"2024-W03"`.
    pub week: String,
    /// Author name.
    pub author: String,
    /// Repository name.
    pub repository: String,
    /// Number of commits in this week/author/repo bucket.
    pub commit_count: usize,
    /// Insertions in this bucket.
    pub insertions: i64,
    /// Deletions in this bucket.
    pub deletions: i64,
    /// Per-category counts within this bucket.
    pub categories: HashMap<String, usize>,
    /// Commits in this bucket detected as reverts (issue #377). Negative
    /// quality signal — feeds [`Self::quality_score`].
    #[serde(default)]
    pub revert_count: usize,
    /// Commits in this bucket classified as `bugfix` (issue #377). Exposed so
    /// the bugfix rate behind the quality score is auditable downstream.
    #[serde(default)]
    pub bugfix_count: usize,
    /// Commits in this bucket carrying a ticket reference (issue #377).
    /// Positive quality signal.
    #[serde(default)]
    pub ticketed_count: usize,
    /// Per-engineer-per-week quality score in `[0.0, 1.0]` (higher is
    /// better). See [`crate::core::quality`] for the formula orientation.
    #[serde(default)]
    pub quality_score: f64,
    /// Quality T-shirt bucket as the string `"1".."5"` (5 = best), parallel
    /// to `effort_tshirt` so consumers can join it the same way.
    #[serde(default)]
    pub quality_tshirt: String,
    /// Closed-but-unmerged ("abandoned") PRs attributed to this engineer in
    /// this week (issue #377). Best-effort attribution by author login; see
    /// the aggregator note on its limitations.
    #[serde(default)]
    pub abandoned_pr_count: usize,
}

/// Per-week aggregated metrics across all developers and repositories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyMetrics {
    /// ISO week label (e.g. `"2024-W03"`).
    pub week: String,
    /// Total commits in the week.
    pub total_commits: usize,
    /// Feature commits.
    pub feature_commits: usize,
    /// Bugfix commits.
    pub bugfix_commits: usize,
    /// Maintenance commits.
    pub maintenance_commits: usize,
    /// Refactor commits.
    pub refactor_commits: usize,
    /// Test commits.
    pub test_commits: usize,
    /// Documentation commits.
    pub doc_commits: usize,
    /// Distinct active developers.
    pub active_developers: usize,
    /// Story points (placeholder — sourced from work items when available).
    pub story_points: f64,
}

/// Per-developer activity summary across the full reporting period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeveloperActivitySummary {
    /// Stable developer identifier (canonical email).
    pub developer_id: String,
    /// Canonical display name.
    pub display_name: String,
    /// Total commits attributed to this developer.
    pub total_commits: usize,
    /// Distinct ISO weeks with at least one commit.
    pub active_weeks: usize,
    /// `total_commits / active_weeks` (zero when no active weeks).
    pub avg_commits_per_week: f64,
    /// Modal `change_type` for this developer (e.g. `"feature"`).
    pub primary_work_type: String,
    /// Story points contributed (currently zero — placeholder).
    pub story_points_total: f64,
    /// Composite activity score, see [`ActivityWeights`].
    pub activity_score: f64,
}

/// Single-row overview metrics for the whole report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    /// Period range as `"<start> .. <end>"` (ISO 8601, UTC).
    pub date_range: String,
    /// Total commit count.
    pub total_commits: usize,
    /// Total distinct developer count.
    pub total_developers: usize,
    /// Total distinct ISO weeks observed.
    pub total_weeks: usize,
    /// Classification coverage percent (commits with a non-null category).
    pub classification_coverage_pct: f64,
}

/// A commit that has no associated work-item / ticket reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackedCommit {
    /// Commit SHA.
    pub sha: String,
    /// Author display name.
    pub author: String,
    /// Author timestamp (ISO 8601).
    pub date: String,
    /// First line of the commit message.
    pub message: String,
}

/// Per-week per-change-type count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyCategorization {
    /// ISO week label.
    pub week: String,
    /// Change type / category label.
    pub change_type: String,
    /// Number of commits.
    pub commit_count: usize,
    /// Percentage of the week's total commits this category represents.
    pub pct_of_week: f64,
}

/// Per-week velocity metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyVelocity {
    /// ISO week label.
    pub week: String,
    /// PRs merged in the week.
    pub prs_merged: usize,
    /// Average PR cycle time (created → merged) in hours.
    pub avg_pr_cycle_time_hours: f64,
    /// Story points delivered (placeholder, currently zero).
    pub story_points: f64,
    /// Average commits per active developer in the week.
    pub commits_per_developer: f64,
}

/// DORA "Accelerate" metrics over the full reporting period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoraMetrics {
    /// Deployment frequency in deploys per week.
    pub deployment_frequency: f64,
    /// Average lead time (PR open → merge) in hours. Zero if no PR data.
    pub lead_time_hours: f64,
    /// Change failure rate: bugfix commits / total commits.
    pub change_failure_rate: f64,
    /// MTTR approximation: average hours between a bug-introducing commit and
    /// its bugfix commit. Zero if no commit-pairs found.
    pub mttr_hours: f64,
    /// Aggregate performance band: `"elite" | "high" | "medium" | "low"`.
    pub performance_level: String,
}

/// Period-level velocity summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocitySummary {
    /// Average PR cycle time in hours (outlier-filtered).
    pub pr_cycle_time_avg_hours: f64,
    /// Median PR cycle time in hours (outlier-filtered).
    pub pr_cycle_time_median_hours: f64,
    /// PRs merged per ISO week (mean across weeks observed).
    pub pr_throughput_per_week: f64,
    /// Revision rate placeholder — currently zero because review-round data
    /// is not yet collected; surfaced so the schema is stable.
    pub revision_rate: f64,
    /// Total PRs considered after outlier filtering.
    pub pr_count: usize,
}

/// Quality / hygiene metrics derived from commit messages and classifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualitySummary {
    /// Composite quality score in `[0, 1]`.
    pub quality_score: f64,
    /// Total revert commits detected.
    pub revert_count: usize,
    /// `revert_count / total_commits` as a fraction in `[0, 1]`.
    pub revert_pct: f64,
    /// Bugfix commit percentage in `[0, 1]`.
    pub bugfix_pct: f64,
    /// Defect rate: bugfix / non-bugfix-feature commits, in `[0, 1]`.
    pub defect_rate: f64,
}

/// Configurable weights for composite developer activity score.
///
/// Defaults match the values in `docs/trusty-git-analytics/requirements/reporting.md`. The five
/// components are normalized via min-max scaling across the reporting period
/// and then linearly combined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityWeights {
    /// Weight for raw commit count.
    pub commits: f64,
    /// Weight for merged PRs.
    pub prs: f64,
    /// Weight for code impact (lines changed).
    pub code_impact: f64,
    /// Weight for code complexity proxy (files changed per commit).
    pub complexity: f64,
    /// Weight for ticketing hygiene (ticketed commits / total).
    pub ticketing: f64,
}

impl Default for ActivityWeights {
    fn default() -> Self {
        Self {
            commits: 0.22,
            prs: 0.26,
            code_impact: 0.26,
            complexity: 0.11,
            ticketing: 0.15,
        }
    }
}

/// Full report payload passed to every formatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportData {
    /// ISO 8601 timestamp at which the report was generated.
    pub generated_at: String,
    /// Earliest observed commit timestamp (ISO 8601), or `None` if empty.
    pub period_start: Option<String>,
    /// Latest observed commit timestamp (ISO 8601), or `None` if empty.
    pub period_end: Option<String>,
    /// Per-author summaries.
    pub authors: Vec<AuthorSummary>,
    /// Per-repository summaries.
    pub repositories: Vec<RepositorySummary>,
    /// Weekly activity rows.
    pub weekly_activity: Vec<WeeklyActivity>,
    /// Total commit count across the dataset.
    pub total_commits: usize,
    /// Total distinct author count across the dataset.
    pub total_authors: usize,
    /// Cross-cutting category → count tally.
    pub category_breakdown: HashMap<String, usize>,
    /// Weekly cross-developer aggregate metrics.
    pub weekly_metrics: Vec<WeeklyMetrics>,
    /// Per-developer activity rollup.
    pub developer_activity: Vec<DeveloperActivitySummary>,
    /// Single-row period summary.
    pub summary: Option<ReportSummary>,
    /// Commits with no work-item reference.
    pub untracked_commits: Vec<UntrackedCommit>,
    /// Per-week per-category counts.
    pub weekly_categorization: Vec<WeeklyCategorization>,
    /// Per-week velocity metrics.
    pub weekly_velocity: Vec<WeeklyVelocity>,
    /// DORA period-level metrics.
    pub dora: Option<DoraMetrics>,
    /// Velocity period-level summary.
    pub velocity: Option<VelocitySummary>,
    /// Quality period-level summary.
    pub quality: Option<QualitySummary>,
    /// Total boilerplate commits detected.
    pub boilerplate_count: usize,
    /// Total revert commits detected.
    pub revert_count: usize,
    /// Number of repositories analyzed for this report (i.e. distinct
    /// `commits.repository` values observed). Surfaced so downstream
    /// consumers can detect undercounting when the configured repo roster
    /// is narrower than the actual portfolio (see issue #67).
    #[serde(default)]
    pub repository_coverage: usize,
    /// Number of commits whose author identity could not be resolved to a
    /// canonical team member (see issue #68). These commits still appear in
    /// the commit totals but are tracked separately so developer counts are
    /// not silently inflated by phantom identities.
    #[serde(default)]
    pub unresolved_author_commits: usize,
    /// Number of distinct author identities that did not resolve to a
    /// configured canonical team member (see issue #68).
    #[serde(default)]
    pub unresolved_authors: usize,
}

impl ReportData {
    /// Construct an empty `ReportData` with the given generation timestamp.
    pub fn empty(generated_at: String) -> Self {
        Self {
            generated_at,
            period_start: None,
            period_end: None,
            authors: Vec::new(),
            repositories: Vec::new(),
            weekly_activity: Vec::new(),
            total_commits: 0,
            total_authors: 0,
            category_breakdown: HashMap::new(),
            weekly_metrics: Vec::new(),
            developer_activity: Vec::new(),
            summary: None,
            untracked_commits: Vec::new(),
            weekly_categorization: Vec::new(),
            weekly_velocity: Vec::new(),
            dora: None,
            velocity: None,
            quality: None,
            boilerplate_count: 0,
            revert_count: 0,
            repository_coverage: 0,
            unresolved_author_commits: 0,
            unresolved_authors: 0,
        }
    }
}

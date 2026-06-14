//! Data model types for per-engineer drill-down reports.
//!
//! Why: separates the serialisable report model from the DB query layer and
//! the formatters so each can evolve independently and be tested in isolation.
//! What: defines [`AuthorDrilldownData`] and its component sub-structs.
//! Test: `tests::format_markdown_contains_headers` and `tests::format_json_parses`
//! construct these types directly and assert on the formatted output.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Fully assembled per-engineer drill-down report.
///
/// Why: formatters (Markdown, JSON) need a single input struct so they can
/// be called independently of the DB; decoupling the data model from both
/// the query layer and the formatters keeps each layer testable in isolation.
/// What: aggregates all drill-down sections — commit summary, effort histogram,
/// PR metrics, category breakdown — plus header metadata for the report.
/// Test: see `tests::format_markdown_contains_headers` and
/// `tests::format_json_parses`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorDrilldownData {
    /// ISO 8601 UTC timestamp at which the report was generated.
    pub generated_at: String,
    /// Canonical email (as stored in `authors.canonical_email`).
    pub email: String,
    /// Canonical display name.
    pub name: String,
    /// Report window.
    pub period: ReportPeriod,
    /// Commit-level aggregate.
    pub commits: CommitSection,
    /// Effort histogram section.
    pub effort: EffortSection,
    /// Pull-request metrics section.
    pub pull_requests: PrSection,
    /// Per-category commit counts.
    pub categories: HashMap<String, usize>,
}

/// Date window for the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPeriod {
    /// Lower bound (ISO 8601 date or timestamp), `None` = all history.
    pub since: Option<String>,
    /// Upper bound (ISO 8601 date or timestamp), `None` = present.
    pub until: Option<String>,
}

/// Commit-level aggregate section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSection {
    /// Total commits in the window.
    pub total: u64,
    /// Commits with `ticketed = 1`.
    pub ticketed: u64,
    /// `ticketed / total`, in `[0.0, 1.0]`. `None` when total == 0.
    pub ticket_coverage: Option<f64>,
    /// Distinct repositories touched.
    pub repositories: Vec<String>,
    /// Earliest commit timestamp (ISO 8601). `None` when total == 0.
    pub first_commit: Option<String>,
    /// Latest commit timestamp (ISO 8601). `None` when total == 0.
    pub last_commit: Option<String>,
    /// Total insertions.
    pub insertions: i64,
    /// Total deletions.
    pub deletions: i64,
}

/// Effort histogram section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffortSection {
    /// Commits that have a row in `fact_commit_effort`.
    pub scored_commits: u64,
    /// Total commits (scored + unscored).
    pub total_commits: u64,
    /// Bucket → commit count (XS/S/M/L/XL).
    pub histogram: HashMap<String, u32>,
}

/// Pull-request metrics section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSection {
    /// Total PRs (all states) matching any of the engineer's provider logins.
    pub total: u64,
    /// Merged PRs.
    pub merged: u64,
    /// Average cycle time (hours). `None` when no merged PRs with valid timestamps.
    pub avg_cycle_time_hours: Option<f64>,
    /// Median (p50) cycle time (hours). `None` when no merged PRs.
    pub median_cycle_time_hours: Option<f64>,
    /// p95 cycle time (hours). `None` when < 20 merged PRs.
    pub p95_cycle_time_hours: Option<f64>,
}

//! Data model for N-week period trend roll-ups.
//!
//! Provides [`AuthorPeriodSummary`] — the per-author, per-period record
//! returned by [`super::query_author_period_trends`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::report::drilldown::PrMetrics;

/// Aggregated per-author metrics for a single N-week period window.
///
/// Why: longitudinal contributor profiling (epic #558) needs period-level
/// roll-ups of the existing per-week data so callers can plot trends over time
/// without fetching the full weekly-activity grain.
/// What: holds all dimensions required for one period window — label, date bounds,
/// commit count, per-category breakdown, effort histogram, averaged quality score,
/// ticket coverage percentage, PR metrics, and repositories touched. All fields
/// derive from existing DB schema; no migrations required.
/// Test: see `query::tests::period_trends_basic_windowing` and related tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorPeriodSummary {
    /// Human-readable period label, e.g. `"2026-W01..W04"`.
    pub period_label: String,

    /// Inclusive start of the period (ISO 8601 date, e.g. `"2026-01-05"`).
    pub since: String,

    /// Inclusive end of the period (ISO 8601 date, e.g. `"2026-02-01"`).
    pub until: String,

    /// Total commits in this period window.
    pub commit_count: u64,

    /// Per-category commit counts (`HashMap<category, count>`).
    pub categories: HashMap<String, u64>,

    /// Effort size → commit count for effort-scored commits in the window.
    pub effort_histogram: HashMap<String, u32>,

    /// Average quality score across all weeks in the window.
    /// Computed as the mean of `fact_weekly_quality.quality_score` rows for
    /// this author that fall within the window. `0.0` when no quality data is
    /// available.
    pub quality_score: f64,

    /// Ticketed commits / total commits in `[0.0, 1.0]`.
    /// `0.0` when `commit_count == 0`.
    pub ticketed_pct: f64,

    /// Aggregated PR metrics for this period (cycle-time, counts).
    pub pr_metrics: PrMetrics,

    /// Distinct repositories touched in this period.
    pub repositories: Vec<String>,
}

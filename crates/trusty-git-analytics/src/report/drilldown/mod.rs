//! Per-engineer drill-down queries, data model, and report formatters.
//!
//! This module provides:
//! - Raw data-access free functions (effort histogram, PR metrics, commit summary)
//! - [`AuthorDrilldownData`] — the assembled report model
//! - [`format_markdown`] / [`format_json`] — output renderers for `tga author`
//!
//! Each data-access function is independently testable with a seeded in-memory
//! SQLite database.

pub mod format;
pub mod model;
pub mod queries;

pub use format::{format_json, format_markdown};
pub use model::{AuthorDrilldownData, CommitSection, EffortSection, PrSection, ReportPeriod};
pub use queries::{
    extract_provider_logins, lookup_author_for_drilldown, query_author_categories,
    query_commit_summary, query_effort_histogram, query_pr_metrics, CommitSummary, EffortHistogram,
    PrMetrics,
};

#[cfg(test)]
mod tests;

//! Stage 3 of the pipeline: read classified commits from a SQLite database
//! and generate CSV, JSON, and Markdown reports.
//!
//! ## Submodules
//!
//! - [`aggregator`]+[`persist`] — DB → in-memory [`ReportData`]; fact-table UPSERTs
//! - [`formatters`] — CSV / JSON / Markdown output
//! - [`templates`] — embedded Tera template strings
//! - [`pipeline`] — [`ReportPipeline`] orchestrator
//! - [`errors`] — [`ReportError`] / [`Result`]
//! - [`models`] — aggregated data structures
//! - [`period_trends`] — N-week period roll-up for contributor profiles (#558)
pub mod aggregator;
// #5453/#6004: ownership/bus-factor/trajectory figures the DD report renders.
pub mod authorship;
pub mod dd_manifest;
// #6190: folding a rebuilt manifest into one trusty-audit has already grounded.
pub mod dd_manifest_merge;
pub mod drilldown;
pub mod errors;
pub mod formatters;
pub mod models;
pub mod period_trends;
pub mod persist;
pub mod pipeline;
pub mod templates;
pub mod ticketed_stats;
// #5405: the board-correlation figures the DD report renders.
pub mod ticketing;

pub use authorship::{
    build_authorship_summary, merge_suggestions, recorded_repository_names, repository_has_commits,
    AuthorshipSummary, IdentityMergeRisk, AUTHORSHIP_SCHEMA_VERSION,
};
pub use dd_manifest::{
    build_dd_manifest, repo_name, DdManifest, DdManifestError, DdManifestOptions, DdReportSection,
    DdRepositoryEntry,
};
pub use errors::{ReportError, Result};
pub use models::ReportData;
pub use period_trends::{query_author_period_trends, AuthorPeriodSummary};
pub use pipeline::{ReportPipeline, ReportStats};
pub use ticketed_stats::{compute_ticketed_stats, TicketedStats};
pub use ticketing::{build_ticketing_summary, TicketingSummary, TICKETING_SCHEMA_VERSION};

#[cfg(test)]
mod tests;

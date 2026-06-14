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
pub mod drilldown;
pub mod errors;
pub mod formatters;
pub mod models;
pub mod period_trends;
pub mod persist;
pub mod pipeline;
pub mod templates;
pub mod ticketed_stats;

pub use errors::{ReportError, Result};
pub use models::ReportData;
pub use period_trends::{query_author_period_trends, AuthorPeriodSummary};
pub use pipeline::{ReportPipeline, ReportStats};
pub use ticketed_stats::{compute_ticketed_stats, TicketedStats};

#[cfg(test)]
mod tests;

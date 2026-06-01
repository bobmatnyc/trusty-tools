//! N-week period-trend rollup for the longitudinal contributor-profile epic (#558).
//!
//! Provides [`query_author_period_trends`] which aggregates existing per-week DB
//! rows into fixed-width N-week windows for a single canonical author. No schema
//! changes are required — all data is read from existing tables (`commits`,
//! `authors`, `fact_commit_effort`, `classifications`, `pull_requests`).
//!
//! ## Submodules
//!
//! - [`model`] — [`AuthorPeriodSummary`] data structure
//! - [`query`] — [`query_author_period_trends`] + private helpers

pub mod model;
pub mod query;

pub use model::AuthorPeriodSummary;
pub use query::query_author_period_trends;

#[cfg(test)]
mod tests;

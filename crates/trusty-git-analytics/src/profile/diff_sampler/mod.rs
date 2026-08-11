//! Diff sampler for contributor-profile period batches.
//!
//! Why: statistics say how much a contributor shipped; only diff text shows how
//! they wrote it. Fetching diffs is expensive (a libgit2 open plus a tree diff
//! per commit), so it runs as its own pass that a statistics-only profile can
//! skip entirely.
//! What: [`sample_diffs_for_batches`] walks each batch, selects up to
//! [`DiffSamplerConfig::max_diffs`] commits stratified by category, fetches
//! each diff, truncates it to [`MAX_DIFF_CHARS`], and attaches the result. A
//! repository that is not checked out locally is skipped with a warning rather
//! than aborting the run — a profile spanning ten repos should not fail because
//! one is missing.
//! Test: `tests` uses temp git repos and an in-memory database to cover
//! stratification, truncation, missing-repo skipping, and the `max_diffs` cap.

pub mod config;
pub mod sampler;

#[cfg(test)]
mod tests;

pub use config::{DiffSamplerConfig, DEFAULT_MAX_DIFFS, MAX_DIFF_CHARS};
pub use sampler::sample_diffs_for_batches;

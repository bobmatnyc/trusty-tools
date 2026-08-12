//! Representative-diff sampling for period batches.
//!
//! Why: period statistics say how much a contributor shipped, not how well.
//! Answering the second question needs actual diff text — but fetching every
//! commit's diff would be both slow and far larger than any context window, so
//! this stage picks a small, deliberately spread sample instead.
//! What: [`sample_diffs_for_batches`] walks each [`super::types::PeriodBatch`],
//! queries that period's commits, stratifies them so bugfix / feature /
//! refactor are each represented before the remaining slots go to the largest
//! commits, fetches each diff, truncates to [`MAX_DIFF_CHARS`], and appends the
//! result in place.
//! Test: `tests` uses temporary git repositories and in-memory databases to
//! exercise stratification, truncation, missing-repo skipping, and the cap.

pub mod config;
pub mod sampler;

#[cfg(test)]
mod tests;

pub use config::{DiffSamplerConfig, DEFAULT_MAX_DIFFS, MAX_DIFF_CHARS};
pub use sampler::sample_diffs_for_batches;

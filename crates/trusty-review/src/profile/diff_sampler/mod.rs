//! Diff sampler for contributor-profile period batches.
//!
//! Why: the LLM profile pass needs concrete diff text — not just statistics —
//! to reason about code quality trends.  This module selects a representative
//! subset of commits per period, retrieves their unified diffs via tga's
//! `diff_for_commit`, truncates to a safe length, and attaches the results to
//! each `PeriodBatch`.
//! What: provides [`sample_diffs_for_batches`] which iterates the batches in
//! place, queries the tga DB for the author's commits in each period,
//! stratifies by category (≥1 bugfix/feature/refactor if present, else by
//! descending effort), calls `diff_for_commit`, truncates to
//! [`MAX_DIFF_CHARS`], and gracefully skips commits whose repository is not
//! available locally (log + continue).
//! Test: `diff_sampler::tests` uses a temp git repo (via git2) to exercise the
//! full path; missing-repo skipping is tested with a repo path that doesn't
//! exist; truncation is tested with content larger than `MAX_DIFF_CHARS`.

pub mod config;
pub mod sampler;

#[allow(unused_imports)]
mod tests;

pub use config::{DEFAULT_MAX_DIFFS, DiffSamplerConfig, MAX_DIFF_CHARS};
pub use sampler::sample_diffs_for_batches;

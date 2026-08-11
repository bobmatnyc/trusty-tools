//! Period batch and sampled-diff types for the profile pipeline.
//!
//! Why: a period's statistics alone cannot support qualitative commentary —
//! "quality_score fell to 2.8" says nothing about what changed. Pairing the
//! statistics with a handful of representative diffs is what makes the period
//! reviewable.
//! What: defines [`SampledDiff`] and [`PeriodBatch`], the latter wrapping tga's
//! own [`AuthorPeriodSummary`] rather than redefining it.
//! Test: `sampled_diff_serde_roundtrip`, `sampled_diff_none_fields_omitted`,
//! and `period_batch_serde_roundtrip` in the parent `tests` module.

use serde::{Deserialize, Serialize};

pub use crate::report::period_trends::AuthorPeriodSummary;

// ─── SampledDiff ─────────────────────────────────────────────────────────────

/// A representative commit diff sampled from a contributor's history.
///
/// Why: statistics describe volume and cadence; only the diff text shows how
/// the code was actually written, which is what a quality assessment rests on.
/// What: pairs a commit's metadata (sha, repository, message, category, effort)
/// with the unified diff text produced by
/// [`diff_for_commit`](crate::collect::git::diff::diff_for_commit), truncated
/// to [`MAX_DIFF_CHARS`](crate::profile::diff_sampler::MAX_DIFF_CHARS).
/// Test: `sampled_diff_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledDiff {
    /// Full 40-char commit SHA.
    pub sha: String,

    /// Repository name, as stored in the `commits.repository` column.
    pub repository: String,

    /// Commit message.
    pub message: String,

    /// Unified diff text, already truncated.
    pub diff_text: String,

    /// Commit category (e.g. `"feature"`, `"bugfix"`, `"refactor"`), or `None`
    /// when the commit was never classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Effort size label (`"XS"` … `"XL"`), or `None` when the commit was
    /// never scored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// ─── PeriodBatch ─────────────────────────────────────────────────────────────

/// Combined statistics and sampled diffs for a single N-week period.
///
/// Why: the profile is assembled one period at a time, and every consumer
/// (renderer, narrative pass) wants the statistics and the diff samples
/// together rather than as two parallel collections it has to zip.
/// What: embeds tga's [`AuthorPeriodSummary`] unchanged and appends the diffs
/// the sampler selected. `sampled_diffs` stays empty until
/// [`sample_diffs_for_batches`](crate::profile::diff_sampler::sample_diffs_for_batches)
/// runs, which is what lets a statistics-only run skip the git work entirely.
/// Test: `period_batch_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PeriodBatch {
    /// Statistical summary for this period.
    pub stats: AuthorPeriodSummary,

    /// Representative commit diffs sampled from this period.
    #[serde(default)]
    pub sampled_diffs: Vec<SampledDiff>,
}

impl PeriodBatch {
    /// Construct a `PeriodBatch` from statistics, with no diffs attached yet.
    ///
    /// Why: batch assembly runs before diff sampling, so the batch must exist
    /// in a diff-less state.
    /// What: wraps `stats` with an empty `sampled_diffs`.
    /// Test: `period_batch_serde_roundtrip` plus every batch-assembly test.
    pub fn from_stats(stats: AuthorPeriodSummary) -> Self {
        Self {
            stats,
            sampled_diffs: Vec::new(),
        }
    }
}

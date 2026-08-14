//! Per-period batch and sampled-diff types.
//!
//! Why: statistics alone cannot support a qualitative judgement, so each period
//! carries both its [`AuthorPeriodSummary`] numbers and a handful of concrete
//! diffs. Bundling them means the narrative pass gets one object per period
//! rather than two lists it has to re-align.
//! What: defines [`SampledDiff`] and [`PeriodBatch`], and re-exports
//! [`AuthorPeriodSummary`] so callers need not reach into `report::period_trends`.
//! Test: `sampled_diff_serde_roundtrip`, `sampled_diff_none_fields_omitted`, and
//! `period_batch_serde_roundtrip` in the parent `types` test module.

use serde::{Deserialize, Serialize};

pub use crate::report::period_trends::AuthorPeriodSummary;

// ─── SampledDiff ──────────────────────────────────────────────────────────────

/// One representative commit diff drawn from a contributor's history.
///
/// `diff_text` is already truncated to `MAX_DIFF_CHARS` by the sampler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledDiff {
    /// Full commit SHA.
    pub sha: String,

    /// Repository name as stored in the `commits.repository` column.
    pub repository: String,

    /// Commit message.
    pub message: String,

    /// Unified diff text, truncated to `MAX_DIFF_CHARS`.
    pub diff_text: String,

    /// Commit category, e.g. `"feature"`; `None` when unclassified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Effort size label, e.g. `"M"`; `None` when unscored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// ─── PeriodBatch ──────────────────────────────────────────────────────────────

/// One period's statistics plus the diffs sampled from it.
///
/// `sampled_diffs` is empty until `sample_diffs_for_batches` fills it, so a
/// deterministic-only run leaves it empty rather than failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodBatch {
    /// Statistical summary for this period.
    pub stats: AuthorPeriodSummary,

    /// Representative diffs sampled from this period.
    #[serde(default)]
    pub sampled_diffs: Vec<SampledDiff>,
}

impl PeriodBatch {
    /// Wrap a period summary with an empty diff list.
    ///
    /// Test: exercised by every batch-assembly test in `profile::batch`.
    pub fn from_stats(stats: AuthorPeriodSummary) -> Self {
        Self {
            stats,
            sampled_diffs: Vec::new(),
        }
    }
}

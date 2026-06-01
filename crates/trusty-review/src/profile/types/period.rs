//! Period batch and sampled-diff types for the profile pipeline.
//!
//! Why: the LLM profile pass consumes one `PeriodBatch` per period, which
//! combines statistical summaries with concrete diff examples; separating these
//! types from the top-level `ContributorProfile` keeps each file focused.
//! What: defines `PeriodBatch` and `SampledDiff`.
//! Test: `sampled_diff_serde_roundtrip` and `period_batch_serde_roundtrip` in the
//! parent `tests` module.

use serde::{Deserialize, Serialize};

// Re-export the tga type so callers within this module can use the short form.
pub use tga::report::period_trends::AuthorPeriodSummary;

// ─── SampledDiff ──────────────────────────────────────────────────────────────

/// A representative commit diff sampled from a contributor's history.
///
/// Why: the LLM-based profile pass needs concrete diff text to reason about
/// code quality trends; raw `AuthorPeriodSummary` statistics alone are
/// insufficient for nuanced commentary.
/// What: pairs a commit's metadata (sha, repo, message, category, effort) with
/// the truncated unified diff text produced by `diff_for_commit`.
/// Test: see `sampled_diff_serde_roundtrip` and the diff-sampler unit tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledDiff {
    /// Full 40-char commit SHA.
    pub sha: String,

    /// Repository name (as stored in the tga `commits.repository` column).
    pub repository: String,

    /// Commit message (first line or full, depending on the DB row).
    pub message: String,

    /// Unified diff text, truncated to `MAX_DIFF_CHARS`.
    pub diff_text: String,

    /// Commit category (e.g. `"feature"`, `"bugfix"`, `"refactor"`).
    /// `None` when the commit was not classified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Effort size label (e.g. `"XS"`, `"S"`, `"M"`, `"L"`, `"XL"`).
    /// `None` when the commit was not scored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// ─── PeriodBatch ─────────────────────────────────────────────────────────────

/// Combined statistics and sampled diffs for a single N-week period.
///
/// Why: the LLM profile pass consumes one `PeriodBatch` per period, which
/// combines the statistical summary with concrete diff examples so the model
/// can correlate quantitative trends with qualitative observations.
/// What: embeds a tga [`AuthorPeriodSummary`] (reused directly — no
/// redefinition) and appends the `sampled_diffs` produced by the diff
/// sampler.  `sampled_diffs` is empty until the diff-sampler stage fills it.
/// Test: see `period_batch_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodBatch {
    /// Statistical summary for this period from tga.
    pub stats: AuthorPeriodSummary,

    /// Representative commit diffs sampled from this period.
    /// Empty until the diff-sampler stage populates it.
    #[serde(default)]
    pub sampled_diffs: Vec<SampledDiff>,
}

impl PeriodBatch {
    /// Construct a `PeriodBatch` from statistics, with an empty diff list.
    ///
    /// Why: the batch assembly stage creates the batch from stats alone;
    /// the diff sampler fills in `sampled_diffs` in a subsequent pass.
    /// What: wraps `stats` with an empty `sampled_diffs` vector.
    /// Test: exercised by all batch-assembly tests.
    pub fn from_stats(stats: AuthorPeriodSummary) -> Self {
        Self {
            stats,
            sampled_diffs: Vec::new(),
        }
    }
}

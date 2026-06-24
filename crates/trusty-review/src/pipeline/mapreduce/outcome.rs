//! Map-stage outcome and reduce-stage result types (Phase 3-5, #1643 / #680).
//!
//! Why: the map stage produces one outcome per `MapUnit`; the reduce stage
//! aggregates those outcomes into a single `ReducedReview` that the runner
//! folds into the existing `ReviewResult`.  Keeping these wire types in their
//! own module (separate from the fan-out logic in `map.rs` and the aggregation
//! logic in `reduce.rs`) honours the 500-SLOC cap and keeps each stage focused.
//!
//! What: `MapOutcome` (per-unit result — reviewed / skipped / failed),
//! `MapReduceStats` (honest partial-coverage telemetry), and `ReducedReview`
//! (the merged verdict + findings the runner consumes).
//!
//! Test: `mapreduce/reduce_tests.rs` and `mapreduce/map_tests.rs`.

use crate::models::{Finding, Verdict};

// ─── MapOutcome ────────────────────────────────────────────────────────────────

/// The result of processing a single `MapUnit` in the map stage.
///
/// Why: the reduce stage must distinguish a unit that was reviewed (findings +
/// verdict) from one that was skipped (metadata-only, no LLM call) or one whose
/// LLM call failed (fail-open: drop that file's review, never poison the whole
/// review).  A typed enum makes the reduce match exhaustive.
/// What: `Reviewed` carries the per-unit verdict and findings; `Skipped` carries
/// the metadata note (e.g. `"deleted file"`); `Failed` carries the per-chunk
/// error string AND a flag distinguishing a pathological over-budget chunk (the
/// #1639 single-file-over-cap fail-closed case) from a transient LLM error.
/// Test: constructed in `map_tests.rs`; consumed in `reduce_tests.rs`.
#[derive(Debug, Clone)]
pub enum MapOutcome {
    /// The unit was sent to the LLM and produced a verdict + findings.
    Reviewed {
        /// File path this outcome covers (for stats / grouping).
        file: String,
        /// Per-chunk verdict parsed from the LLM response.
        verdict: Verdict,
        /// Findings parsed from this chunk's review.
        findings: Vec<Finding>,
    },
    /// The unit was metadata-only — no LLM call was made (deleted / binary /
    /// rename-only / summary-only / budget-exhausted).
    Skipped {
        /// File path this outcome covers.
        file: String,
        /// Reason note from the splitter (surfaced in the partial banner).
        note: String,
    },
    /// The unit's LLM call failed, OR the chunk was a single hunk that alone
    /// exceeded the per-file budget (the #1639 pathological case).  Fail-OPEN:
    /// the reduce stage drops this file's review and records it, never letting
    /// one chunk poison the whole review.
    Failed {
        /// File path this outcome covers.
        file: String,
        /// Human-readable error reason.
        error: String,
        /// True when the failure was a single-hunk-over-budget chunk that could
        /// not be split further (#1639 backstop), rather than a transient LLM
        /// error.  Surfaced separately in `MapReduceStats`.
        hunk_oversized: bool,
    },
}

impl MapOutcome {
    /// File path this outcome refers to.
    ///
    /// Why: the reduce stage groups outcomes by file and the stats builder needs
    /// the path regardless of the variant.
    /// What: returns the `file` field of whichever variant.
    /// Test: covered transitively by `reduce_tests.rs`.
    pub fn file(&self) -> &str {
        match self {
            MapOutcome::Reviewed { file, .. }
            | MapOutcome::Skipped { file, .. }
            | MapOutcome::Failed { file, .. } => file,
        }
    }
}

// ─── MapReduceStats ──────────────────────────────────────────────────────────

/// Honest partial-coverage telemetry for a map-reduce review (§ "Failure modes"
/// of #680).
///
/// Why: a map-reduce review can legitimately skip files (metadata-only), drop
/// files (LLM failure), or leave a pathological single-file-over-cap chunk
/// unreviewed.  The result must NEVER silently present itself as complete; this
/// struct carries the counts so the runner can emit a degraded banner (analogous
/// to the `[DIFF TRUNCATED …]` honesty marker) when coverage is partial.
/// What: counts of units in each terminal state plus the number of surfaced
/// findings after dedup.  `is_partial()` is true when any file was failed or
/// any hunk was oversized.
/// Test: `reduce_stats_*` in `reduce_tests.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapReduceStats {
    /// Total map units produced by the splitter.
    pub units_total: usize,
    /// Units that were reviewed by an LLM call (`Reviewed`).
    pub files_reviewed: usize,
    /// Units skipped without an LLM call (`Skipped` — metadata-only/budget).
    pub files_skipped: usize,
    /// Units whose LLM call failed and whose review was dropped (`Failed`).
    pub files_failed: usize,
    /// Units that were a single hunk exceeding the per-file budget (#1639
    /// pathological case — a subset of `files_failed`).
    pub hunks_oversized: usize,
    /// Number of findings surfaced after dedup + the `max_findings` cap.
    pub findings_surfaced: usize,
}

impl MapReduceStats {
    /// Returns `true` when coverage was partial (a file failed or a hunk was
    /// over-cap), so the runner must label the review non-authoritative.
    ///
    /// Why: a partial map-reduce review must be honestly flagged — the #1639
    /// fail-closed guard is the backstop, but for a per-file partial result we
    /// surface a degraded banner instead of failing the whole review CLOSED.
    /// What: returns `files_failed > 0 || hunks_oversized > 0`.
    /// Test: `reduce_stats_partial_flag`.
    pub fn is_partial(&self) -> bool {
        self.files_failed > 0 || self.hunks_oversized > 0
    }
}

// ─── ReducedReview ─────────────────────────────────────────────────────────────

/// The merged output of the reduce stage — what the runner folds into `ReviewResult`.
///
/// Why: the reduce stage's job is to collapse N per-chunk outcomes into ONE
/// verdict + finding set with the SAME shape the unified path produces, so the
/// downstream grade/verify/post code is unchanged.
/// What: `verdict` is derived deterministically from the union of findings via
/// the existing `derive_verdict` precedence rules (a chunk REQUEST_CHANGES/BLOCK
/// propagates up); `findings` is the deduped, capped, prioritised union;
/// `stats` carries the partial-coverage telemetry.
/// Test: `reduce_*` in `reduce_tests.rs`.
#[derive(Debug, Clone)]
pub struct ReducedReview {
    /// Aggregated verdict derived deterministically from the union of findings.
    pub verdict: Verdict,
    /// Deduped, prioritised, capped union of all per-chunk findings.
    pub findings: Vec<Finding>,
    /// Partial-coverage telemetry.
    pub stats: MapReduceStats,
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;

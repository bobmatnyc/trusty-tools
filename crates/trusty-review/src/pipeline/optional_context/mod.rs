//! Optional review inputs and the `run_review_with` entry point's types (#9192).
//!
//! Why: epic #9191 adds inputs a caller must ask for, and the owner ruled the
//! library stays source-compatible: `CallerContext`, `ReviewDeps` and
//! `ReviewResult` keep their shape, so the new inputs travel beside them.
//! What: [`ReviewOptions`] carries what `run_review_with` takes beyond
//! `run_review`'s arguments; [`ReviewOutcome`] is what it returns.
//! Test: `off_is_byte_identical_unified`, `off_is_byte_identical_mapreduce`.

use std::sync::Arc;

use crate::models::ReviewResult;

pub(crate) mod assemble;
pub(crate) mod seams;

pub(crate) use seams::PrSource;

/// What `run_review_with` takes beyond `run_review`'s arguments (#9192).
///
/// Why: a default value runs the review exactly as `run_review` does, so a
/// caller opts in to each new input and nothing else changes.
/// What: the PR seam tests inject; production leaves it `None`.
/// Test: `off_is_byte_identical_unified`.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ReviewOptions {
    /// Test seam for the GitHub metadata and diff reads (#9192).
    pub(crate) pr_source: Option<Arc<dyn PrSource>>,
}

/// What `run_review_with` returns (#9192).
///
/// Why: the review result keeps its serialized shape; anything new travels
/// beside it rather than inside it.
/// What: the `ReviewResult` `run_review` would have returned.
/// Test: `off_is_byte_identical_unified`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReviewOutcome {
    /// The review result, identical to what `run_review` returns.
    pub result: ReviewResult,
}

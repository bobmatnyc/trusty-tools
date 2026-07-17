//! Map-reduce review pipeline — per-file diff splitting + LLM fan-out + reduce.
//!
//! Why: the unified-diff path silently drops files when the diff exceeds
//! `MAX_DIFF_CHARS`; the map-reduce path reviews each file independently and
//! then reduces the per-file verdicts into one merged result.  This module is
//! the public API surface for all map-reduce sub-stages and the top-level
//! `run_map_reduce` orchestrator the runner calls (Phase 5, #1643).
//!
//! What:
//!  - Phase 2: `split_into_units` turns a `FilteredDiff` into `Vec<MapUnit>`.
//!  - Phase 3: `run_map_stage` reviews each unit with a bounded-parallel LLM
//!    fan-out (`map.rs`).
//!  - Phase 4: `reduce` aggregates the per-chunk outcomes into one
//!    `ReducedReview` (`reduce.rs`).
//!  - Phase 5: `run_map_reduce` (this file) wires split → map → reduce so the
//!    runner has a single entry point.
//!
//! The module uses a re-export facade (`mod.rs`) per the 500-line-cap convention.
//!
//! Test: comprehensive unit tests live in `splitter_tests.rs`, `map_tests.rs`,
//! `reduce_tests.rs`, `outcome_tests.rs`, and the end-to-end runner tests.

use std::sync::Arc;

use tracing::info;

use crate::{
    config::mapreduce::MapReduceConfig, llm::LlmProvider,
    pipeline::diff_analyzer::models::FilteredDiff,
};

pub mod map;
pub mod outcome;
pub mod reduce;
pub mod splitter;
pub mod synthesis;
pub mod unit;

pub use map::{MapContext, run_map_stage};
pub use outcome::{MapOutcome, MapReduceStats, ReducedReview};
pub use reduce::reduce;
pub use splitter::split_into_units;
pub use synthesis::synthesize_review;
pub use unit::{MapUnit, MapUnitKind};

/// Run the full map-reduce review: split → map (bounded fan-out) → reduce → synthesis.
///
/// Why: the runner needs ONE call that takes the already-filtered diff and the
/// shared review context and returns a merged `ReducedReview` whose shape matches
/// the unified path.  Keeping the split/map/reduce wiring here (not in the
/// near-cap `runner.rs`) honours the SLOC cap and keeps the runner readable.
/// What: splits `filtered` into `MapUnit`s under the config budgets, fans the
/// `Review` units out over `config.concurrency` LLM calls, reduces the
/// outcomes into a `ReducedReview` (deterministic verdict + deduped findings +
/// partial-coverage stats), and then runs an optional LLM synthesis pass
/// (`config.synthesis`, default `true`, closes #1663) that calibrates the
/// aggregate verdict to match a single holistic reviewer.  The synthesis pass
/// applies a High-severity safety floor so critical findings can never be softened;
/// the count-based `≥2 Medium → REQUEST_CHANGES` floor is intentionally omitted
/// in the synthesis path because the LLM has already judged those findings
/// holistically.  Never truncates: every surviving file/hunk reaches a reviewer
/// (or is honestly recorded as skipped/failed in the stats).
/// Test: `reduce_tests.rs`, `synthesis_tests.rs`, and the runner integration tests
/// `run_review_oversized_diff_mapreduce_reviews_tail_signature` etc.
pub async fn run_map_reduce(
    filtered: &FilteredDiff,
    llm: &Arc<dyn LlmProvider>,
    ctx: &MapContext<'_>,
    config: &MapReduceConfig,
) -> ReducedReview {
    let units = split_into_units(filtered, config);
    info!(
        files = filtered.files.len(),
        units = units.len(),
        concurrency = config.concurrency,
        "map-reduce: split diff into units"
    );
    let mut outcomes = run_map_stage(&units, llm, ctx, config.concurrency).await;

    // Verify diff-provable citations BEFORE reduce derives the aggregate verdict
    // (#2881): a per-unit reviewer can confabulate a `code_provable: true` finding
    // whose cited content is not in the file it reviewed (the repro attributed a
    // new test file's content to a large regenerated bundle).  Downgrade any such
    // finding against the whole filtered diff so it can never force the deterministic
    // BLOCK / REQUEST_CHANGES floor in `reduce`/`synthesize`.
    let cite_index = crate::pipeline::citation_check::DiffContentIndex::from_filtered(filtered);
    for outcome in &mut outcomes {
        if let MapOutcome::Reviewed { findings, .. } = outcome {
            crate::pipeline::citation_check::downgrade_uncitable_findings(findings, &cite_index);
        }
    }

    let reduced = reduce(outcomes, config);
    synthesize_review(reduced, llm, ctx, config).await
}

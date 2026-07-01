//! Map stage — bounded-parallel per-file LLM reviews (Phase 3, #1643 / #680).
//!
//! Why: the unified path sends one truncated diff to a single LLM call, dropping
//! trailing files.  The map stage instead reviews EACH `MapUnit` (one file, or a
//! whole-hunk sub-chunk of an oversized file) with its OWN LLM call, so no
//! changed code is ever invisible — the exact failure mode from #1638 (a changed
//! `build(…, null)` signature near the end of a large diff).
//!
//! What: `run_map_stage` fans the units out over `buffer_unordered(concurrency)`
//! (the same bounded pattern as the verify round, `verify.rs:142`), builds each
//! call's prompt with the SAME `build_review_prompt_with_coverage` the unified
//! path uses (so `parse_review_response` works unchanged), and collects one
//! `MapOutcome` per unit.  Failures are fail-OPEN: a failed unit becomes
//! `MapOutcome::Failed` (its file's review is dropped, recorded in stats) rather
//! than poisoning the whole review.  A single hunk that alone exceeds the budget
//! (`hunk_oversized`) is the pathological #1639 case — it is failed-closed for
//! that chunk ONLY.
//!
//! Test: `mapreduce/map_tests.rs`.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use tracing::{debug, warn};

use crate::{
    llm::{LlmProvider, LlmRequest},
    pipeline::{
        parser::parse_review_response,
        prompt::{ReviewContext, ReviewPrMeta, build_review_prompt_with_coverage},
    },
    voice::VoiceConfig,
};

use super::outcome::{MapOutcome, TokenUsage};
use super::unit::{MapUnit, MapUnitKind};

/// Borrowed inputs shared across every map-stage LLM call.
///
/// Why: each per-file call reuses the SAME PR metadata, code-search context,
/// model id, and voice config as the unified path — only the diff text differs
/// per unit.  Bundling the shared borrows into one struct keeps `run_map_stage`'s
/// signature small and avoids threading eight arguments through the fan-out.
/// What: all fields are borrows into the runner's owned values; the struct is
/// `Copy`-cheap to clone for each task because it holds references.
/// Test: constructed in `map_tests.rs`.
pub struct MapContext<'a> {
    /// GitHub org (passed through to the prompt builder).
    pub owner: &'a str,
    /// GitHub repo.
    pub repo: &'a str,
    /// PR metadata (title/body/author/url) — same for every chunk.
    pub pr_meta: &'a ReviewPrMeta,
    /// Code-search / analyze context — same for every chunk (PR-level slice).
    pub context: &'a ReviewContext,
    /// External (JIRA/Confluence) context markdown — same for every chunk.
    pub external_context: &'a str,
    /// Reviewer model id (may carry a routing prefix; prompt builder strips it).
    pub reviewer_model: &'a str,
    /// Layered voice config — same for every chunk.
    pub voice_config: &'a VoiceConfig,
    /// Whether coverage-gating prompt language is enabled.
    pub coverage_enabled: bool,
}

/// Review every `MapUnit` and return one `MapOutcome` per unit.
///
/// Why: this is the core of the no-truncation guarantee — each unit's diff text
/// is bounded to the per-file budget by the splitter, so every changed line
/// reaches a reviewer.  Bounded concurrency caps cost / rate-limit pressure.
/// What:
///   - `MetadataOnly` units → `Skipped` (no LLM call).
///   - `Review` units with `hunk_oversized` → `Failed { hunk_oversized: true }`
///     (the #1639 pathological single-hunk-over-cap case; failed-closed for that
///     chunk only so it never poisons the rest of the review).
///   - other `Review` units → one LLM call; on success `Reviewed`, on transport
///     error `Failed { hunk_oversized: false }` (fail-open, drop that file).
///
/// Concurrency is bounded by `concurrency` (mirrors `VERIFY_CONCURRENCY`).
/// Per-chunk results are returned in COMPLETION order; the reduce stage sorts
/// deterministically, so order here is not load-bearing.
///
/// Test: `map_reviews_each_unit`, `map_skips_metadata_only`,
/// `map_failed_unit_does_not_poison`, `map_oversized_hunk_fails_closed_for_chunk`.
pub async fn run_map_stage(
    units: &[MapUnit],
    llm: &Arc<dyn LlmProvider>,
    ctx: &MapContext<'_>,
    concurrency: usize,
) -> Vec<MapOutcome> {
    let conc = concurrency.max(1);
    debug!(
        units = units.len(),
        concurrency = conc,
        "map stage: starting bounded fan-out"
    );

    // Plan every unit SYNCHRONOUSLY into an owned task — building the
    // `LlmRequest` up front means the async tasks hold no borrows into `ctx`
    // across an await point.  This keeps the fan-out future `Send` for ALL
    // lifetimes (it is `tokio::spawn`-ed by the webhook service), avoiding the
    // higher-ranked-lifetime Send failure that a borrowed `&MapContext` would
    // otherwise introduce.
    let tasks: Vec<MapTask> = units.iter().map(|u| plan_unit(u, ctx)).collect();

    // Tasks that need no LLM call resolve immediately; only `Call` tasks fan out.
    stream::iter(tasks)
        .map(|task| {
            let llm = Arc::clone(llm);
            async move { run_task(task, &llm).await }
        })
        .buffer_unordered(conc)
        .collect::<Vec<MapOutcome>>()
        .await
}

/// An owned, borrow-free plan for processing one `MapUnit`.
///
/// Why: pre-computing the per-unit work as an owned value (request + file, or a
/// terminal outcome) removes every borrow from the async fan-out, which is what
/// keeps the spawned review future `Send` for all lifetimes.
/// What: `Resolved` carries an already-decided outcome (metadata-only skip or
/// the #1639 oversized-hunk fail-closed); `Call` carries the built `LlmRequest`
/// and the file path needed to stamp findings + build the outcome.
/// Test: exercised by all `map_*` tests via `run_map_stage`.
enum MapTask {
    /// No LLM call — the outcome is already known.
    Resolved(MapOutcome),
    /// One LLM call is required for `file`; `req` is fully built.
    Call {
        /// File path for this unit (used for findings + the outcome).
        file: String,
        /// The fully-built reviewer request for this unit's diff text.
        req: Box<LlmRequest>,
    },
}

/// Build the owned `MapTask` for a unit (synchronous, borrow-free output).
///
/// Why: see `MapTask` — planning synchronously is what keeps the fan-out `Send`.
/// What: metadata-only → `Resolved(Skipped)`; oversized single hunk →
/// `Resolved(Failed{hunk_oversized:true})` (#1639 backstop); otherwise builds the
/// reviewer prompt and returns `Call`.
/// Test: covered by `map_*` tests.
fn plan_unit(unit: &MapUnit, ctx: &MapContext<'_>) -> MapTask {
    match &unit.kind {
        MapUnitKind::MetadataOnly { note } => MapTask::Resolved(MapOutcome::Skipped {
            file: unit.file.clone(),
            note: note.clone(),
        }),
        MapUnitKind::Review { diff_text } => {
            if unit.hunk_oversized {
                warn!(
                    file = %unit.file,
                    chunk = unit.chunk_index,
                    "map stage: single hunk exceeds per-file budget — failing CLOSED for this chunk only (#1639 backstop)"
                );
                return MapTask::Resolved(MapOutcome::Failed {
                    file: unit.file.clone(),
                    error: format!(
                        "file {} chunk {} is a single hunk larger than the per-file budget; \
                         could not review without truncation",
                        unit.file, unit.chunk_index
                    ),
                    hunk_oversized: true,
                });
            }
            let req = build_review_prompt_with_coverage(
                ctx.owner,
                ctx.repo,
                ctx.pr_meta,
                diff_text,
                ctx.context,
                ctx.external_context,
                ctx.reviewer_model,
                ctx.voice_config,
                ctx.coverage_enabled,
            );
            MapTask::Call {
                file: unit.file.clone(),
                req: Box::new(req),
            }
        }
    }
}

/// Execute one planned `MapTask`, producing its `MapOutcome`.
///
/// Why: the async half of the split — it only owns the task and a cloned `Arc`,
/// so it holds no borrows across the LLM await.
/// What: `Resolved` returns its outcome directly; `Call` issues the LLM request,
/// parses the response (stamping the unit's file onto file-less findings so
/// inline anchoring is preserved), and fail-OPENs a transport error to `Failed`.
/// Test: covered by all `map_*` tests.
async fn run_task(task: MapTask, llm: &Arc<dyn LlmProvider>) -> MapOutcome {
    let (file, req) = match task {
        MapTask::Resolved(outcome) => return outcome,
        MapTask::Call { file, req } => (file, *req),
    };

    match llm.complete(req).await {
        Ok(resp) => {
            let parsed = parse_review_response(&resp.text);
            debug!(
                file = %file,
                verdict = %parsed.verdict,
                findings = parsed.findings.len(),
                "map stage: reviewed chunk"
            );
            let findings = parsed
                .findings
                .into_iter()
                .map(|mut f| {
                    if f.file.trim().is_empty() || f.file == "unknown" {
                        f.file = file.clone();
                    }
                    f
                })
                .collect();
            MapOutcome::Reviewed {
                file,
                verdict: parsed.verdict,
                findings,
                // Capture this chunk's token/cost telemetry so the reduce stage
                // can sum it into the aggregate the shallow-review heuristic
                // reads (#1885) — the unified path gets these off its single call.
                tokens: TokenUsage {
                    input_tokens: resp.input_tokens,
                    output_tokens: resp.output_tokens,
                    cost_usd: resp.cost_usd,
                },
            }
        }
        Err(e) => {
            // Fail-OPEN: one chunk's transport error drops that file's review and
            // is recorded, but the rest of the map stage proceeds.  The reduce
            // stage flags partial coverage.
            warn!(
                file = %file,
                error = %e,
                "map stage: chunk LLM call failed — dropping this file's review (fail-open)"
            );
            MapOutcome::Failed {
                file,
                error: format!("LLM error: {e}"),
                hunk_oversized: false,
            }
        }
    }
}

#[cfg(test)]
#[path = "map_tests.rs"]
mod tests;

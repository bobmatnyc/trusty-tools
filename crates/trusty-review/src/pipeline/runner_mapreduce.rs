//! Map-reduce branch of the review runner (Phase 5, #1643 / #680).
//!
//! Why: over-cap diffs must be reviewed COMPLETELY, per-file, with no
//! truncation.  The unified path (and the #1639 fail-closed backstop) cannot do
//! that.  This module is the runner's map-reduce branch: it runs split → map →
//! reduce, then folds the deterministic `ReducedReview` into the SAME
//! `ReviewResult` shape the unified path produces so downstream grade / verify /
//! coverage-floor / inline-comment / post code is unchanged.
//!
//! Extracted from `runner.rs` to keep that file under the 500-SLOC cap (#610);
//! the runner only computes `DiffStats`, calls `select_review_mode`, and on
//! `ReviewPath::MapReduce` delegates here.
//!
//! What: `run_mapreduce_branch` is the single entry point.  The verdict is
//! derived DETERMINISTICALLY inside `reduce` (never by a summariser), then the
//! existing `apply_grade_and_floor` → coverage-floor → `maybe_verify` → grade
//! clamp → inline-comment → finalize chain runs exactly as for the unified path.
//!
//! Test: `run_review_oversized_diff_mapreduce_reviews_tail_signature`,
//! `run_review_mapreduce_chunk_request_changes_propagates` (runner_tests.rs).

use tracing::{info, warn};

use crate::{
    config::{MapReduceConfig, ReviewConfig},
    coverage::{CoverageVerdictContrib, apply_coverage_floor},
    models::{ReviewResult, ReviewStatus},
    pipeline::{
        diff_analyzer::models::FilteredDiff,
        letter_grade::clamp_grade_to_verdict,
        mapreduce::{MapContext, ReducedReview, run_map_reduce},
        parser::ParsedReview,
        prompt::{ReviewContext, ReviewPrMeta},
        runner::{ReviewDeps, ReviewInput},
        runner_helpers::{abort_dry, apply_grade_and_floor, attach_inline_comments, finalize_run},
        verify::maybe_verify,
        voice_config::build_voice_config,
    },
};

/// Owned inputs the map-reduce branch needs beyond the borrowed config/deps.
///
/// Why: bundling the per-run values (filtered diff, PR metadata, gathered
/// context, raw diff, coverage contribution, degraded reason) into one struct
/// keeps `run_mapreduce_branch`'s signature short and mirrors how the unified
/// path threads these through `run_review`.
/// What: all fields are owned/moved values produced earlier in `run_review`.
/// Test: constructed by `run_review`; exercised via the runner integration tests.
pub(super) struct MapReduceRun {
    /// Noise-filtered diff (DiffAnalyzer Stages A+B output) to split per-file.
    pub filtered: FilteredDiff,
    /// Raw (pre-filter) diff for inline-comment anchoring (#1414 parity).
    pub raw_diff: String,
    /// PR metadata for the per-file prompts.
    pub pr_meta: ReviewPrMeta,
    /// Gathered code-search/analyze/APEX context (PR-level, sliced per file).
    pub context: ReviewContext,
    /// External (JIRA/Confluence) context markdown.
    pub external_context: String,
    /// Coverage verdict contribution (None when coverage gating disabled).
    pub coverage_contrib: Option<CoverageVerdictContrib>,
    /// Degraded reason from the #590 context gate (None = authoritative).
    pub degraded_reason: Option<String>,
}

/// Run the map-reduce review branch and return the finalized `ReviewResult`.
///
/// Why: this is the REAL fix for over-cap diffs — every file is reviewed with
/// its own LLM call so no changed code is ever invisible (the #1638 symptom).
/// What: runs split → map → reduce via `run_map_reduce`, then folds the
/// `ReducedReview` into `result` and runs the SAME post-LLM chain as the unified
/// path (grade floor, coverage floor, verification, grade clamp, inline comments,
/// finalize).  When the reduce stage assessed NOTHING (no reviewed chunks), the
/// result fails CLOSED to UNKNOWN (the #1639 backstop, now only for the truly
/// pathological all-failed case).  Partial coverage (some files failed/oversized)
/// is honestly labelled via a degraded banner, not failed closed.
/// Test: `run_review_oversized_diff_mapreduce_reviews_tail_signature`,
/// `run_review_mapreduce_chunk_request_changes_propagates`.
pub(super) async fn run_mapreduce_branch(
    config: &ReviewConfig,
    input: &ReviewInput,
    deps: &ReviewDeps,
    mr_config: &MapReduceConfig,
    mut result: ReviewResult,
    run: MapReduceRun,
) -> ReviewResult {
    let voice_config = build_voice_config(config);
    let ctx = MapContext {
        owner: &result.owner,
        repo: &result.repo,
        pr_meta: &run.pr_meta,
        context: &run.context,
        external_context: &run.external_context,
        reviewer_model: &input.reviewer_model,
        voice_config: &voice_config,
        coverage_enabled: config.coverage.enabled,
    };

    info!(
        files = run.filtered.files.len(),
        "map-reduce branch: reviewing over-cap diff per-file (no truncation)"
    );
    let reduced: ReducedReview = run_map_reduce(&run.filtered, &deps.llm, &ctx, mr_config).await;

    // Pathological all-failed case: nothing was actually reviewed.  This is the
    // residual #1639 backstop — fail CLOSED to UNKNOWN rather than emit a green
    // check for a review that produced zero assessed chunks.
    if reduced.stats.files_reviewed == 0 {
        warn!(
            units = reduced.stats.units_total,
            failed = reduced.stats.files_failed,
            skipped = reduced.stats.files_skipped,
            "map-reduce branch: no chunk was reviewed — failing CLOSED to UNKNOWN (#1639 backstop)"
        );
        result.verdict = crate::models::Verdict::Unknown;
        result.error = Some(
            "map-reduce review assessed no files (all chunks failed or were skipped) — \
             could not review"
                .to_string(),
        );
        return abort_dry(result, config, input, deps);
    }

    // Synthesise a `ParsedReview` from the DETERMINISTIC reduce output so the
    // existing grade/floor chain treats it identically to a unified parse.
    let parsed = ParsedReview {
        verdict: reduced.verdict.clone(),
        grade: None,
        summary: String::new(),
        findings: reduced.findings.clone(),
        is_fail_safe: false,
        fail_safe_reason: None,
    };

    // Telemetry: surface the map-reduce model.
    result.model = input.reviewer_model.clone();

    // Narrative body: the unified path sets `review_body` from the LLM prose
    // (`apply_llm_response`), but the map-reduce path has N per-chunk responses
    // and no single prose summary.  Without a body the GitHub poster substitutes
    // the "_No narrative summary was produced_" sentinel on EVERY over-cap review
    // (posting.rs:138).  Synthesise a deterministic one-line summary from the
    // reduce stats so the posted comment is informative.
    result.review_body = format!(
        "Map-reduce review: {} file(s) reviewed across {} unit(s), \
         {} skipped, {} failed; {} finding(s) surfaced.",
        reduced.stats.files_reviewed,
        reduced.stats.units_total,
        reduced.stats.files_skipped,
        reduced.stats.files_failed,
        reduced.stats.findings_surfaced,
    );

    let degraded_reason = run.degraded_reason.clone();
    fold_reduced_into_result(config, deps, &mut result, parsed, &run).await;

    // Honest partial-coverage labelling (analogous to the #1638 truncation
    // marker): when some files could not be reviewed the review is non-
    // authoritative.  Surface it in BOTH the posted body (a visible banner) and
    // the internal `error` field so neither the PR author nor a log consumer
    // mistakes a partial review for a complete one.
    //
    // Note: status is set AFTER the fold so finalize_run cannot clobber it.
    if reduced.stats.is_partial() {
        let llm_errors = reduced
            .stats
            .files_failed
            .saturating_sub(reduced.stats.hunks_oversized);
        let notice = format!(
            "> **Coverage notice:** {} file(s) could not be reviewed \
             ({} LLM error(s), {} over-cap hunk(s)) — this review is partial.\n\n",
            reduced.stats.files_failed, llm_errors, reduced.stats.hunks_oversized,
        );
        result.review_body = format!("{notice}{}", result.review_body);
        result.status = ReviewStatus::Degraded;
        if result.error.is_none() {
            result.error = Some(format!(
                "map-reduce coverage partial: {} reviewed, {} skipped, {} failed \
                 ({} LLM error(s), {} over-cap hunk(s))",
                reduced.stats.files_reviewed,
                reduced.stats.files_skipped,
                reduced.stats.files_failed,
                llm_errors,
                reduced.stats.hunks_oversized,
            ));
        }
    }

    // Degraded labelling (#590 parity with the unified path): when an operator
    // opted out of a required context dependency, prepend a banner and set the
    // error reason so no consumer mistakes the review for authoritative.
    if let Some(reason) = degraded_reason.as_ref() {
        result.status = ReviewStatus::Degraded;
        result.review_body = format!(
            "{}{}",
            crate::pipeline::context_gate::degraded_banner(reason),
            result.review_body
        );
        if result.error.is_none() {
            result.error = Some(format!("degraded (non-authoritative): {reason}"));
        }
    }

    finalize_run(result, config, input, deps.dedup.as_ref()).await
}

/// Apply the post-LLM grade/verify/inline chain to a reduced parse.
///
/// Why: the reduce output must go through the EXACT same severity floor,
/// coverage floor, verification round, grade clamp, and inline-comment mapping
/// as the unified path so the verdict policy never drifts between the two paths.
/// What: mirrors `run_review` steps 7b–7e against the synthesised `parsed`.
/// Test: covered by the map-reduce runner integration tests.
async fn fold_reduced_into_result(
    config: &ReviewConfig,
    deps: &ReviewDeps,
    result: &mut ReviewResult,
    parsed: ParsedReview,
    run: &MapReduceRun,
) {
    let (final_verdict, final_grade, original_llm_grade) = apply_grade_and_floor(&parsed);

    // Coverage floor (no-op when coverage gating disabled).
    let (final_verdict, _cov_grade) = if let Some(ref cov) = run.coverage_contrib {
        apply_coverage_floor(final_verdict, final_grade, cov)
    } else {
        (final_verdict, final_grade)
    };

    let mut findings = parsed.findings;
    // Verification round — re-derives the verdict from surviving findings.  The
    // verifier sees the RAW diff so it can check any finding's location.
    result.verdict = maybe_verify(
        config,
        deps.verifier.as_ref(),
        &run.raw_diff,
        final_verdict,
        &mut findings,
    )
    .await;
    result.findings = findings;

    // Envelope grade: clamp the original (pre-floor) grade to the post-
    // verification verdict (closes #1486 parity with the unified path).
    result.grade = Some(clamp_grade_to_verdict(original_llm_grade, &result.verdict).to_string());

    // Inline per-line comments from the RAW diff (#1414 parity).
    attach_inline_comments(result, &run.raw_diff);
}

#[cfg(test)]
#[path = "runner_mapreduce_tests.rs"]
mod tests;

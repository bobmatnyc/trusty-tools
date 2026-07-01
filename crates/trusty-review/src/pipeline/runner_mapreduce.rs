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
        letter_grade::{Grade, clamp_grade_to_verdict, default_grade_for_verdict},
        mapreduce::{MapContext, ReducedReview, run_map_reduce},
        parser::ParsedReview,
        prompt::{ReviewContext, ReviewPrMeta},
        runner::{ReviewDeps, ReviewInput},
        runner_helpers::{
            abort_dry, apply_grade_and_floor, attach_inline_comments, build_author_rationale,
            finalize_run,
        },
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

    // When the synthesis pass (#1663) ran successfully, `reduced.grade` carries
    // the calibrated letter grade and `reduced.summary` carries the prose summary.
    // Thread both into the ParsedReview so the downstream grade/verify chain can
    // use them; the synthesis_active flag tells fold_reduced_into_result to use
    // the synthesis-floored verdict directly instead of re-applying the full
    // derive_verdict_with_grade (which would re-add the count-based Medium floor
    // that synthesis is calibrating away).
    let synthesis_active = reduced.grade.is_some();
    let parsed = ParsedReview {
        verdict: reduced.verdict.clone(),
        grade: reduced.grade.clone(),
        // Thread the pre-floor grade for observable telemetry (#1665 item 3).
        // When synthesis fired the two-tier floor, grade_pre_floor differs from
        // grade and downstream code can detect it.
        grade_pre_floor: reduced.grade_pre_floor.clone(),
        summary: reduced.summary.clone(),
        findings: reduced.findings.clone(),
        is_fail_safe: false,
        fail_safe_reason: None,
    };

    // Telemetry: surface the map-reduce model and the AGGREGATE token/cost usage
    // summed across every map-stage call plus the synthesis call (#1885). The
    // unified path reads these straight off its single LlmResponse; on this path
    // they were never populated, which left `result.output_tokens == 0` and made
    // the shallow-review heuristic (wired below in `fold_reduced_into_result`)
    // silently inoperative on exactly the largest, highest-risk diffs.
    result.model = input.reviewer_model.clone();
    result.input_tokens = reduced.tokens.input_tokens;
    result.output_tokens = reduced.tokens.output_tokens;
    result.cost_estimate_usd = reduced.tokens.cost_usd;

    // Narrative body: use the synthesis prose summary when available (#1663);
    // fall back to the deterministic stats string when synthesis is disabled.
    let stats_body = format!(
        "Map-reduce review: {} file(s) reviewed across {} unit(s), \
         {} skipped, {} failed; {} finding(s) surfaced.",
        reduced.stats.files_reviewed,
        reduced.stats.units_total,
        reduced.stats.files_skipped,
        reduced.stats.files_failed,
        reduced.stats.findings_surfaced,
    );
    result.review_body = if !reduced.summary.is_empty() {
        format!("{}\n\n{}", reduced.summary, stats_body)
    } else {
        stats_body
    };

    let degraded_reason = run.degraded_reason.clone();
    fold_reduced_into_result(
        config,
        input,
        deps,
        &mut result,
        parsed,
        &run,
        synthesis_active,
    )
    .await;

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
/// Why: the reduce output must go through the severity floor, coverage floor,
/// verification round, grade clamp, and inline-comment mapping so the verdict
/// policy is applied consistently.  When `synthesis_active` is `true` (#1663),
/// the synthesis pass has already applied the High-severity safety floor and we
/// MUST NOT re-apply `apply_grade_and_floor` (which would re-add the count-based
/// `≥2 Medium → REQUEST_CHANGES` floor that synthesis is calibrating away).
/// Instead we use the already-floored `parsed.verdict` directly and parse the
/// grade from `parsed.grade`, bypassing `derive_verdict_with_grade`.
/// What: mirrors `run_review` steps 7b–7e against the `parsed` input.
///
///   - `synthesis_active=false` → full `apply_grade_and_floor` (mechanical path).
///   - `synthesis_active=true`  → synthesis-floored verdict + grade used directly.
///
/// Test: covered by the map-reduce runner integration tests.
async fn fold_reduced_into_result(
    config: &ReviewConfig,
    input: &ReviewInput,
    deps: &ReviewDeps,
    result: &mut ReviewResult,
    parsed: ParsedReview,
    run: &MapReduceRun,
    synthesis_active: bool,
) {
    // Derive (final_verdict, final_grade, original_llm_grade) depending on path.
    //
    // For the synthesis path (#1663 / #1665 item 3):
    //   - `final_grade`        = grade for the FLOORED verdict (post-floor).
    //   - `original_llm_grade` = grade from the LLM's RAW synthesis verdict
    //                            (pre-floor), threaded in via `ReducedReview::grade_pre_floor`.
    //   When the two-tier floor (#1665) changed the verdict, the two grades differ
    //   and downstream telemetry can detect the flooring.  When no floor fired,
    //   both are equal (same behaviour as before #1665).
    //
    // Both grades are Option<Grade> (#1474 parity): None for an UNKNOWN verdict.
    // The synthesis path is always a real verdict (not UNKNOWN), so Some() wraps
    // the concrete grades to keep the type consistent with apply_grade_and_floor.
    let (final_verdict, final_grade, original_llm_grade) = if synthesis_active {
        // Synthesis path (#1663): verdict is already floored by apply_synthesis_floor.
        // Re-applying derive_verdict_with_grade would wrongly re-add the count
        // floor.  Use the synthesis verdict + grade directly instead.
        let final_grade: Grade = parsed
            .grade
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| default_grade_for_verdict(&parsed.verdict));
        // Pre-floor grade for telemetry: use grade_pre_floor when available;
        // fall back to final_grade when no flooring occurred (they'll be equal).
        let pre_floor_grade: Grade = parsed
            .grade_pre_floor
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(final_grade);
        (
            parsed.verdict.clone(),
            Some(final_grade),
            Some(pre_floor_grade),
        )
    } else {
        // Mechanical path: apply the full severity floor via apply_grade_and_floor.
        // Returns Option<Grade> for each — None when the verdict is UNKNOWN (#1474).
        apply_grade_and_floor(&parsed)
    };

    // Coverage floor (no-op when coverage gating disabled).
    let (final_verdict, _cov_grade) = if let Some(ref cov) = run.coverage_contrib {
        apply_coverage_floor(final_verdict, final_grade, cov)
    } else {
        (final_verdict, final_grade)
    };

    let mut findings = parsed.findings;
    // Verification round — re-derives the verdict from surviving findings.  The
    // verifier sees the RAW diff so it can check any finding's location.
    // Pass the caller-supplied PR description + discussion as author rationale
    // (#1618) so the adversarial verifier can REFUTE a finding the author has
    // already empirically addressed.  `build_author_rationale` returns None when
    // neither is present, leaving the verifier prompt unchanged for existing callers.
    let author_rationale = build_author_rationale(
        input.caller_context.pr_description.as_deref(),
        input.caller_context.pr_discussion.as_deref(),
    );
    result.verdict = maybe_verify(
        config,
        deps.verifier.as_ref(),
        &run.raw_diff,
        final_verdict,
        &mut findings,
        author_rationale.as_deref(),
    )
    .await;
    result.findings = findings;

    // Envelope grade: clamp the original (pre-floor) grade to the post-
    // verification verdict (closes #1486 parity with the unified path).
    // None when UNKNOWN (#1474 parity) — never emits "F" for un-reviewable diffs.
    result.grade =
        original_llm_grade.map(|g| clamp_grade_to_verdict(g, &result.verdict).to_string());

    // Inline per-line comments from the RAW diff (#1414 parity).
    attach_inline_comments(result, &run.raw_diff);

    // Shallow-review flag (#1877 / #1885) — now wired on the map-reduce path.
    //
    // Historically this path never populated `result.output_tokens` (it stayed 0),
    // so the heuristic could not run here without false-positiving on every large
    // clean review. Issue #1885 closes that gap: the aggregate token total is now
    // summed across all map + synthesis calls (`reduced.tokens`) and set on
    // `result` by `run_mapreduce_branch` BEFORE this fold. We therefore apply the
    // SAME `is_shallow_clean_review` / `cap_shallow_review_grade` logic the unified
    // path uses (`runner.rs` step 7d-post), so a suspiciously cheap clean APPROVE
    // on a huge diff is flagged and grade-capped regardless of which path produced
    // it. `filtered.filtered_byte_size` is the map-reduce analog of the unified
    // path's rendered `diff.len()` — the char count actually sent to the reviewers,
    // which the aggregate token spend is proportional to.
    apply_shallow_review_flag(result, run.filtered.filtered_byte_size);
}

/// Flag and grade-cap a suspiciously shallow clean review on the map-reduce path.
///
/// Why: map-reduce handles the LARGEST diffs, exactly the population the #1877
/// heuristic targets; leaving it unwired (the #1885 gap) meant a fast/cheap
/// rubber-stamp APPROVE on a huge diff kept an unearned top grade. Extracting the
/// wiring into a helper keeps `fold_reduced_into_result` readable and gives the
/// behaviour a direct unit-test seam.
/// What: runs `is_shallow_clean_review` against the post-verification verdict,
/// empty-findings check, the filtered diff length, and the aggregate
/// `result.output_tokens`; when it fires, sets `result.shallow_clean_review` and
/// caps `result.grade` at B- via `cap_shallow_review_grade` (leaving the APPROVE
/// verdict untouched — this is a confidence signal, not a severity judgement).
/// Test: `run_review_mapreduce_shallow_clean_flags_low_tokens`,
/// `run_review_mapreduce_substantive_review_not_flagged` (runner_mapreduce_tests.rs).
fn apply_shallow_review_flag(result: &mut ReviewResult, filtered_byte_size: usize) {
    result.shallow_clean_review = crate::pipeline::letter_grade::is_shallow_clean_review(
        &result.verdict,
        result.findings.is_empty(),
        filtered_byte_size,
        result.output_tokens,
    );
    if !result.shallow_clean_review {
        return;
    }
    warn!(
        verdict = %result.verdict,
        diff_len = filtered_byte_size,
        output_tokens = result.output_tokens,
        cost_usd = result.cost_estimate_usd,
        "flagged shallow clean review on map-reduce path — zero findings with \
         implausibly low aggregate token spend for this diff size (#1885); grade capped at B-"
    );
    if let Some(g) = result
        .grade
        .as_deref()
        .and_then(|s| s.parse::<Grade>().ok())
    {
        result.grade = Some(crate::pipeline::letter_grade::cap_shallow_review_grade(g).to_string());
    }
}

#[cfg(test)]
#[path = "runner_mapreduce_tests.rs"]
mod tests;

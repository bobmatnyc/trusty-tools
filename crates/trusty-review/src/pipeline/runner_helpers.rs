//! Helper functions for the review runner (extracted from runner.rs).
//!
//! Why: extracted from `runner.rs` to keep that file under the 500-line cap
//! (#610) after the coverage-gating additions in #1014.  All functions here
//! are small, cohesive helpers called exactly once by `run_review`.
//!
//! What: grade derivation, GitHub PR metadata fetch, abort-dry, and finalise-run.
//!
//! Test: covered transitively by runner integration tests.

use std::sync::Arc;

use tracing::warn;

use crate::integrations::github::{
    AuthStrategy, CommentableLines, GithubClient, GithubError, RunMode, build_inline_plan,
    fetch_pr_metadata,
};
use crate::{
    config::ReviewConfig,
    models::{InlineCommentOut, ReviewResult, Verdict},
    pipeline::{
        grade::derive_verdict_with_grade,
        letter_grade::default_grade_for_verdict,
        output::{print_review_result, write_review_log},
        post::{PostContext, finalize_review},
        prompt::ReviewPrMeta,
    },
    store::DedupStore,
};

use super::runner::{ReviewDeps, ReviewInput};

/// Derive (verdict, floor_grade, original_llm_grade) from a `ParsedReview`.
///
/// Why: extracted to keep `run_review` under the line cap and make it testable.
/// Also returns the original LLM grade (pre-floor) so the runner can re-derive
/// the envelope grade AFTER verification by clamping the original grade to the
/// post-verification verdict (fixes #1486: the floor-escalated grade must not
/// survive verification relaxation).
///
/// What: fail-safe → `(verdict, grade, grade)` where an UNKNOWN fail-safe carries
/// `None` for BOTH grades (un-reviewable ⇒ no letter grade, #1474) and any other
/// fail-safe verdict carries its default grade; normal → resolves LLM grade string
/// (or default), calls `derive_verdict_with_grade` for max(grade, model) + floor.
/// Both `floor_grade` and `original_llm_grade` are `None` whenever the diff was
/// un-reviewable (verdict UNKNOWN), so the post-verification envelope grade (step
/// 7d) also resolves to `None` — never an "F" (#1474).
/// Returns `(floor_verdict, floor_grade, original_llm_grade)`.
/// Test: covered by runner integration tests and the #1486 regression test.
pub(super) fn apply_grade_and_floor(
    parsed: &crate::pipeline::parser::ParsedReview,
) -> (
    Verdict,
    Option<crate::pipeline::letter_grade::Grade>,
    Option<crate::pipeline::letter_grade::Grade>,
) {
    if parsed.is_fail_safe {
        let v = parsed.verdict.clone();
        // An UNKNOWN fail-safe is un-reviewable — no letter grade (#1474).
        let g = (v != Verdict::Unknown).then(|| default_grade_for_verdict(&v));
        return (v, g, g);
    }
    // An UNKNOWN verdict is un-reviewable — suppress the LLM grade entirely so the
    // post-verification clamp (step 7d) yields `None`, not the LLM's grade (#1474).
    let original_llm_grade = (parsed.verdict != Verdict::Unknown).then(|| {
        parsed
            .grade
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                let g = default_grade_for_verdict(&parsed.verdict);
                warn!(
                    verdict = %parsed.verdict,
                    default_grade = %g,
                    "LLM grade absent or unparseable — using default for verdict"
                );
                g
            })
    });
    // `derive_verdict_with_grade` returns `None` for an UNKNOWN verdict; for any
    // real verdict it returns `Some(grade)`.  Feed it the resolved LLM grade (or a
    // conservative default for the UNKNOWN case, which it ignores).
    let grade_input =
        original_llm_grade.unwrap_or_else(|| default_grade_for_verdict(&parsed.verdict));
    let (floor_verdict, floor_grade) =
        derive_verdict_with_grade(parsed.verdict.clone(), grade_input, &parsed.findings);
    (floor_verdict, floor_grade, original_llm_grade)
}

/// Fetch PR metadata and return `(ReviewPrMeta, head_sha)`.
///
/// Why: centralises the GitHub API call and head-SHA surfacing so the runner
/// can key the dedup store.
/// What: resolves token via run_mode, calls `fetch_pr_metadata`.
/// Test: tested indirectly via mock in integration tests.
pub(super) async fn fetch_github_pr_meta(
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    pr: u64,
    run_mode: RunMode,
) -> Result<(ReviewPrMeta, String), GithubError> {
    let client = GithubClient::new()?;
    let token = AuthStrategy::select(run_mode, None)
        .resolve_token(&client, config, owner)
        .await?;
    let meta = fetch_pr_metadata(&client, owner, repo, pr, &token).await?;
    let head_sha = meta.head.sha.clone();
    Ok((
        ReviewPrMeta {
            title: meta.title,
            // Fix 3 (#599): thread the PR description through so the external
            // context sources can scan it for ticket keys + fold it into queries.
            body: meta.body.unwrap_or_default(),
            author: meta.user.login,
            url: meta.html_url,
        },
        head_sha,
    ))
}

/// Finalise an *aborted* review as dry-run only, releasing the dedup claim.
///
/// Why: a review that aborts before producing a real verdict (diff-load failure
/// or LLM transport error) must never be posted live — it carries only a
/// fail-safe APPROVE/UNKNOWN.  It must also *release* its dedup claim so a later
/// retry (e.g. once the LLM recovers) can re-run instead of being suppressed.
/// What: releases the in-progress dedup claim (fail-safe on error), writes the
/// dry-run log so the failure is inspectable, prints when requested, and returns
/// the result flagged `dry_run = true`.
/// Test: `run_review_fail_safe_on_llm_error`, `run_review_missing_diff_file_sets_error`.
pub(super) fn abort_dry(
    mut result: ReviewResult,
    config: &ReviewConfig,
    input: &ReviewInput,
    deps: &ReviewDeps,
) -> ReviewResult {
    result.dry_run = true;
    // Release the in-progress claim so a retry can re-run this head SHA.
    if !result.head_sha.is_empty()
        && let Some(store) = deps.dedup.as_ref()
        && let Err(e) = store.release(
            &result.owner,
            &result.repo,
            result.pr_number,
            &result.head_sha,
        )
    {
        warn!("dedup release() after abort failed (non-fatal): {e}");
    }
    if input.write_log {
        write_review_log(&result, &config.log_dir);
    }
    if input.print_result {
        print_review_result(&result);
    }
    result
}

/// Build inline per-line comments from the raw diff and attach to the result (#1414).
///
/// Why: findings reach posting with only `file` + `line`; to post them as inline
/// review comments (or, in dry-run, preview them) the runner must map each finding
/// to a commentable diff line and divert off-diff findings to the summary body.
/// Computing this here — where the raw, unfiltered diff is available — is the only
/// place that knows the true PR diff positions GitHub will accept.
/// What: parses `raw_diff` into a `CommentableLines` index, builds the inline plan
/// from `result.findings`, and stores the inline comments as `InlineCommentOut` on
/// the result plus the suppressed-nit count (#1420).  Findings that fall back to the
/// summary stay in `result.findings` (the body renders the non-inline ones).  A
/// no-op when there are no findings.
/// Test: `attach_inline_comments_maps_on_diff` (runner_tests.rs).
pub(super) fn attach_inline_comments(result: &mut ReviewResult, raw_diff: &str) {
    if result.findings.is_empty() {
        return;
    }
    let commentable = CommentableLines::from_unified_diff(raw_diff);
    let plan = build_inline_plan(&result.findings, &commentable);
    result.suppressed_nits = plan.suppressed_nits;
    // Carry the authoritative inline/summary partition by finding identity so the
    // summary body never drops a finding that shares a (file, line) with an inline
    // one (#1414 silent-omission fix).
    result.inline_finding_indices = plan.inline_indices;
    result.inline_comments = plan
        .comments
        .into_iter()
        .map(|c| InlineCommentOut {
            path: c.path,
            line: c.line,
            body: c.body,
        })
        .collect();
}

/// Apply post-or-log finalisation (Phase 1, #582) for a completed review.
///
/// Why: single exit path so live/dry policy is applied exactly once.
/// What: builds `PostContext` from result fields, delegates to `finalize_review`.
/// Test: `post::tests` cover branch selection; runner tests assert dry-run.
pub(super) async fn finalize_run(
    result: ReviewResult,
    config: &ReviewConfig,
    input: &ReviewInput,
    dedup: Option<&Arc<DedupStore>>,
) -> ReviewResult {
    // Clone the dedup-key fields up front so `result` can be moved into
    // `finalize_review` while `PostContext` borrows the owned copies.
    let owner = result.owner.clone();
    let repo = result.repo.clone();
    let pr = result.pr_number;
    let head_sha = result.head_sha.clone();
    let post_ctx = PostContext {
        owner: &owner,
        repo: &repo,
        pr,
        head_sha: &head_sha,
        run_mode: input.run_mode,
        dedup,
    };
    finalize_review(
        result,
        config,
        input.trigger,
        input.allow_posting,
        input.write_log,
        input.print_result,
        post_ctx,
    )
    .await
}

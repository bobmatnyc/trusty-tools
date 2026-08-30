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

use tracing::{error, warn};

use crate::integrations::github::{
    AuthStrategy, CommentableLines, GithubClient, GithubError, RunMode, build_inline_plan,
    fetch_pr_metadata,
};
use crate::{
    config::ReviewConfig,
    models::{InlineCommentOut, ReviewResult, Verdict},
    pipeline::{
        diff::DiffSource,
        grade::derive_verdict_with_grade,
        letter_grade::default_grade_for_verdict,
        output::{print_review_result, write_review_log},
        post::{PostContext, finalize_review},
        prompt::ReviewPrMeta,
    },
    store::DedupStore,
};

use super::runner::{ReviewDeps, ReviewInput};

/// Combine caller-supplied PR description + discussion into a single author-
/// rationale block for the adversarial verifier (#1618).
///
/// Why: the verifier can refute a finding the author has already empirically
/// addressed (e.g. "checked the data source; no values exceed X; contract owner
/// agreed"), but only if it sees the author's own words.  The reviewer renders
/// description and discussion as separate sections; the verifier just needs the
/// combined prose as context, so we fold them into one labelled block here.
/// What: returns `None` when BOTH inputs are absent/blank (verifier prompt stays
/// unchanged for existing callers); otherwise returns the present pieces joined
/// under `## PR Description` / `## PR Discussion / Author Rationale` sub-headings.
/// Provider-agnostic — just the caller's free-form prose.
/// Test: `build_author_rationale_*` (runner_tests.rs) and
/// `verify_request_includes_author_rationale` (verify_prompt_tests.rs).
pub(super) fn build_author_rationale(
    pr_description: Option<&str>,
    pr_discussion: Option<&str>,
) -> Option<String> {
    let desc = pr_description.map(str::trim).filter(|t| !t.is_empty());
    let disc = pr_discussion.map(str::trim).filter(|t| !t.is_empty());
    if desc.is_none() && disc.is_none() {
        return None;
    }
    let mut out = String::new();
    if let Some(d) = desc {
        out.push_str("## PR Description\n\n");
        out.push_str(d);
        out.push_str("\n\n");
    }
    if let Some(d) = disc {
        out.push_str("## PR Discussion / Author Rationale\n\n");
        out.push_str(d);
        out.push('\n');
    }
    Some(out.trim_end().to_string())
}

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

/// Resolve the GitHub token for a diff-fetch, replacing an empty placeholder.
///
/// Why: service-path callers (`resolve_diff_source` in `service/handlers.rs`,
/// and the webhook dispatcher in `service/webhook.rs`) intentionally build
/// `DiffSource::Github` with an empty `token` field, commenting that "the
/// pipeline will resolve it from config" — but nothing ever did that
/// resolution, so serve-mode diff fetches sent an empty bearer token straight
/// to GitHub and got back an opaque `401 Bad credentials` (#1880). This is the
/// single funnel that closes that gap, mirroring the token resolution already
/// performed independently for PR-metadata fetch (`fetch_github_pr_meta`
/// above) and for posting (`pipeline::post::finalize_review`).
/// What: `LocalFile` sources need no GitHub credentials and pass through
/// unchanged. `Github` sources with an already-resolved (non-empty) token —
/// the CLI `run`/`compare`/`calibrate` paths, which resolve up front — also
/// pass through unchanged so this never triggers a redundant token exchange.
/// Only a `Github` source with an *empty* token triggers resolution via
/// `AuthStrategy::select(run_mode, None).resolve_token(...)`, returning a new
/// `DiffSource::Github` with the resolved token. If no token can be resolved
/// (no PAT, no `gh` login, no App credentials, or no installation for the
/// owner) this returns the underlying `GithubError` — whose `Display` already
/// names the missing env var / config key — so the caller fails CLOSED with an
/// actionable message instead of silently sending an empty-token request.
/// Test: `resolve_diff_token_passes_through_local_file`,
/// `resolve_diff_token_passes_through_already_resolved_token`,
/// `resolve_diff_token_serve_mode_without_app_creds_errors`,
/// `resolve_diff_token_cli_mode_resolves_from_config_token`.
pub(super) async fn resolve_diff_token(
    source: &DiffSource,
    config: &ReviewConfig,
    run_mode: RunMode,
) -> Result<DiffSource, GithubError> {
    let DiffSource::Github {
        owner,
        repo,
        pr,
        token,
    } = source
    else {
        return Ok(source.clone());
    };
    if !token.is_empty() {
        return Ok(source.clone());
    }

    let client = GithubClient::new()?;
    let resolved = AuthStrategy::select(run_mode, None)
        .resolve_token(&client, config, owner)
        .await?;
    Ok(DiffSource::Github {
        owner: owner.clone(),
        repo: repo.clone(),
        pr: *pr,
        token: resolved,
    })
}

/// Whether an aborting review holds the dedup claim for its head SHA (#5064).
///
/// Why: `abort_dry` releases the claim so a retry can re-run the SHA. That is
/// correct only when this process acquired it. A review aborting *because* the
/// claim failed holds nothing — releasing there removes whatever record is on
/// disk, including a `Completed` one written by another process, which reopens
/// the duplicate-comment hole the abort exists to close.
/// What: `Held` releases the claim; `NotHeld` leaves the store untouched.
/// Test: `failed_claim_abort_does_not_delete_another_processes_record`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DedupClaim {
    /// This review acquired the claim; the abort must release it.
    Held,
    /// This review never acquired the claim; the abort must not write.
    NotHeld,
}

/// Mark a review aborted because it has no head SHA to key its dedup claim to.
///
/// Why: the claim in `run_review` and the `complete()` in `finalize_review` are
/// both keyed on the head SHA, and `decide_action` never reads it — so a failed
/// `fetch_github_pr_meta` used to leave an empty SHA and still reach
/// `FinalizeAction::Post`, posting with the claim never taken and never
/// completed (#6062). The caller decides the post path is reachable and then
/// calls this; it lives here to keep `runner.rs` under its SLOC cap.
/// What: logs the abort and writes the verdict and the operator-facing reason
/// onto `result`, appending the metadata-fetch error when there is one so the
/// message names the cause as well as the consequence. The caller still owns
/// the `abort_dry` exit, which releases nothing (the claim was never held).
/// Test: `run_review_empty_head_sha_fails_closed_before_posting`,
/// `run_review_serve_mode_empty_token_fails_closed_with_actionable_error`.
pub(super) fn mark_no_head_sha_abort(result: &mut ReviewResult, meta_error: Option<&str>) {
    error!(
        owner = %result.owner,
        repo = %result.repo,
        pr = result.pr_number,
        "PR metadata carried no head SHA and posting is enabled — aborting without posting (#6062)"
    );
    result.verdict = Verdict::Unknown;
    let cause = meta_error
        .map(|e| format!("; PR metadata fetch failed: {e}"))
        .unwrap_or_default();
    result.error = Some(format!(
        "not reviewed: the PR metadata fetch produced no head SHA, so the dedup claim cannot \
         be keyed and a re-run could post a duplicate comment (#6062){cause}"
    ));
}

/// Finalise an *aborted* review as dry-run only, releasing the dedup claim.
///
/// Why: a review that aborts before producing a real verdict (diff-load failure
/// or LLM transport error) must never be posted live — it carries only a
/// fail-safe APPROVE/UNKNOWN.  It must also *release* its dedup claim so a later
/// retry (e.g. once the LLM recovers) can re-run instead of being suppressed.
/// What: syncs `findings_count` to `findings.len()` (#1877), releases the
/// in-progress dedup claim when `claim` is `Held` (fail-safe on error), writes
/// the dry-run log so the failure is inspectable, prints when requested, and
/// returns the result flagged `dry_run = true`.
/// Test: `run_review_fail_safe_on_llm_error`, `run_review_missing_diff_file_sets_error`,
/// `findings_count_matches_len_on_abort`,
/// `failed_claim_abort_does_not_delete_another_processes_record`.
pub(super) async fn abort_dry(
    mut result: ReviewResult,
    config: &ReviewConfig,
    input: &ReviewInput,
    deps: &ReviewDeps,
    claim: DedupClaim,
) -> ReviewResult {
    result.dry_run = true;
    // #1877: keep the authoritative findings_count in sync at this canonical
    // early-abort exit point, regardless of which guard triggered the abort.
    result.findings_count = result.findings.len();
    // #4459: the same sync for the unverified count, so an aborted run reports
    // it too rather than leaving a stale zero.
    result.unverified_count = crate::pipeline::post::count_unverified(&result.findings);
    // Release the in-progress claim so a retry can re-run this head SHA.
    // #5064: only when this review actually acquired it — see `DedupClaim`.
    if claim == DedupClaim::Held
        && !result.head_sha.is_empty()
        && let Some(store) = deps.dedup.as_ref()
        && let Err(e) = store
            .release(
                &result.owner,
                &result.repo,
                result.pr_number,
                &result.head_sha,
            )
            .await
    {
        // Non-fatal: nothing was posted, and the InProgress record ages out.
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

/// Run every output-hygiene and grounding pass over a freshly parsed review.
///
/// Why: four passes must all run BEFORE grading, in this order, and they moved
/// here together when the #1873 pass pushed `runner.rs` over the 500-SLOC cap.
/// Keeping them in one function is also what keeps the unified path's ordering
/// visible beside the map-reduce path's, which runs the same four in
/// `mapreduce/mod.rs`.
/// What, in order:
///   1. `finding_hygiene::sanitize_findings` — drop self-negated / CoT-leaking
///      findings and demote the speculative ones (#4043, #4044, #4081, #5309).
///   2. `citation_check::enforce_citation_integrity` — drop any finding whose
///      cited path or quoted content the diff does not contain (#2881, #4042).
///   3. `absence_claim::drop_refuted_absence_claims` — drop any finding whose
///      premise is that a file is missing from a diff that contains it (#1873).
///   4. `finding_hygiene::relax_verdict_if_evidence_wiped` — when 1–3 removed
///      every finding, the model's own verdict rested on the same evidence and
///      is relaxed with it (#4042, #4044).
///
/// Test: `run_review_outer_and_embedded_verdict_agree_after_severity_floor`,
/// `unified_path_emits_no_finding_citing_a_path_outside_the_diff`.
pub(super) fn ground_parsed_findings(
    parsed: &mut crate::pipeline::parser::ParsedReview,
    filtered: &crate::pipeline::diff_analyzer::models::FilteredDiff,
) {
    let findings_before = parsed.findings.len();
    crate::pipeline::finding_hygiene::sanitize_findings(&mut parsed.findings);

    let cite_index = crate::pipeline::citation_check::DiffContentIndex::from_filtered(filtered);
    crate::pipeline::citation_check::enforce_citation_integrity(&mut parsed.findings, &cite_index);
    crate::pipeline::absence_claim::drop_refuted_absence_claims(&mut parsed.findings, &cite_index);

    crate::pipeline::finding_hygiene::relax_verdict_if_evidence_wiped(
        &mut parsed.verdict,
        &mut parsed.grade,
        findings_before,
        &parsed.findings,
    );
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Split into a sibling file to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "runner_helpers_tests.rs"]
mod tests;

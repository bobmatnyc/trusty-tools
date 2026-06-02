//! Review pipeline runner — the top-level orchestration loop.
//!
//! Why: wires together diff loading, context retrieval, prompt construction,
//! LLM call, parsing, and (Phase 1, #582) the live-post-or-dry-run-log decision
//! into a single `run_review` function that the CLI `run`/`compare` commands and
//! the webhook service all call.
//!
//! What: `run_review` runs the pipeline (diff → context → LLM → parse → grade)
//! then either posts a GitHub PR review comment (live) or writes a dry-run log,
//! gated by the trigger decision and the SHA-keyed dedup store.  Returns a
//! `ReviewResult` even on pipeline errors (fail-safe APPROVE/UNKNOWN).
//!
//! Phase 2 (#583) adds the per-finding verification round between verdict parse
//! and finalisation: candidate findings are confirmed/refuted by the verifier
//! model and the verdict is re-derived so refuted blocking findings relax it.
//!
//! Deferred to later phases (stubs/comments intact):
//!  - Suppression filtering + per-repo `.github/code-intelligence.yml` (Phase 3 / #584)
//!  - Tracker-issue upsert (Phase 4 / #585)
//!  - JIRA/Confluence/APEX/GH-Issues context (Phase 6 / #550)
//!  - Multi-pass / enrichment rounds
//!
//! Test: `run_review_with_fake_provider_approves`,
//! `run_review_fail_safe_on_llm_error`,
//! `run_review_local_diff_skips_github`,
//! `run_review_dedup_skips_completed`.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::{
    config::ReviewConfig,
    integrations::{
        analyze_client::AnalyzeClient,
        github::{AuthStrategy, GithubClient, GithubError, RunMode, fetch_pr_metadata},
        search_client::SearchClient,
    },
    llm::LlmProvider,
    models::{ReviewResult, Verdict},
    pipeline::{
        diff::{DiffSource, extract_changed_files, extract_identifiers, load_diff, truncate_diff},
        grade::derive_verdict,
        output::{print_review_result, write_review_log},
        parser::parse_review_response,
        post::{PostContext, finalize_review},
        prompt::{ReviewPrMeta, build_review_prompt},
        runner_context::gather_context,
        trigger::TriggerDecision,
        verify::maybe_verify,
    },
    store::{ClaimOutcome, DedupStore},
};

// ─── Pipeline input ───────────────────────────────────────────────────────────

/// All inputs for a single review run.
///
/// Why: grouping the inputs into a struct avoids long function signatures and
/// makes the `compare` subcommand easy to implement (same input, multiple
/// models).
/// What: contains the diff source, config reference, model override, and
/// injected service clients.
/// Test: used directly by all runner tests.
pub struct ReviewInput {
    /// Where to obtain the diff (GitHub PR or local file).
    pub diff_source: DiffSource,
    /// Reviewer model id (may differ from config default in `compare` mode).
    pub reviewer_model: String,
    /// Whether to actually write the log file (false in `compare` mode to
    /// avoid cluttering the log dir with partial results).
    pub write_log: bool,
    /// Print the result to STDOUT after the run.
    pub print_result: bool,
    /// Trigger override deciding live-post vs dry-run (Phase 1, #582 / REV-703).
    ///
    /// `None` (the default) means "defer to the global `config.dry_run` flag";
    /// the webhook handler sets `ForceLive`/`ForceDryRun` from the requested
    /// reviewer.  CLI `run`/`compare` leave this `None` (and `compare` stays
    /// dry-run because it never enables posting).
    pub trigger: TriggerDecision,
    /// Run mode that selects the GitHub auth strategy (CLI=PAT/`gh`, Serve=App).
    ///
    /// Determines how the runner resolves a token for posting / metadata fetch.
    pub run_mode: RunMode,
    /// Whether the runner is allowed to post live at all.
    ///
    /// Why: a safety belt independent of the trigger — `compare` and
    /// `--local-diff` set this `false` so they can never post even if a trigger
    /// or config somehow forces live.  `run`/`serve` set it `true`.
    pub allow_posting: bool,
}

/// Injected service dependencies (trait objects for testability).
///
/// Why: the pipeline calls trusty-search and trusty-analyze via trait objects
/// so tests can inject fakes without a running daemon.
/// What: all fields are `Arc<dyn Trait>` for cheap cloning in `compare` mode.
/// Test: `run_review_with_fake_provider_approves`.
pub struct ReviewDeps {
    /// LLM provider for the reviewer role.
    pub llm: Arc<dyn LlmProvider>,
    /// LLM provider for the verifier role (Phase 2, #583).  `None` disables the
    /// verification round (e.g. tests that don't exercise it, or when
    /// `config.verification.enabled` is false the caller passes `None`).
    pub verifier: Option<Arc<dyn LlmProvider>>,
    /// Code search client (required; gracefully degrades on error).
    pub search: Arc<dyn SearchClient>,
    /// Static analysis client (optional; None skips the analyze step).
    pub analyze: Option<Arc<dyn AnalyzeClient>>,
    /// SHA-keyed dedup store (Phase 1, #582).  `None` disables dedup (e.g.
    /// `compare`, `--local-diff`, or tests that don't exercise it).  Store
    /// errors are fail-safe: logged, never fatal.
    pub dedup: Option<Arc<DedupStore>>,
}

// ─── Main runner ──────────────────────────────────────────────────────────────

/// Run the MVP review pipeline for a single PR / diff.
///
/// Why: the single entry point used by both the CLI `run` and `compare`
/// subcommands; ensures both take the same code path.
/// What: loads the diff, gathers context, builds the prompt, calls the LLM,
/// parses the response, and writes the log.  Returns a `ReviewResult` even
/// on pipeline errors (fail-safe: verdict = APPROVE with an `error` field set).
/// Test: `run_review_with_fake_provider_approves`, `run_review_fail_safe_on_llm_error`.
pub async fn run_review(
    config: &ReviewConfig,
    input: ReviewInput,
    deps: ReviewDeps,
) -> ReviewResult {
    // ── Step 1: determine owner/repo/pr from diff source ──────────────────
    let (owner, repo, pr_number, is_local) = match &input.diff_source {
        DiffSource::Github {
            owner, repo, pr, ..
        } => (owner.clone(), repo.clone(), *pr, false),
        DiffSource::LocalFile { path } => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("local");
            ("local".to_string(), stem.to_string(), 0_u64, true)
        }
    };

    let pr_url = if !is_local {
        format!("https://github.com/{owner}/{repo}/pull/{pr_number}")
    } else {
        String::new()
    };

    // ── Step 2: fetch PR metadata (skip for local-diff mode) ──────────────
    let (pr_meta, head_sha): (ReviewPrMeta, String) = if is_local {
        (ReviewPrMeta::default(), String::new())
    } else {
        match fetch_github_pr_meta(config, &owner, &repo, pr_number, input.run_mode).await {
            Ok((m, sha)) => (m, sha),
            Err(e) => {
                warn!("failed to fetch PR metadata: {e} — using empty metadata");
                (
                    ReviewPrMeta {
                        title: format!("PR #{pr_number}"),
                        author: String::new(),
                        url: pr_url.clone(),
                    },
                    String::new(),
                )
            }
        }
    };

    // Build a result skeleton with the PR identity filled in.
    let mut result = ReviewResult::new(
        owner.clone(),
        repo.clone(),
        pr_number,
        pr_meta.title.clone(),
        pr_url,
    );
    result.head_sha = head_sha.clone();

    // ── Step 2b: dedup claim (Phase 1, #582) ──────────────────────────────
    // Claim the (owner,repo,pr,head_sha) slot before doing expensive work.  A
    // completed claim for the same head SHA short-circuits the whole pipeline.
    // Store errors are fail-safe: we log and proceed (never block a review).
    if !is_local
        && !head_sha.is_empty()
        && let Some(store) = deps.dedup.as_ref()
    {
        match store.claim(&owner, &repo, pr_number, &head_sha) {
            Ok(ClaimOutcome::Skipped) => {
                info!(
                    owner = %owner,
                    repo = %repo,
                    pr = pr_number,
                    head_sha = %head_sha,
                    "dedup: a completed review already exists for this head SHA — skipping"
                );
                result.verdict = Verdict::Approve;
                result.error = Some("skipped: duplicate of a completed review".to_string());
                result.dry_run = true;
                return result;
            }
            Ok(ClaimOutcome::Claimed) => {
                debug!(head_sha = %head_sha, "dedup: claimed review slot");
            }
            Err(e) => {
                warn!("dedup claim failed (proceeding without dedup): {e}");
            }
        }
    }

    // ── Step 3: load and truncate diff ────────────────────────────────────
    let raw_diff = match load_diff(&input.diff_source).await {
        Ok(d) => d,
        Err(e) => {
            warn!("failed to load diff: {e}");
            result.error = Some(format!("diff load failed: {e}"));
            return abort_dry(result, config, &input, &deps);
        }
    };
    let diff = truncate_diff(&raw_diff);
    debug!(diff_chars = diff.len(), "diff loaded and truncated");

    // ── Step 4: extract identifiers for context retrieval ─────────────────
    let identifiers = extract_identifiers(&diff, 8);
    let changed_files = extract_changed_files(&diff);
    debug!(
        identifiers = ?identifiers,
        changed_files_count = changed_files.len(),
        "extracted identifiers from diff"
    );

    // ── Step 5: gather context in parallel ────────────────────────────────
    let context = gather_context(config, &deps, &identifiers, &changed_files, &pr_meta.title).await;

    // ── Step 6: build prompt and call LLM ─────────────────────────────────
    let llm_req = build_review_prompt(
        &owner,
        &repo,
        &pr_meta,
        &diff,
        &context,
        &input.reviewer_model,
    );
    debug!(model = %input.reviewer_model, "calling LLM reviewer");

    let llm_resp = match deps.llm.complete(llm_req).await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("LLM call failed: {e} — applying fail-safe APPROVE (spec REV-130)");
            result.verdict = Verdict::Approve;
            result.error = Some(format!("LLM error: {e}"));
            return abort_dry(result, config, &input, &deps);
        }
    };

    info!(
        model = %llm_resp.model,
        input_tokens = llm_resp.input_tokens,
        output_tokens = llm_resp.output_tokens,
        cost_usd = llm_resp.cost_usd,
        latency_ms = llm_resp.latency_ms,
        "LLM reviewer call complete"
    );
    result.apply_llm_response(&llm_resp);

    // ── Step 7: parse verdict + findings ──────────────────────────────────
    let parsed = parse_review_response(&llm_resp.text);
    if parsed.is_fail_safe {
        warn!(
            reason = ?parsed.fail_safe_reason,
            "verdict parsing fell back to fail-safe APPROVE"
        );
    }

    // ── Step 7b: apply severity-anchored floor (grading calibration) ───────
    // Derive the final verdict from (model-proposed, findings).  The floor
    // prevents the model from silently softening Critical/High issues to APPROVE*.
    // UNKNOWN is always preserved as-is (diff unassessable — no floor applies).
    let final_verdict = if parsed.is_fail_safe {
        // Fail-safe path: the parser couldn't extract findings, so we cannot
        // apply the severity floor.  Preserve the fail-safe APPROVE.
        parsed.verdict
    } else {
        derive_verdict(parsed.verdict, &parsed.findings)
    };

    info!(
        verdict = %final_verdict,
        findings_count = parsed.findings.len(),
        "final verdict after severity-anchored floor"
    );

    let mut findings = parsed.findings;

    // ── Step 7c: per-finding verification round (Phase 2, #583) ────────────
    // Confirm or refute candidate findings with the verifier model; refuted
    // findings are demoted below the advisory tier and the verdict is re-derived
    // so a BLOCK whose only blocking finding was refuted relaxes.  `maybe_verify`
    // applies the enabled / verifier-wired gating and returns the verdict
    // unchanged when the round is skipped.
    result.verdict = maybe_verify(
        config,
        deps.verifier.as_ref(),
        &diff,
        final_verdict,
        &mut findings,
    )
    .await;
    result.findings = findings;

    finalize_run(result, config, &input, deps.dedup.as_ref()).await
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Fetch PR metadata from GitHub and build a `ReviewPrMeta` plus the head SHA.
///
/// Why: centralises the GitHub API call and mapping from `PrMetadata` to the
/// lighter-weight `ReviewPrMeta` the prompt needs, and surfaces the head SHA so
/// the runner can key the dedup store.  The token is resolved through the
/// dual-mode auth abstraction (#582) so it works in both CLI and service modes.
/// What: selects the auth strategy from `run_mode`, resolves a token, and calls
/// `fetch_pr_metadata`; on any error the caller falls back to empty metadata.
/// Test: no real-network test; tested indirectly via mock in integration tests.
async fn fetch_github_pr_meta(
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    pr: u64,
    run_mode: RunMode,
) -> Result<(ReviewPrMeta, String), GithubError> {
    let client = GithubClient::new();
    let token = AuthStrategy::select(run_mode, None)
        .resolve_token(&client, config, owner)
        .await?;
    let meta = fetch_pr_metadata(&client, owner, repo, pr, &token).await?;
    let head_sha = meta.head.sha.clone();
    Ok((
        ReviewPrMeta {
            title: meta.title,
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
fn abort_dry(
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

/// Apply the post-or-log finalisation for a completed review.
///
/// Why: the success exit path of `run_review` must go through the same
/// post-or-log decision (Phase 1, #582) so the live/dry policy and fail-safe
/// error handling are applied exactly once and consistently.
/// What: reads the PR coordinates + head SHA off the result, builds a
/// `PostContext`, and delegates to `pipeline::post::finalize_review`, threading
/// the trigger decision, the `allow_posting` belt, the run mode, and the
/// optional dedup store.
/// Test: branch selection is covered by `post::tests`; runner tests assert the
/// dry-run side effects.
async fn finalize_run(
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

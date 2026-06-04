//! Review pipeline runner — top-level orchestration loop.
//!
//! Why: single entry point for CLI `run`/`compare` and the webhook service.
//! What: diff → context gate (#590) → context → LLM → parse → grade (#732)
//! → verify (#583) → post-or-log (#582).  Returns a `ReviewResult` on all paths.
//!
//! Deferred: suppression (#584), issue upsert (#585), multi-pass enrichment.
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
    models::{ReviewResult, ReviewStatus, Verdict},
    pipeline::{
        context_gate::{GateOutcome, degraded_banner, preflight_context},
        diff::{DiffSource, extract_changed_files, extract_identifiers, load_diff, truncate_diff},
        diff_analyzer::DiffAnalyzer, // noise filter (Stages A+B); #624
        grade::derive_verdict_with_grade,
        letter_grade::default_grade_for_verdict,
        output::{print_review_result, write_review_log},
        parser::parse_review_response,
        post::{PostContext, finalize_review},
        prompt::{ReviewPrMeta, build_review_prompt},
        runner_context::{gather_context, gather_external_context_md},
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
    /// Code search client.  REQUIRED by default (#590): the required-context
    /// gate (`preflight_context`) skips the review when search is unreachable
    /// unless the operator opted out via `config.context.require_search = false`.
    pub search: Arc<dyn SearchClient>,
    /// Static analysis client.  REQUIRED by default (#590): the gate skips the
    /// review when analyze is unreachable/absent unless the operator opted out
    /// via `config.context.require_analyze = false`.  `None` is treated as
    /// "analyze unavailable" by the gate (a hard skip when required).
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
/// What: loads the diff, runs the required-context gate (#590), gathers context,
/// builds the prompt, calls the LLM, parses the response, and writes the log.
/// When a required context dependency is unavailable the review is SKIPPED (no
/// LLM call, `status = Skipped`).  Returns a `ReviewResult` even on pipeline
/// errors (fail-safe: verdict = APPROVE with an `error` field set).
/// Test: `run_review_with_fake_provider_approves`, `run_review_fail_safe_on_llm_error`,
/// `run_review_search_down_skips_when_required`,
/// `run_review_search_down_degraded_when_optout`.
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
                        body: String::new(),
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

    // ── Step 3: load, filter (DiffAnalyzer Stages A+B), and truncate diff ─
    // truncate_diff is the final safety net after noise filtering (REV-209).
    let raw_diff = match load_diff(&input.diff_source).await {
        Ok(d) => d,
        Err(e) => {
            warn!("failed to load diff: {e}");
            result.error = Some(format!("diff load failed: {e}"));
            return abort_dry(result, config, &input, &deps);
        }
    };
    let filtered = DiffAnalyzer::default().analyze(&raw_diff).await;
    let max = crate::config::constants::MAX_DIFF_CHARS;
    let diff = truncate_diff(&filtered.render_for_prompt(max));
    debug!(orig = raw_diff.len(), filt = diff.len(), "diff filtered");

    // ── Step 4: extract identifiers for context retrieval ─────────────────
    let identifiers = extract_identifiers(&diff, 8);
    let changed_files = extract_changed_files(&diff);
    debug!(ids = ?identifiers, files = changed_files.len(), "extracted identifiers from diff");

    // ── Step 4b: required-context gate (#590) ─────────────────────────────
    // trusty-search AND trusty-analyze are REQUIRED by default.  If either is
    // unreachable, SKIP the review loudly (no LLM call, no post) instead of
    // producing a context-free, false-confidence verdict.  An operator who
    // explicitly opted a dependency out gets a DEGRADED, non-authoritative run.
    let degraded_reason: Option<String> = match preflight_context(config, &deps).await {
        GateOutcome::Proceed => None,
        GateOutcome::Skip(reason) => {
            warn!("required-context gate: skipping review — {reason}");
            result.status = ReviewStatus::Skipped;
            result.verdict = Verdict::Unknown;
            result.error = Some(reason);
            result.dry_run = true;
            // Return WITHOUT finalize_review so a skipped review is never posted.
            // Release any dedup claim so a retry (once the dep recovers) can re-run.
            return abort_dry(result, config, &input, &deps);
        }
        GateOutcome::Degraded(reason) => {
            warn!("required-context gate: proceeding DEGRADED (non-authoritative) — {reason}");
            result.status = ReviewStatus::Degraded;
            Some(reason)
        }
    };

    // ── Step 5: gather context in parallel (search/analyze/APEX + external) ──
    // All sources are FAIL-OPEN: errors contribute nothing, never block the review
    // (distinct from the #590 required gate above).  APEX (#550 PR-B) is gated by
    // config.apex_index: empty = disabled.
    let title = &pr_meta.title;
    let body = &pr_meta.body;
    let (context, external_context) = tokio::join!(
        gather_context(config, &deps, &identifiers, &changed_files, title, body),
        gather_external_context_md(
            config,
            &owner,
            &repo,
            &identifiers,
            &changed_files,
            title,
            body,
            input.run_mode,
        ),
    );

    // ── Step 6: build prompt and call LLM ─────────────────────────────────
    let llm_req = build_review_prompt(
        &owner,
        &repo,
        &pr_meta,
        &diff,
        &context,
        &external_context,
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

    // ── Degraded labelling (#590) ─────────────────────────────────────────
    // When an operator opted out of a required dependency, the review still ran
    // but MUST be loudly labelled non-authoritative: prepend a banner to the
    // rendered body and set the `error` reason so no consumer mistakes it for an
    // authoritative verdict.  `status` was already set to Degraded by the gate.
    if let Some(reason) = degraded_reason.as_ref() {
        result.review_body = format!("{}{}", degraded_banner(reason), result.review_body);
        if result.error.is_none() {
            result.error = Some(format!("degraded (non-authoritative): {reason}"));
        }
    }

    // ── Step 7: parse verdict + findings ──────────────────────────────────
    let parsed = parse_review_response(&llm_resp.text);
    if parsed.is_fail_safe {
        warn!(
            reason = ?parsed.fail_safe_reason,
            "verdict parsing fell back to fail-safe APPROVE"
        );
    }

    // ── Step 7b–7d: grade derivation, verification, grade reconciliation ─────
    let (final_verdict, final_grade) = apply_grade_and_floor(&parsed);
    info!(
        verdict = %final_verdict,
        grade = %final_grade,
        findings_count = parsed.findings.len(),
        "final verdict + grade after severity-anchored floor"
    );
    let mut findings = parsed.findings;
    // 7c: verification round — re-derives verdict from surviving findings.
    result.verdict = maybe_verify(
        config,
        deps.verifier.as_ref(),
        &diff,
        final_verdict,
        &mut findings,
    )
    .await;
    result.findings = findings;
    // 7d: clamp grade to stay consistent with the post-verification verdict.
    result.grade = Some(
        crate::pipeline::letter_grade::clamp_grade_to_verdict(final_grade, &result.verdict)
            .to_string(),
    );

    finalize_run(result, config, &input, deps.dedup.as_ref()).await
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Derive (verdict, grade) from a `ParsedReview` using grade + severity floor.
///
/// Why: extracted to keep `run_review` under the line cap and make it testable.
/// What: fail-safe → (APPROVE, default grade); normal → resolves LLM grade string
/// (or default), calls `derive_verdict_with_grade` for max(grade, model) + floor.
/// Test: covered by runner integration tests.
fn apply_grade_and_floor(
    parsed: &crate::pipeline::parser::ParsedReview,
) -> (Verdict, crate::pipeline::letter_grade::Grade) {
    if parsed.is_fail_safe {
        let v = parsed.verdict.clone();
        let g = default_grade_for_verdict(&v);
        return (v, g);
    }
    let grade = parsed
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
        });
    derive_verdict_with_grade(parsed.verdict.clone(), grade, &parsed.findings)
}

/// Fetch PR metadata and return `(ReviewPrMeta, head_sha)`.
///
/// Why: centralises the GitHub API call and head-SHA surfacing so the runner
/// can key the dedup store.
/// What: resolves token via run_mode, calls `fetch_pr_metadata`.
/// Test: tested indirectly via mock in integration tests.
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

/// Apply post-or-log finalisation (Phase 1, #582) for a completed review.
///
/// Why: single exit path so live/dry policy is applied exactly once.
/// What: builds `PostContext` from result fields, delegates to `finalize_review`.
/// Test: `post::tests` cover branch selection; runner tests assert dry-run.
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

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

use super::runner_coverage::load_coverage_contrib;
use super::runner_helpers::{
    abort_dry, apply_grade_and_floor, attach_inline_comments, fetch_github_pr_meta, finalize_run,
};
use crate::{
    config::{DiffStats, MapReduceConfig, ReviewConfig, ReviewPath, select_review_mode},
    coverage::{CoverageVerdictContrib, apply_coverage_floor},
    integrations::{analyze_client::AnalyzeClient, github::RunMode, search_client::SearchClient},
    llm::LlmProvider,
    models::{ReviewResult, ReviewStatus, Verdict},
    pipeline::{
        context_gate::{GateOutcome, degraded_banner, preflight_context},
        diff::{
            DiffSource, diff_was_truncated, extract_changed_files, extract_identifiers, load_diff,
            truncate_diff,
        },
        diff_analyzer::DiffAnalyzer, // noise filter (Stages A+B); #624
        parser::parse_review_response,
        prompt::{ReviewPrMeta, build_review_prompt_with_coverage},
        runner_context::{gather_context, gather_external_context_md},
        runner_mapreduce::{MapReduceRun, run_mapreduce_branch},
        trigger::TriggerDecision,
        verify::maybe_verify,
        voice_config::build_voice_config,
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
    // Render ONCE at no cap so DiffStats sees the true (untruncated) length the
    // selector needs; the unified path re-bounds to `max` below.
    let rendered_full = filtered.render_for_prompt(usize::MAX);
    let diff = truncate_diff(&filtered.render_for_prompt(max));
    debug!(orig = raw_diff.len(), filt = diff.len(), "diff filtered");

    // ── Step 3a: select review path (unified vs map-reduce) (#1643 / #680) ─
    // The selector decides per the `TRUSTY_REVIEW_MAP_MODE` heuristic: `auto`
    // routes to map-reduce when the unified render WOULD truncate (rendered
    // chars > MAX_DIFF_CHARS) OR the file count exceeds the threshold; `always`
    // forces map-reduce; `never` forces the unified path (today's behaviour).
    let mr_config = MapReduceConfig::from_env();
    let stats = DiffStats {
        diff_chars: rendered_full.len(),
        file_count: filtered.files.len(),
    };
    let review_path = select_review_mode(stats, &mr_config);
    debug!(
        mode = %mr_config.mode,
        diff_chars = stats.diff_chars,
        file_count = stats.file_count,
        path = ?review_path,
        "review path selected"
    );

    // ── Step 3b: diff-truncation guard (#1638) — UNIFIED PATH ONLY ─────────
    // Only the unified path can truncate; the map-reduce path reviews per-file
    // with no truncation, so the fail-closed guard is now a backstop scoped to
    // the unified path (and to the pathological all-failed map-reduce case,
    // handled in `run_mapreduce_branch`).
    // The unified path renders the (noise-filtered) diff bounded to MAX_DIFF_CHARS;
    // for an over-cap diff `render_for_prompt` drops whole files at the END of the
    // diff (and `truncate_diff` cuts trailing hunks).  A changed function/method
    // signature in a dropped/cut region is then INVISIBLE to the reviewer — the
    // live symptom that motivated this fix (a `build(…, null)` signature cut off so
    // the reviewer could not see it).  Reviewing a partial diff yields a wrong /
    // incomplete verdict, so we fail CLOSED to UNKNOWN (consistent with the #1241
    // truncated-output guard and the #590 required-context gate) rather than
    // silently reviewing only the visible portion.  The map-reduce path (#680) will
    // later review over-cap diffs per-file with no truncation; until that fan-out
    // lands, fail-closed is the safe behaviour.
    if review_path == ReviewPath::Unified && diff_was_truncated(&diff) {
        warn!(
            orig_chars = raw_diff.len(),
            rendered_chars = diff.len(),
            max_chars = max,
            "diff exceeded MAX_DIFF_CHARS and was truncated — failing CLOSED to UNKNOWN \
             so the reviewer never silently reviews a partial diff (#1638)"
        );
        result.verdict = Verdict::Unknown;
        result.error = Some(format!(
            "diff too large to review in full ({orig} chars > {max} cap) — content was \
             truncated before the reviewer, so changed code (e.g. function signatures) \
             may be invisible; could not review",
            orig = raw_diff.len(),
        ));
        return abort_dry(result, config, &input, &deps);
    }

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
    let (mut context, external_context) = tokio::join!(
        gather_context(config, &deps, &identifiers, &changed_files, title, body),
        gather_external_context_md(
            config,
            &owner,
            &repo,
            &identifiers,
            &changed_files,
            title,
            body,
            pr_number,
            input.run_mode,
        ),
    );

    // ── Step 5b: load coverage data and build coverage verdict contrib (#1014) ──
    // Coverage is FAIL-OPEN and OFF by default.  When `config.coverage.enabled`
    // is false (the default), `load_coverage_contrib` returns None and the entire
    // coverage pipeline is skipped.  Failures (e.g. LCOV file missing) produce a
    // warning and None — never an error that blocks the review.
    let coverage_contrib: Option<CoverageVerdictContrib> =
        load_coverage_contrib(config, &diff).await;

    // Inject the coverage contrib into the context struct for prompt assembly.
    context.coverage_contrib = coverage_contrib.clone();

    // ── Step 5c: MAP-REDUCE branch (#1643 / #680) ─────────────────────────
    // When the selector chose the per-file path, delegate to the map-reduce
    // branch: split the (untruncated) filtered diff into per-file units, review
    // EACH with its own LLM call (bounded fan-out), and reduce the per-chunk
    // verdicts/findings into one ReviewResult.  No file is ever truncated away,
    // so a changed signature near the END of a large diff is still reviewed.
    if review_path == ReviewPath::MapReduce {
        let run = MapReduceRun {
            filtered,
            raw_diff,
            pr_meta,
            context,
            external_context,
            coverage_contrib,
            degraded_reason,
        };
        return run_mapreduce_branch(config, &input, &deps, &mr_config, result, run).await;
    }

    // ── Step 6: build prompt and call LLM (UNIFIED PATH) ──────────────────
    // Build the 3-layer VoiceConfig (stock + principles + voice) from config.
    let voice_config = build_voice_config(config);
    let llm_req = build_review_prompt_with_coverage(
        &owner,
        &repo,
        &pr_meta,
        &diff,
        &context,
        &external_context,
        &input.reviewer_model,
        &voice_config,
        config.coverage.enabled,
    );
    debug!(model = %input.reviewer_model, "calling LLM reviewer");

    // Capture the requested output ceiling BEFORE the request is moved into
    // `complete`; truncation detection (#1241) compares the produced
    // `output_tokens` against this ceiling.
    let requested_max_tokens = llm_req.max_tokens;

    let llm_resp = match deps.llm.complete(llm_req).await {
        Ok(resp) => resp,
        Err(e) => {
            // Fail-CLOSED (#1241 supersedes spec REV-130): an LLM/transport error
            // means the review never happened — never silently APPROVE.  UNKNOWN
            // surfaces a clear "could not review" state and posts no green check.
            warn!("LLM call failed: {e} — applying fail-safe UNKNOWN (fail-closed, #1241)");
            result.verdict = Verdict::Unknown;
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

    // ── Step 6b: truncation guard (#1241) ─────────────────────────────────
    // If the reviewer hit (or nearly hit) the output-token ceiling, its JSON is
    // very likely cut off mid-object.  Parsing such output and treating it as a
    // verdict risks a silent (and wrong) APPROVE.  Fail CLOSED to UNKNOWN instead
    // of parse-and-trust.
    if is_truncated(
        llm_resp.finish_reason.as_deref(),
        llm_resp.output_tokens,
        requested_max_tokens,
    ) {
        warn!(
            output_tokens = llm_resp.output_tokens,
            max_tokens = requested_max_tokens,
            "LLM output hit the token ceiling — treating as truncated → UNKNOWN (fail-closed, #1241)"
        );
        result.verdict = Verdict::Unknown;
        result.error = Some(format!(
            "review output truncated at token ceiling ({}/{} tokens) — could not review",
            llm_resp.output_tokens, requested_max_tokens
        ));
        return abort_dry(result, config, &input, &deps);
    }

    // ── Step 7: parse verdict + findings ──────────────────────────────────
    let parsed = parse_review_response(&llm_resp.text);
    if parsed.is_fail_safe {
        warn!(
            reason = ?parsed.fail_safe_reason,
            "verdict parsing fell back to fail-safe UNKNOWN (fail-closed, #1241)"
        );
    }

    // ── Step 7b–7e: grade derivation, coverage floor, verification, reconcile ─
    // `original_llm_grade` is the pre-floor LLM grade; it is held separately so
    // that after verification potentially RELAXES the verdict (e.g. BLOCK → APPROVE
    // because the only blocking finding was refuted), the envelope grade can be
    // re-clamped from the original LLM grade rather than the floor-escalated grade.
    // Without this, a floor that escalated B- → F would not recover to B- when
    // verification refutes the escalating finding (closes #1486).
    let (final_verdict, final_grade, original_llm_grade) = apply_grade_and_floor(&parsed);
    info!(
        verdict = %final_verdict,
        grade = %final_grade,
        findings_count = parsed.findings.len(),
        "final verdict + grade after severity-anchored floor"
    );

    // 7b-post: apply coverage floor AFTER severity derivation (#1014).
    // Coverage can only TIGHTEN (REQUEST_CHANGES) — never soften a BLOCK.
    // This is a no-op when coverage gating is disabled (the default).
    // Note: the coverage-adjusted grade (_cov_grade) is intentionally not used as
    // the source for step 7d's grade derivation — see the #1486 fix comment below
    // for why the original_llm_grade is the correct basis for the post-verification
    // grade clamp.  The coverage floor only shifts the verdict; the grade is
    // re-clamped from original_llm_grade to the final post-verification verdict.
    let (final_verdict, _cov_grade) = if let Some(ref cov) = coverage_contrib {
        let before = final_verdict.clone();
        let (cv, cg) = apply_coverage_floor(final_verdict, final_grade, cov);
        if cv != before {
            info!(
                before = %before,
                after = %cv,
                reason = %cov.summary,
                "coverage floor tightened verdict"
            );
        }
        (cv, cg)
    } else {
        (final_verdict, final_grade)
    };

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

    // 7d: derive the envelope grade from the post-verification verdict (closes #1486).
    //
    // BEFORE this fix: the grade was clamped from `final_grade` (the floor-escalated
    // grade) to `result.verdict`.  When verification RELAXES the verdict (e.g. BLOCK
    // → APPROVE because the only blocking finding was refuted by the verifier), the
    // clamp of F→APPROVE is a no-op (F implies BLOCK which is already stricter than
    // APPROVE), so the envelope grade stayed F even though the verdict became APPROVE.
    //
    // AFTER this fix: we clamp the original LLM grade (pre-floor) to the
    // post-verification verdict.  The floor escalation (B- → F for a BLOCK) no
    // longer leaks into the envelope when verification relaxes the verdict: if the
    // floor finding was refuted, the envelope correctly shows B-/APPROVE instead of
    // F/APPROVE.  If the blocking finding survives verification (verdict stays BLOCK),
    // `clamp_grade_to_verdict(B-, BLOCK)` = F — the correct, consistent result.
    //
    // Coverage-floor tightening (step 7b-post above) may also shift `final_grade`
    // independently of verification; for now we treat original_llm_grade as the
    // soft starting point and defer tighter coverage-grade interaction to a follow-up.
    // In practice the coverage floor only drives REQUEST_CHANGES (not BLOCK), and the
    // original LLM grade for REQUEST_CHANGES is already at the D-band, so the clamp
    // is correct in the common case.
    result.grade = Some(
        crate::pipeline::letter_grade::clamp_grade_to_verdict(original_llm_grade, &result.verdict)
            .to_string(),
    );

    // 7e: build inline per-line comments from the RAW diff (#1414).  Using the
    // pre-filter `raw_diff` (not the noise-filtered `diff` sent to the LLM) means
    // anchors map to the actual PR diff lines GitHub will accept; findings that do
    // not map to a diff line fall back to the summary body.
    attach_inline_comments(&mut result, &raw_diff);

    finalize_run(result, config, &input, deps.dedup.as_ref()).await
}

/// Default fraction of the output-token ceiling at/above which a response is
/// treated as truncated when no `finish_reason` is available (closes #1241).
///
/// Why: this is the FALLBACK heuristic.  Some providers stop generating exactly
/// at `max_tokens` without surfacing a `finish_reason`; when the completion lands
/// at ≥95 % of the ceiling the structured JSON is very likely cut off mid-object,
/// so trusting its parse risks a silent wrong-APPROVE.  95 % (not 100 %) leaves a
/// small margin for provider-side token-count rounding so a genuinely-complete
/// response that lands a few tokens under the ceiling is not mis-flagged.  As of
/// #1357 this ratio is only consulted when `finish_reason` is absent — a provider
/// that reports `finish_reason: "stop"` at 99 % of the ceiling is NOT flagged.
/// What: the default multiplier applied to `max_tokens`, overridable at runtime
/// via `truncation_token_ratio` (env seam) so operators can retune without a
/// rebuild.
/// Test: `is_truncated_ratio_fallback_*` unit tests in `runner_tests.rs`.
const DEFAULT_TRUNCATION_TOKEN_RATIO: f64 = 0.95;

/// Environment variable that overrides [`DEFAULT_TRUNCATION_TOKEN_RATIO`].
///
/// Why: #1357 asked for the fallback ratio to be configurable.  A single env seam
/// (rather than threading a config field through every call site) keeps the change
/// small while still letting operators retune the fallback band without a rebuild.
/// What: parsed as `f64` in `truncation_token_ratio`; ignored when unset, empty,
/// unparseable, or outside `(0.0, 1.0]`.
const TRUNCATION_TOKEN_RATIO_ENV: &str = "TRUSTY_REVIEW_TRUNCATION_TOKEN_RATIO";

/// Resolve the effective truncation token ratio (env override, else default).
///
/// Why: centralises the configurable-ratio seam (#1357) so both the runner and its
/// tests read the ratio through one place; an out-of-range or unparseable override
/// falls back to the default rather than silently disabling the safety check.
/// What: reads `TRUSTY_REVIEW_TRUNCATION_TOKEN_RATIO`; returns the parsed value when
/// it is a finite `f64` in `(0.0, 1.0]`, else `DEFAULT_TRUNCATION_TOKEN_RATIO`.
/// Test: `truncation_ratio_env_override_applies`, `truncation_ratio_env_invalid_falls_back`.
fn truncation_token_ratio() -> f64 {
    match std::env::var(TRUNCATION_TOKEN_RATIO_ENV) {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 && v <= 1.0 => v,
            _ => DEFAULT_TRUNCATION_TOKEN_RATIO,
        },
        Err(_) => DEFAULT_TRUNCATION_TOKEN_RATIO,
    }
}

/// Return `true` when an LLM completion appears truncated at the token ceiling.
///
/// Why: a truncated reviewer response must fail CLOSED to UNKNOWN rather than be
/// parsed into a (likely wrong) APPROVE — the #1241 safety fix.  Before #1357 the
/// detection was purely arithmetic (token-ratio), which FALSE-POSITIVED on large
/// but complete responses that legitimately landed in the ≥95 % band.  The
/// provider's own `finish_reason` is the authoritative truncation signal, so #1357
/// makes it PRIMARY and keeps the token-ratio only as a fallback when the provider
/// did not surface a reason.
/// What:
///   1. PRIMARY — when `finish_reason` is present: return `true` iff it is a
///      length/truncation reason (`"length"` / `"max_tokens"` / `"max_token"`),
///      and `false` for any natural-stop reason (`"stop"`, `"end_turn"`, …).  The
///      token ratio is NOT consulted, so a complete response at 99 % of the ceiling
///      is not mis-flagged.
///   2. FALLBACK — when `finish_reason` is `None`: return `true` when `max_tokens > 0`
///      AND `output_tokens >= ceil(max_tokens * truncation_token_ratio())`.  A
///      `max_tokens` of 0 (unknown ceiling) disables the check (returns `false`).
///
/// `finish_reason` is matched case-insensitively (providers already lowercase it,
/// but we trim/lowercase defensively).
///
/// Test: `is_truncated_finish_reason_length_true`,
/// `is_truncated_finish_reason_stop_at_high_ratio_false`,
/// `is_truncated_ratio_fallback_at_ceiling_true`,
/// `is_truncated_ratio_fallback_well_under_false`,
/// `is_truncated_unset_ceiling_false`.
fn is_truncated(finish_reason: Option<&str>, output_tokens: u32, max_tokens: u32) -> bool {
    // PRIMARY: trust the provider's explicit completion reason when present.
    if let Some(reason) = finish_reason {
        let r = reason.trim().to_ascii_lowercase();
        if !r.is_empty() {
            return matches!(r.as_str(), "length" | "max_tokens" | "max_token");
        }
    }

    // FALLBACK: no usable finish_reason — use the token-ratio heuristic.
    if max_tokens == 0 {
        return false;
    }
    let threshold = (f64::from(max_tokens) * truncation_token_ratio()).ceil() as u32;
    output_tokens >= threshold
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

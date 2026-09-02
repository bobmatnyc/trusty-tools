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

use tracing::{debug, error, info, warn};

use super::runner_coverage::load_coverage_contrib;
use super::runner_helpers::{
    DedupClaim, abort_dry, apply_grade_and_floor, attach_inline_comments, build_author_rationale,
    fetch_github_pr_meta, finalize_run, ground_parsed_findings, mark_no_head_sha_abort,
    resolve_diff_token,
};
use crate::{
    config::{
        DiffStats, InvocationSurface, MapReduceConfig, ReviewConfig, ReviewPath, select_review_mode,
    },
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
        post::{FinalizeAction, decide_action},
        prompt::{ReviewPrMeta, build_review_prompt_with_coverage},
        runner_context::{gather_context, gather_external_context_md},
        runner_mapreduce::{MapReduceRun, run_mapreduce_branch},
        trigger::TriggerDecision,
        verify::maybe_verify,
        voice_config::build_voice_config,
    },
    store::{ClaimOutcome, DedupError, DedupStore},
};

// ─── Pipeline input ───────────────────────────────────────────────────────────

/// Optional caller-supplied PR context (#1618).
///
/// Why: a caller (CI, editor, MCP) often has richer context than the bare diff —
/// the PR description prose, the human review/issue discussion (author rationale),
/// and any related/referenced source the diff depends on.  On the local-diff path
/// there is NO GitHub fetch, so the caller is the ONLY source of this context.
/// Threading it lets the reviewer see the author's intent and lets the adversarial
/// verifier refute a finding the author has already empirically addressed.
/// What: three optional prose blocks, all `None` by default so every existing
/// construction site is a one-line `caller_context: CallerContext::default()`.
/// The reviewer renders all three as labelled sections; the verifier receives the
/// description + discussion as author rationale.  Provider-agnostic — no
/// integration-specific shape, just free-form caller prose.
/// Test: `prompt_includes_caller_context`, `verify_request_includes_author_rationale`.
#[derive(Debug, Default, Clone)]
pub struct CallerContext {
    /// PR body/description prose.
    pub pr_description: Option<String>,
    /// Concatenated human review/issue comments — author rationale.
    pub pr_discussion: Option<String>,
    /// Caller-supplied referenced/related code or domain context.
    pub referenced_code: Option<String>,
}

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
    /// Optional caller-supplied PR context (#1618): description, discussion, and
    /// referenced code.  `CallerContext::default()` (all `None`) for callers that
    /// have no extra context — unaffected behaviour.
    pub caller_context: CallerContext,
    /// Which kind of caller triggered this review (search-unreachable semantics
    /// fix) — decides the safe DEFAULT for `require_search` when the operator
    /// has not set an explicit override.
    ///
    /// `InvocationSurface::Hosted` (the type's `#[default]`) is the SAFE choice
    /// for any call site that is not explicitly known to be interactive: the
    /// webhook bot and CLI GitHub-PR runs, which CAN post to a real PR, must
    /// never silently degrade.  `InvocationSurface::Interactive` is set
    /// explicitly by the MCP tool handlers (`mcp::tools`) and by `run
    /// --local-diff`/`--base`/`--source-root` local reviews — surfaces that can
    /// never post to a real PR, so a diff-only DEGRADED review is safe and
    /// still useful when search is down.
    pub surface: InvocationSurface,
}

/// Injected service dependencies (trait objects for testability).
///
/// Why: the pipeline calls trusty-search and trusty-analyze via trait objects
/// so tests can inject fakes without a running daemon.
/// What: all fields are `Arc<dyn Trait>` for cheap cloning in `compare` mode.
/// Test: `run_review_with_fake_provider_approves`.
#[derive(Clone)]
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
/// `run_review_search_down_degraded_when_optout`,
/// `run_review_posting_without_dedup_store_fails_closed`,
/// `run_review_empty_head_sha_fails_closed_before_posting`.
pub async fn run_review(
    config: &ReviewConfig,
    input: ReviewInput,
    deps: ReviewDeps,
) -> ReviewResult {
    // ── Step 1: determine owner/repo/pr from diff source ──────────────────
    // `LocalFile`, `GitRange`, and `Stdin` are all treated identically here:
    // owner="local" is the sentinel `post::finalize_review` checks (via
    // `is_github = owner != "local"`) to force `FinalizeAction::LogOnly` — so
    // every non-GitHub source automatically inherits the "never post" / #2993
    // dry-run guarantee without a separate posting check.
    let (owner, repo, pr_number, is_local) = match &input.diff_source {
        DiffSource::Github {
            owner, repo, pr, ..
        } => (owner.clone(), repo.clone(), *pr, false),
        DiffSource::LocalFile { path } => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("local");
            ("local".to_string(), stem.to_string(), 0_u64, true)
        }
        DiffSource::GitRange { base, head, .. } => {
            let head_label = head.as_deref().unwrap_or("HEAD");
            (
                "local".to_string(),
                format!("{base}...{head_label}"),
                0_u64,
                true,
            )
        }
        DiffSource::Stdin => ("local".to_string(), "stdin".to_string(), 0_u64, true),
    };

    let pr_url = if !is_local {
        format!("https://github.com/{owner}/{repo}/pull/{pr_number}")
    } else {
        String::new()
    };

    // ── Step 1b: posting requires the claim gate (#5113) ──────────────────
    // The dedup contract makes a live post idempotent, so a run that can reach
    // the post path without a store has no defence against a duplicate comment
    // on a re-run. `decide_action` is the single place that decides whether the
    // post path is reachable, so ask it rather than re-deriving the rule. This
    // runs before the PR-metadata fetch so an unguarded run costs nothing.
    if !is_local
        && deps.dedup.is_none()
        && decide_action(config.dry_run, input.trigger, input.allow_posting, true)
            == FinalizeAction::Post
    {
        error!(
            owner = %owner,
            repo = %repo,
            pr = pr_number,
            "posting is enabled with no dedup claim store — aborting without posting (#5113)"
        );
        let mut result = ReviewResult::new(
            owner.clone(),
            repo.clone(),
            pr_number,
            format!("PR #{pr_number}"),
            pr_url.clone(),
        );
        result.verdict = Verdict::Unknown;
        result.error = Some(
            "not reviewed: posting is enabled but no dedup claim store is configured, so a \
             re-run could post a duplicate comment (#5113)"
                .to_string(),
        );
        // NotHeld — no claim was ever attempted, let alone acquired.
        return abort_dry(result, config, &input, &deps, DedupClaim::NotHeld).await;
    }

    // ── Step 2: fetch PR metadata (skip for local-diff mode) ──────────────
    // #6062: the fetch failure's own reason travels with the empty head SHA —
    // the guard below reports both the consequence and the cause, so a run that
    // stops for a missing SHA still names the config an operator has to fix.
    let (pr_meta, head_sha, meta_error): (ReviewPrMeta, String, Option<String>) = if is_local {
        (ReviewPrMeta::default(), String::new(), None)
    } else {
        match fetch_github_pr_meta(config, &owner, &repo, pr_number, input.run_mode).await {
            Ok((m, sha)) => (m, sha, None),
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
                    Some(e.to_string()),
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

    // ── Step 2a: no head SHA, no post (#6062) ─────────────────────────────
    // The claim below and `finalize_review`'s `complete()` both key on the head
    // SHA, and `decide_action` never reads it — so a failed metadata fetch used
    // to review and post with the claim never taken and never completed, and a
    // retry after the same failure posted a duplicate. Ask `decide_action`
    // whether the post path is reachable, exactly as the #5113 guard does.
    if !is_local
        && head_sha.is_empty()
        && decide_action(config.dry_run, input.trigger, input.allow_posting, true)
            == FinalizeAction::Post
    {
        // #6062: empty head_sha is fail-closed — no claim, no post
        mark_no_head_sha_abort(&mut result, meta_error.as_deref());
        // NotHeld — no claim was ever attempted, let alone acquired.
        return abort_dry(result, config, &input, &deps, DedupClaim::NotHeld).await;
    }

    // ── Step 2b: dedup claim (Phase 1, #582) ──────────────────────────────
    // Claim the (owner,repo,pr,head_sha) slot before doing expensive work.  A
    // completed claim for the same head SHA short-circuits the whole pipeline.
    // #5064: a store error means the gate did not engage, so the review
    // aborts without posting rather than proceeding unguarded.
    if !is_local
        && !head_sha.is_empty()
        && let Some(store) = deps.dedup.as_ref()
    {
        match classify_claim(store.claim(&owner, &repo, pr_number, &head_sha).await) {
            ClaimGate::DuplicateSkip => {
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
                // #1877: `result.findings` is empty here (no LLM call happened
                // yet) but keep the sync explicit rather than relying on the
                // `ReviewResult::new()` default staying 0 forever.
                result.findings_count = result.findings.len();
                return result;
            }
            // #5126: the slot is held by someone else, so this review never
            // ran. Report that, never a verdict.
            ClaimGate::InProgressElsewhere => {
                warn!(
                    owner = %owner,
                    repo = %repo,
                    pr = pr_number,
                    head_sha = %head_sha,
                    "dedup: another holder owns the in-progress claim — not reviewed"
                );
                let stale_secs = crate::config::constants::DEDUP_STALE_SECS;
                result.verdict = Verdict::Unknown;
                result.error = Some(format!(
                    "not reviewed: another review holds the in-progress dedup claim for \
                     head SHA {head_sha}; it clears when that review finishes or after \
                     {stale_secs}s"
                ));
                // NotHeld — this review never acquired the claim, so it must
                // not delete the holder's record.
                return abort_dry(result, config, &input, &deps, DedupClaim::NotHeld).await;
            }
            ClaimGate::Proceed => {
                debug!(head_sha = %head_sha, "dedup: claimed review slot");
            }
            // #5064: the claim gate did not engage — abort rather than post.
            ClaimGate::Abort(reason) => {
                error!(
                    owner = %owner,
                    repo = %repo,
                    pr = pr_number,
                    head_sha = %head_sha,
                    "dedup claim failed — aborting without posting: {reason}"
                );
                result.error = Some(format!("dedup claim unavailable: {reason}"));
                // #5064: NotHeld — this review never acquired the claim, so it
                // must not delete whatever record is on disk.
                return abort_dry(result, config, &input, &deps, DedupClaim::NotHeld).await;
            }
        }
    }

    // ── Step 2c: resolve the GitHub token for the diff fetch (#1880) ──────
    // `DiffSource::Github` may carry an empty placeholder token (service-path
    // callers defer resolution here); resolve it now via the run-mode's
    // `AuthStrategy` rather than letting `load_diff` send an empty bearer
    // token and surface an opaque 401 from GitHub.
    let diff_source = match resolve_diff_token(&input.diff_source, config, input.run_mode).await {
        Ok(source) => source,
        Err(e) => {
            warn!("failed to resolve GitHub token for diff fetch: {e}");
            result.error = Some(format!("GitHub token resolution failed: {e}"));
            return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
        }
    };

    // ── Step 3: load, filter (DiffAnalyzer Stages A+B), and truncate diff ─
    // truncate_diff is the final safety net after noise filtering (REV-209).
    let raw_diff = match load_diff(&diff_source).await {
        Ok(d) => d,
        Err(e) => {
            warn!("failed to load diff: {e}");
            result.error = Some(format!("diff load failed: {e}"));
            return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
        }
    };
    let filtered = DiffAnalyzer::default().analyze(&raw_diff).await;
    let max = crate::config::constants::MAX_DIFF_CHARS;
    // #1660: render ONCE, bounded to `max` — NOT a second, unbounded render at
    // `usize::MAX` just to learn the length.  `render_for_prompt` already
    // stays within its own budget and self-announces with
    // `RENDER_TRUNCATED_MARKER` whenever it had to drop content to do so
    // (see `diff_was_truncated`), so that single bounded render tells us
    // everything the selector needs: when nothing was dropped, this render's
    // length IS the true (untruncated) length; when something was dropped, the
    // marker alone proves the untruncated length exceeds `max`, which is all
    // `select_review_mode` compares against (`diff_chars > MAX_DIFF_CHARS`) —
    // the exact untruncated figure is never otherwise consumed.
    let diff = truncate_diff(&filtered.render_for_prompt(max));
    let would_truncate = diff_was_truncated(&diff);
    debug!(orig = raw_diff.len(), filt = diff.len(), "diff filtered");

    // ── Step 3a: select review path (unified vs map-reduce) (#1643 / #680) ─
    // The selector decides per the `TRUSTY_REVIEW_MAP_MODE` heuristic: `auto`
    // routes to map-reduce when the unified render WOULD truncate (rendered
    // chars > MAX_DIFF_CHARS) OR the file count exceeds the threshold; `always`
    // forces map-reduce; `never` forces the unified path (today's behaviour).
    let mr_config = MapReduceConfig::from_env();
    let stats = DiffStats {
        // Exact only when nothing was dropped (see the render comment above);
        // once truncation happened the precise count is moot — `max + 1` still
        // satisfies the selector's sole `> MAX_DIFF_CHARS` comparison.
        diff_chars: if would_truncate {
            max.saturating_add(1)
        } else {
            diff.len()
        },
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
    if review_path == ReviewPath::Unified && would_truncate {
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
        return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
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
    let degraded_reason: Option<String> =
        match preflight_context(config, &deps, input.surface).await {
            GateOutcome::Proceed => None,
            GateOutcome::Skip(reason) => {
                warn!("required-context gate: skipping review — {reason}");
                result.status = ReviewStatus::Skipped;
                // Search-unreachable semantics fix: this is the SOLE producer of
                // ReviewStatus::Skipped, so this Skip is always a genuine infra
                // outage, never a policy skip — mark it so the MCP layer can be
                // loud (isError:true + sentinel) without guessing from `status`
                // alone (see `ReviewResult::infra_unavailable`).
                result.infra_unavailable = true;
                result.verdict = Verdict::Unknown;
                result.error = Some(reason);
                result.dry_run = true;
                // Return WITHOUT finalize_review so a skipped review is never posted.
                // Release any dedup claim so a retry (once the dep recovers) can re-run.
                return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
            }
            GateOutcome::Degraded(reason) => {
                warn!("required-context gate: proceeding DEGRADED (non-authoritative) — {reason}");
                result.status = ReviewStatus::Degraded;
                Some(reason)
            }
        };

    // ── Step 5: gather context in parallel (search/analyze + external) ──
    // All sources are FAIL-OPEN: errors contribute nothing, never block the review
    // (distinct from the #590 required gate above).
    // #4999: APEX retrieval was dropped by owner ruling (0/69 citations).
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

    // Inject caller-supplied PR context (#1618) for the reviewer prompt.  These
    // render as labelled sections only when present; on the local-diff path they
    // are the sole source of PR description / discussion / referenced code (no
    // GitHub fetch happened).  Cloned because the verifier also needs the
    // description + discussion as author rationale below.
    context.pr_description = input.caller_context.pr_description.clone();
    context.pr_discussion = input.caller_context.pr_discussion.clone();
    context.referenced_code = input.caller_context.referenced_code.clone();

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
            return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
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
    // #4999: `apply_llm_response` copies the response text verbatim, and under
    // forced structured output that text IS the JSON payload. Render it to prose
    // here — before the banner, the truncation guard, and every abort path — so
    // no consumer can ever read the wire payload as the review.
    result.review_body = crate::pipeline::body_render::render_review_body(&result.review_body);

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
        return abort_dry(result, config, &input, &deps, DedupClaim::Held).await;
    }

    // ── Step 7: parse verdict + findings ──────────────────────────────────
    let mut parsed = parse_review_response(&llm_resp.text);
    if parsed.is_fail_safe {
        warn!(
            reason = ?parsed.fail_safe_reason,
            "verdict parsing fell back to fail-safe UNKNOWN (fail-closed, #1241)"
        );
        // #4491: carry the reason into the rendered result and the JSON log, so a
        // parse failure never reads as a clean review with "Findings: none".
        let reason = parsed
            .fail_safe_reason
            .clone()
            .unwrap_or_else(|| "unparseable LLM response".to_string());
        result.error = Some(format!("review not parsed — {reason}"));
    }

    // ── Step 7-hyg/cite/absent/relax: output hygiene + grounding ──────────
    // Self-negated findings, ungrounded citations, refuted diff-absence claims,
    // and a self-reported verdict left resting on findings this run removed —
    // all four run before grading. See `ground_parsed_findings`.
    ground_parsed_findings(&mut parsed, &filtered);

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
        // `final_grade` is None for an un-reviewable UNKNOWN verdict (#1474).
        grade = final_grade.map(|g| g.to_string()).unwrap_or_else(|| "none".to_string()),
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
    // Pass the caller-supplied PR description + discussion as author rationale
    // (#1618) so the adversarial verifier can REFUTE a finding the author has
    // already empirically addressed (e.g. "checked the data source; no values
    // exceed X").  `build_author_rationale` returns None when neither is present,
    // leaving the verifier prompt unchanged for existing callers.
    let author_rationale = build_author_rationale(
        input.caller_context.pr_description.as_deref(),
        input.caller_context.pr_discussion.as_deref(),
    );
    result.verdict = maybe_verify(
        config,
        deps.verifier.as_ref(),
        &diff,
        final_verdict,
        &mut findings,
        author_rationale.as_deref(),
    )
    .await;
    result.findings = findings;

    // 7d: derive the envelope grade from the post-verification verdict (closes #1486),
    // suppressing the letter grade entirely for an un-reviewable UNKNOWN (#1474).
    //
    // #1486: we clamp the ORIGINAL LLM grade (pre-floor) to the post-verification
    // verdict — NOT the floor-escalated `final_grade`.  When verification RELAXES the
    // verdict (e.g. BLOCK → APPROVE because the only blocking finding was refuted),
    // clamping the floor-escalated grade (F) would be a no-op (F implies BLOCK, already
    // stricter than APPROVE), leaving a stale F/APPROVE.  Clamping the original grade
    // (B-) instead correctly recovers B-/APPROVE; if the blocking finding survives
    // (verdict stays BLOCK), `clamp_grade_to_verdict(B-, BLOCK)` = F — consistent.
    //
    // #1474: `original_llm_grade` is `None` for an un-reviewable UNKNOWN verdict, so
    // the envelope grade stays `None` (output field omitted) — never an "F".  UNKNOWN
    // means "could not review", which must never collapse into "reviewed → critical
    // failure".  `.map` cleanly threads both behaviours: None ⇒ no grade; Some(grade)
    // ⇒ clamp to the (real, non-UNKNOWN) post-verification verdict.
    //
    // Coverage-floor tightening (step 7b-post above) only drives REQUEST_CHANGES (not
    // BLOCK), and the original LLM grade for REQUEST_CHANGES is already at the D-band,
    // so the clamp is correct in the common case.
    //
    // #4044: `clamp_grade_to_verdict` only moves a grade that is too OPTIMISTIC
    // for the verdict; a grade that is too SEVERE passes through untouched. So
    // when verification refuted the blocking findings and the verdict relaxed,
    // the model's own "F" survived verbatim next to an APPROVE — the grade was
    // still resting on evidence the pipeline had already discarded.
    // `reconcile_grade_with_verdict` moves it in both directions; `grade.rs`
    // already switched to it for the #PR84 case and this call site was missed.
    result.grade = original_llm_grade.map(|g| {
        crate::pipeline::letter_grade::reconcile_grade_with_verdict(g, &result.verdict).to_string()
    });

    // 7d-post: flag a suspiciously "shallow" clean review (#1877) — a
    // zero-findings APPROVE on a large diff whose output-token spend looks too
    // small for a substantive pass (e.g. `pricerator#637`: A+/0-findings in
    // 2.6s/$0.011 on a 300+-line PR) — and cap its grade so it never reads as
    // top-quality on trust alone.  Uses the RENDERED, possibly-truncated `diff`
    // actually sent to the reviewer (not `raw_diff`) since that is what the
    // model's token spend is proportional to.  This does not touch the
    // fail-CLOSED paths above (LLM/transport error, truncated output, oversized
    // diff) — those already correctly resolve to UNKNOWN, not APPROVE.
    result.shallow_clean_review = crate::pipeline::letter_grade::is_shallow_clean_review(
        &result.verdict,
        result.findings.is_empty(),
        diff.len(),
        result.output_tokens,
    );
    if result.shallow_clean_review {
        warn!(
            verdict = %result.verdict,
            diff_len = diff.len(),
            output_tokens = result.output_tokens,
            cost_usd = result.cost_estimate_usd,
            latency_ms = result.latency_ms,
            "flagged shallow clean review — zero findings with implausibly low \
             token spend for this diff size (#1877); grade capped at B-"
        );
        if let Some(g) = result
            .grade
            .as_deref()
            .and_then(|s| s.parse::<crate::pipeline::letter_grade::Grade>().ok())
        {
            result.grade =
                Some(crate::pipeline::letter_grade::cap_shallow_review_grade(g).to_string());
        }
    }

    // 7d-reconcile: mirror the final top-level grade into the raw `review_body`
    // JSON so the embedded self-grade the model reported can never disagree with
    // the authoritative post-floor/post-cap grade (issue #1886).  This runs AFTER
    // every grade adjustment above (severity floor, #1486 clamp, #1877 shallow cap)
    // and BEFORE the footer is appended in `finalize_review`, so both observable
    // copies of the grade are settled to the single source of truth.
    result.review_body = crate::pipeline::grade_reconcile::reconcile_review_body_grade(
        &result.review_body,
        result.grade.as_deref(),
    );

    // #1902: the verdict embedded in the same raw JSON is stale for the same
    // reason the grade was — it is the model's pre-floor lean, not the value
    // the severity floor, coverage floor, and verification round settled on.
    // A merge gate reading the top-level BLOCK while a human read an embedded
    // APPROVE is the reported harm.
    result.review_body = crate::pipeline::grade_reconcile::reconcile_review_body_verdict(
        &result.review_body,
        &result.verdict,
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

// ─── Dedup claim gate (#5064) ────────────────────────────────────────────────

/// What the runner does with a `claim()` outcome.
///
/// Why: naming the outcomes makes the fail-closed rule reviewable in one place.
/// It used to be an inline `match` whose error arm proceeded with the review,
/// so a store failure produced an ungated live comment — and on the next
/// redelivery, another one.
/// What: `Proceed` owns the slot; `DuplicateSkip` short-circuits a review that
/// already ran to completion; `InProgressElsewhere` blocks a review that has
/// NOT run because another holder owns the slot (#5126); `Abort` carries the
/// reason a claim could not be established.
/// Test: `classify_claim_*` in `runner_tests.rs`.
pub(super) enum ClaimGate {
    /// This caller owns the review slot.
    Proceed,
    /// A completed review already exists for this head SHA.
    DuplicateSkip,
    /// Another holder owns a fresh in-progress claim; nothing was reviewed.
    InProgressElsewhere,
    /// The claim gate did not engage; abort without posting.
    Abort(String),
}

/// Decide what a `claim()` result means for the review about to run.
///
/// Why: #5064 — every `DedupError` means the same thing operationally. The
/// caller does not know whether this head SHA was already reviewed, and could
/// not record that it is reviewing it now. Proceeding posts an unguarded
/// comment; aborting drops the review. The webhook handler has already returned
/// 202 by this point (`service::webhook`), so GitHub will NOT redeliver — the
/// review is lost until a human re-requests it. That is still the better half
/// of the trade: a dropped review is visible and re-requestable, a duplicate
/// comment cannot be retracted. Every error aborts, `Contended` included, which
/// is the variant a stuck sibling process produces during a rolling upgrade.
/// What: maps `Ok(Claimed)` → `Proceed`, `Ok(Skipped)` → `DuplicateSkip`,
/// `Ok(InProgressElsewhere)` → `InProgressElsewhere` (#5126), and every `Err` →
/// `Abort` carrying the error's `Display`.
/// Test: `classify_claim_contended_aborts`, `classify_claim_open_error_aborts`,
/// `classify_claim_claimed_proceeds`, `classify_claim_skipped_is_duplicate`,
/// `stranded_in_progress_claim_is_not_a_duplicate_skip`.
pub(super) fn classify_claim(outcome: Result<ClaimOutcome, DedupError>) -> ClaimGate {
    match outcome {
        Ok(ClaimOutcome::Claimed) => ClaimGate::Proceed,
        Ok(ClaimOutcome::Skipped) => ClaimGate::DuplicateSkip,
        Ok(ClaimOutcome::InProgressElsewhere) => ClaimGate::InProgressElsewhere,
        Err(e) => ClaimGate::Abort(e.to_string()),
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

//! Per-finding verification round (Phase 2, #583).
//!
//! Why: the reviewer LLM over-fires — calibration showed REQUEST_CHANGES/BLOCK
//! verdicts driven by speculative findings that do not survive scrutiny.  A
//! second, cheaper LLM pass that confirms or refutes each candidate finding
//! cuts those false-positive blocking verdicts before they are posted.  This is
//! the trusty-review port of the code-intelligence verifier protocol.
//!
//! What: `run_verification_round` sends EVERY undecided finding to the verifier
//! (#8904), batched and capped by `verify_batch`, with a strict CONFIRMED /
//! REFUTED / UNVERIFIABLE judgment. `verify_posted::enforce_outcomes` then
//! drops every refuted, unjudged, or over-cap finding and settles the verdict:
//! nothing dropped → `rederive_verdict`; anything dropped → the #8905 withhold
//! policy, which never turns a non-APPROVE verdict into APPROVE, floored at the
//! pre-verification verdict when the verifier failed on a finding.
//! `probe_verifier_liveness` is the startup
//! gate that refuses live mode when the verifier model is unavailable.
//!
//! ## Every posted finding (#8904)
//! Before #8904, `select_candidates` sent only the findings that could change
//! the verdict: confidence ≥ 0.90 on an APPROVE / APPROVE* review, ≥ 0.50 on a
//! blocking one. On trusty-review 0.36.1 that selected nothing in 9 of 10
//! reviews, and 7 of 7 fabricated findings were posted unverified.
//!
//! ## Third judgment (#5309)
//! The verifier may also answer `UNVERIFIABLE` — "the evidence needed to settle
//! this finding is not in the diff I was given". Before #5309 the schema offered
//! only CONFIRMED and REFUTED, so a claim the diff could not settle had to be
//! forced into one of them; on PR #5303 it was forced into CONFIRMED, on a
//! finding whose own text said it could not be confirmed from the diff, and that
//! produced a false BLOCK. `Unverifiable` is not a refutation: the finding
//! survives into the verdict input, but `apply_outcome` strips the signals that
//! let it escalate. See `evidence_admission` for the deterministic sibling that
//! catches the same shape before the verifier is ever called.
//!
//! ## Liveness gate
//! The startup liveness probe (`probe_verifier_liveness`, in `verify_liveness.rs`)
//! refuses live mode when the verifier model is dead, so a stale inference profile
//! cannot silently auto-refute every finding.  See that module for the full incident
//! rationale.
//!
//! ## Fail-open fix (#1876)
//! A transient verifier error (rate limit, transport blip, upstream 5xx) is
//! "unable to verify", not "the model refuted this finding".  Prior to #1876,
//! `verify_one` mapped transient errors to plain `VerifyOutcome::Refuted` —
//! structurally identical to a clean model REFUTED — which made
//! `rederive_verdict` fail OPEN: a network hiccup on the ONLY candidate finding
//! could collapse the whole review's baseline to APPROVE even though the model
//! never rendered a judgment.  Transient errors now map to `ErrorRefuted`
//! (matching the existing #726 treatment of config/lifecycle errors and
//! truncated responses), so `rederive_verdict` preserves `primary_verdict`
//! instead of discarding it.  See `verify_one` for the full rationale.
//!
//! ## Fan-out retry and honest UNVERIFIED (#4459)
//! The round used to fan out at a hardcoded width of 4 with no retry, and in
//! production that fan-out was the thing breaking it: 27 of 29 findings in one
//! measured review came back `Transport` while a single call to the same model
//! in isolation succeeded in ~845 ms, so fabrication detection was effectively
//! off. `VerifyPolicy` now carries the width and a per-finding attempt budget
//! from `[verification] concurrency` / `max_attempts`, `verify_one` retries a
//! transient failure with exponential backoff and jitter, and a finding still
//! unreachable after the last attempt is recorded `Unverifiable` rather than
//! `ErrorRefuted`, which reads as a judgment nothing made. #8904 withholds such
//! a finding; `ReviewResult::withheld_unverified_count` counts it, and
//! `ReviewResult::unverified_count` includes that count (Test:
//! `run_review_partial_verifier_outage_reports_the_withheld_count`).
//!
//! Test: `verify_tests.rs` — candidate selection, CONFIRMED/REFUTED outcomes,
//! verdict re-derivation, truncation regression (#726), transient-error
//! fail-open regression (#1876), fan-out retry (#4459), liveness-gate logic;
//! `verify_posted_tests.rs` — every-finding coverage, fail-closed drops, the
//! call cap, and the withhold verdict (#8904).

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::{
    config::ReviewConfig,
    config::constants::VERIFY_REFUTED_CONFIDENCE,
    config::verification::{
        DEFAULT_VERIFY_BATCH_SIZE, DEFAULT_VERIFY_CONCURRENCY, DEFAULT_VERIFY_MAX_ATTEMPTS,
        DEFAULT_VERIFY_MAX_CALLS,
    },
    llm::{LlmError, LlmProvider},
    models::{Finding, Verdict, VerifyOutcome},
    pipeline::{
        grade::derive_verdict,
        verify_batch::{build_batch_request, file_diff_slice, parse_batch_judgments, plan_batches},
        verify_posted::{VerifyReport, enforce_outcomes},
        verify_prompt::build_verify_request,
    },
};

/// Base backoff before the second attempt at one finding (#4459).
///
/// Why: the ladder has to outlast a provider's throttling window without
/// stalling a review — 250 / 500 / 1000 ms plus jitter spans the range a
/// transport blip or a 429 under fan-out clears in, and a default round of 3
/// attempts adds at most ~750 ms of sleep to a finding that eventually succeeds.
/// What: attempt *n* (1-based) sleeps `VERIFY_BACKOFF_BASE_MS << (n - 1)` plus
/// jitter; `VerifyPolicy::backoff_base_ms` overrides it, and `0` disables the
/// sleep entirely so tests exercise the ladder without waiting.
const VERIFY_BACKOFF_BASE_MS: u64 = 250;

/// How the round fans out and how hard it retries (#4459).
///
/// Why: the fan-out ceiling was a private constant and there was no retry at
/// all, so a review whose verifier calls hit transport errors had no way to
/// recover and an operator had no way to relieve the pressure. Both are now
/// resolved config (`[verification] concurrency` / `max_attempts`), and this
/// struct is the shape the round reads them through.
/// What: `concurrency` is the `buffer_unordered` width; `max_attempts` counts
/// TOTAL attempts per finding, so `1` means no retry; `backoff_base_ms` is the
/// first sleep in the exponential ladder. Both counts are clamped to ≥ 1 on
/// construction — a `0` would silently verify nothing.
/// #8904 adds `max_calls` (verifier requests per review), `batch_size`
/// (findings per request) and `per_file` (send each batch only its file's
/// diff sections — set by the map-reduce path, whose diff is over the cap).
/// Test: `verify_transient_failure_is_retried_until_it_succeeds`,
/// `verify_round_never_exceeds_the_configured_concurrency`,
/// `policy_from_config_clamps_zero_counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyPolicy {
    /// Verifier calls in flight at once.
    pub concurrency: usize,
    /// Total attempts per request, first call included.
    pub max_attempts: u32,
    /// Sleep before the second attempt; doubles each further attempt.
    pub backoff_base_ms: u64,
    /// Verifier requests per review; findings past it are withheld (#8904).
    pub max_calls: usize,
    /// Findings per verifier request (#8904).
    pub batch_size: usize,
    /// Send each batch only its own file's diff sections (#8904).
    pub per_file: bool,
}

impl Default for VerifyPolicy {
    /// The pre-#4459 fan-out width, plus the retry ladder that width needs.
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_VERIFY_CONCURRENCY,
            max_attempts: DEFAULT_VERIFY_MAX_ATTEMPTS,
            backoff_base_ms: VERIFY_BACKOFF_BASE_MS,
            max_calls: DEFAULT_VERIFY_MAX_CALLS,
            batch_size: DEFAULT_VERIFY_BATCH_SIZE,
            per_file: false,
        }
    }
}

impl VerifyPolicy {
    /// Read the policy out of resolved verification config.
    ///
    /// Why: `VerificationConfig` owns the env/file precedence; this is the only
    /// place the pipeline turns it into a fan-out decision.
    /// What: clamps every count to ≥ 1, keeps the default backoff base, and
    /// leaves `per_file` off.
    /// Test: `policy_from_config_clamps_zero_counts`.
    pub fn from_config(config: &crate::config::VerificationConfig) -> Self {
        Self {
            concurrency: config.concurrency.max(1),
            max_attempts: config.max_attempts.max(1),
            backoff_base_ms: VERIFY_BACKOFF_BASE_MS,
            max_calls: config.max_calls.max(1),
            batch_size: config.batch_size.max(1),
            per_file: false,
        }
    }

    /// Sleep before attempt `attempt` (1-based; attempt 1 never sleeps).
    ///
    /// Why: a fixed ladder makes every concurrent call retry in lockstep, which
    /// re-creates the burst that failed in the first place. Jitter spreads the
    /// retries of a fan-out across the window instead of stacking them.
    /// What: `backoff_base_ms << (attempt - 2)` plus up to 25% jitter drawn from
    /// the wall clock. Returns zero when `backoff_base_ms` is zero.
    /// Test: `backoff_grows_and_stays_within_its_jitter_band`.
    fn backoff(&self, attempt: u32) -> std::time::Duration {
        if self.backoff_base_ms == 0 || attempt < 2 {
            return std::time::Duration::ZERO;
        }
        let base = self.backoff_base_ms << (attempt - 2).min(6);
        // #4459: no `rand` in this crate's graph, and the jitter only has to
        // decorrelate concurrent retries — nanosecond clock skew between the
        // in-flight calls is enough for that.
        let jitter_span = (base / 4).max(1);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        std::time::Duration::from_millis(base + nanos % jitter_span)
    }
}

// ─── Runner seam ──────────────────────────────────────────────────────────────

/// Run the verification round if enabled and a verifier is wired.
///
/// Why: this is the single gating seam the runner calls so the enabled /
/// verifier-wired checks live with the rest of the verification logic.
/// What: when `config.verification.enabled` and a `verifier` provider is present,
/// runs [`run_verification_round_with_policy`] with the resolved verifier role
/// and `[verification]` policy (`per_file` from the caller) and returns its
/// report; otherwise returns `None` (findings and verdict untouched), logging
/// at debug when disabled and at warn when enabled with no verifier (#8904).
/// Test: `run_review_verification_disabled_skips_round`,
/// `run_review_enabled_without_a_verifier_withholds_every_finding`,
/// `run_review_posts_no_refuted_advisory_finding`.
pub async fn maybe_verify(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
    diff: &str,
    per_file: bool,
    verdict: Verdict,
    findings: &mut Vec<Finding>,
    author_rationale: Option<&str>,
) -> Option<VerifyReport> {
    if !config.verification.enabled {
        debug!("verification disabled by config — skipping round");
        return None;
    }
    let Some(verifier) = verifier else {
        // #4044: `gate_then_verify` withholds every finding.
        warn!("verification enabled but no verifier provider wired — withholding every finding");
        return None;
    };
    let role = &config.role_models.verifier;
    let policy = VerifyPolicy {
        per_file,
        ..VerifyPolicy::from_config(&config.verification)
    };
    let report = run_verification_round_with_policy(
        verifier,
        &role.model,
        diff,
        verdict,
        findings,
        Some(role.temperature),
        Some(role.max_tokens),
        author_rationale,
        policy,
    )
    .await;
    Some(report)
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Run the verification round and return the settled verdict.
///
/// Why: the stable seam for callers that want only the verdict.
/// What: [`run_verification_round_with_policy`] under the default policy,
/// returning `report.verdict`. `findings` loses every finding the round drops.
/// Test: `verify_confirmed_keeps_and_block_holds`,
/// `verify_refuted_drops_and_block_is_withheld`,
/// `verify_no_findings_is_noop`.
// 8 args: verifier + model + diff + verdict + findings + temp/tokens overrides +
// author_rationale (#1618).  Each is an independent input to the round.
#[allow(clippy::too_many_arguments)]
pub async fn run_verification_round(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
    diff: &str,
    primary_verdict: Verdict,
    findings: &mut Vec<Finding>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    author_rationale: Option<&str>,
) -> Verdict {
    run_verification_round_with_policy(
        verifier,
        verifier_model,
        diff,
        primary_verdict,
        findings,
        temperature,
        max_tokens,
        author_rationale,
        VerifyPolicy::default(),
    )
    .await
    .verdict
}

/// Verify every undecided finding under an explicit policy (#4459, #8904).
///
/// Why: #8904 — every finding about to be posted must pass the verifier, not
/// only the ones that could change the verdict.
/// What: [`select_candidates`] takes every finding with no recorded outcome;
/// [`plan_batches`] chunks them, highest impact first, into at most `policy.max_calls`
/// batches of `policy.batch_size`. Each batch is one verifier request (the
/// whole `diff`, or with `policy.per_file` only its findings' files' sections) retried per
/// `policy.max_attempts`. [`enforce_outcomes`] then records each outcome,
/// drops every non-CONFIRMED finding (refuted, unjudged, over-cap,
/// unverifiable, other), and settles the verdict. An UNKNOWN primary is still
/// verified: only its CONFIRMED findings are posted, and the verdict stays
/// UNKNOWN.
/// Test: `verify_transient_failure_is_retried_until_it_succeeds`,
/// `verify_permanent_transport_failure_is_withheld`,
/// `verify_round_never_exceeds_the_configured_concurrency`,
/// `verify_cap_withholds_findings_past_the_last_call`.
#[allow(clippy::too_many_arguments)]
pub async fn run_verification_round_with_policy(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
    diff: &str,
    primary_verdict: Verdict,
    findings: &mut Vec<Finding>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    author_rationale: Option<&str>,
    policy: VerifyPolicy,
) -> VerifyReport {
    let candidates = select_candidates(findings);
    let plan = plan_batches(findings, &candidates, policy.batch_size, policy.max_calls);
    info!(
        candidates = candidates.len(),
        total = findings.len(),
        calls = plan.batches.len(),
        over_cap = plan.over_cap.len(),
        primary = %primary_verdict,
        concurrency = policy.concurrency,
        "verification round: verifying every posted finding (#8904)"
    );

    // Each batch borrows its findings immutably to build its request; the
    // outcomes are applied afterwards, so no mutable borrow crosses an await.
    let calls = plan.batches.len();
    let per_batch: Vec<Vec<(usize, VerifyOutcome, VerifierReach)>> = stream::iter(plan.batches)
        .map(|batch| {
            let evidence = policy
                .per_file
                .then(|| file_diff_slice(diff, batch.iter().map(|&i| findings[i].file.as_str())))
                .flatten();
            let evidence = evidence.as_deref().unwrap_or(diff);
            let req = if let [only] = batch.as_slice() {
                build_verify_request(
                    verifier_model,
                    evidence,
                    &findings[*only],
                    temperature,
                    max_tokens,
                    author_rationale,
                )
            } else {
                let members: Vec<&Finding> = batch.iter().map(|&i| &findings[i]).collect();
                build_batch_request(
                    verifier_model,
                    evidence,
                    &members,
                    temperature,
                    max_tokens,
                    author_rationale,
                )
            };
            async move {
                let judged = verify_batch(verifier, req, batch.len(), policy).await;
                batch
                    .into_iter()
                    .zip(judged)
                    .map(|(idx, (outcome, reach))| (idx, outcome, reach))
                    .collect()
            }
        })
        .buffer_unordered(policy.concurrency.max(1))
        .collect()
        .await;
    let outcomes: Vec<_> = per_batch.into_iter().flatten().collect();

    let report = enforce_outcomes(
        primary_verdict.clone(),
        findings,
        outcomes,
        &plan.over_cap,
        calls,
    );
    info!(
        primary = %primary_verdict,
        final = %report.verdict,
        calls = report.calls,
        refuted = report.refuted,
        unjudged = report.unjudged,
        over_cap = report.over_cap,
        "verification round complete (#8904)"
    );
    report
}

/// Re-derive the verdict when the round dropped nothing (#8904).
///
/// Why: refuted, unjudged, over-cap and unverifiable findings are dropped and
/// the #8905 withhold policy settles those rounds (#4044), so from the round
/// this path sees only CONFIRMED findings, or none when an approving review
/// lost only advisory unverifiable ones. `derive_verdict` treats its baseline as a
/// lower bound, so passing the model's BLOCK unchanged would pin it even when
/// the confirmed evidence does not support it.
///
/// Three-way baseline selection:
///   a)  the confirmed findings alone floor to BLOCK under `derive_verdict`
///       (#4044) → keep `primary_verdict`.
///   a2) confirmed, but not BLOCK-grade (#1015 + #1343) → baseline is
///       `min(primary_verdict, APPROVE*)`. It caps the BASELINE, not the result:
///       `derive_verdict(baseline, findings)` still floors a confident Medium
///       to REQUEST_CHANGES (#1876), and a confirmed `praise` finding cannot
///       raise a clean APPROVE (#1343 runtime residual).
///   c)  nothing confirmed → the result is at least `primary_verdict`: no
///       judgment in the round supports relaxing it (#4459).
///
/// `UNKNOWN` is handled by the caller and never reaches here.
/// What: selects the baseline, calls `derive_verdict(baseline, findings)`, and
/// on path (c) raises the result to `primary_verdict`.
/// Test: `rederive_keeps_confirmed_block` (a),
/// `rederive_confirmed_medium_still_escalates_to_request_changes` (a2),
/// `rederive_confirmed_praise_keeps_clean_approve` (a2),
/// `rederive_unverifiable_only_preserves_primary_verdict` (c).
pub(crate) fn rederive_verdict(primary_verdict: Verdict, findings: &[Finding]) -> Verdict {
    // #4044: ask the grader, not a per-finding predicate — category caps
    // (#1359/#3474/#7036) must hold here too.
    let confirmed: Vec<Finding> = findings
        .iter()
        .filter(|f| matches!(f.verified, Some(VerifyOutcome::Confirmed)))
        .cloned()
        .collect();
    if confirmed.is_empty() {
        // Path (c).
        let rederived = derive_verdict(primary_verdict.clone(), findings);
        return verdict_max(rederived, primary_verdict);
    }
    let baseline = if derive_verdict(Verdict::Approve, &confirmed) == Verdict::Block {
        // Path (a): confirmed BLOCK-grade evidence supports the escalation.
        primary_verdict
    } else {
        // Path (a2).
        verdict_min(primary_verdict, Verdict::ApproveWithReservations)
    };
    derive_verdict(baseline, findings)
}

/// Return the *more severe* (severity-max) of two verdicts.
///
/// Why (#4459): the mirror of [`verdict_min`], used by path (c) so a round with
/// no confirmation can escalate on surviving evidence but never relax the
/// model's own verdict.
/// What: compares via `Verdict::ordinal`, the single source of truth shared with
/// `grade.rs`.
/// Test: `rederive_unverifiable_only_preserves_primary_verdict`.
fn verdict_max(a: Verdict, b: Verdict) -> Verdict {
    if a.ordinal() >= b.ordinal() { a } else { b }
}

/// Return the *less severe* (severity-min) of two verdicts.
///
/// Why: path (a2) of `rederive_verdict` must CAP the baseline at APPROVE* without
/// ever *raising* a clean APPROVE — confirming a Medium/Low-effort finding may relax
/// a floor-driven REQUEST_CHANGES (#1015) but must never harden an APPROVE the model
/// itself emitted (#1343 runtime residual).  `derive_verdict` already takes the
/// stricter-of(model, floor) downstream, so using the severity-min here is purely a
/// ceiling on the *baseline*, not on the final verdict.
/// What: defines the ordinal APPROVE(0) < APPROVE*(1) < REQUEST_CHANGES(2) <
/// BLOCK(3) and returns whichever of `a`/`b` has the lower ordinal.  `Unknown` is
/// terminal and never reaches here (the caller short-circuits it).
/// Test: `rederive_confirmed_medium_caps_at_approve_star` (REQUEST_CHANGES→APPROVE*),
/// `rederive_confirmed_praise_keeps_clean_approve` (APPROVE stays APPROVE — #1343).
fn verdict_min(a: Verdict, b: Verdict) -> Verdict {
    // #1357: compare via `Verdict::ordinal` (the single source of truth) instead
    // of a module-local copy of the ordinal table, so this module and `grade.rs`
    // can never drift apart.
    if a.ordinal() <= b.ordinal() { a } else { b }
}

// ─── Candidate selection ─────────────────────────────────────────────────────

/// Select the indices of findings to send to the verifier.
///
/// Why (#8904): every finding that is posted must pass the verifier. The
/// earlier rule sent only findings that could change the verdict (≥ 0.90 on an
/// APPROVE / APPROVE* review, ≥ 0.50 on a blocking one), which selected nothing
/// in 9 of 10 reviews on 0.36.1 and posted 7 of 7 fabrications unchecked.
/// What: returns the index of every finding with no recorded outcome, whatever
/// the verdict or confidence. A finding that ALREADY carries a `verified`
/// outcome is never a candidate (#4081): `claim_grounding` pre-stamps
/// `Unverifiable` on package-registry claims, and asking a second model with
/// the same stale training knowledge would launder the recollection into
/// `verified: "confirmed"`. Such a finding is posted with its "never verified"
/// caveat and cannot escalate.
/// Test: `select_candidates_takes_every_undecided_finding`,
/// `select_candidates_skips_findings_with_a_decided_outcome`.
pub fn select_candidates(findings: &[Finding]) -> Vec<usize> {
    findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.verified.is_none())
        .map(|(i, _)| i)
        .collect()
}

// ─── One verifier request ────────────────────────────────────────────────────

/// Verifier JSON output (forced via `response_schema`).
///
/// Why: the verifier is forced to emit `{judgment, reason}`; parsing it into a
/// typed struct lets the outcome mapping be exhaustive instead of string-sniffing.
/// What: `judgment` is `"CONFIRMED"` / `"REFUTED"` / `"UNVERIFIABLE"`; `reason`
/// is advisory.
/// Test: `verify_judgment_full_shape_deserializes`.
#[derive(Debug, Deserialize)]
struct VerifyJudgment {
    judgment: String,
    #[serde(default)]
    #[allow(dead_code)]
    reason: String,
}

/// Send one verifier request for `size` findings and map the answer to one
/// outcome per finding.
///
/// Why: the safety-critical error handling lives here. An alarm-class error
/// (#726) means the verifier model is broken: every finding gets
/// `ErrorRefuted` and the `verification_model_error` signal fires, never a
/// plain refutation. A transient error (#1876) is retried with backoff and
/// jitter up to `policy.max_attempts` (#4459), because the transport errors
/// that disabled this pass came from the round's own fan-out; an exhausted
/// budget records `Unverifiable` with the error class. An unparseable or
/// truncated answer records `TruncationRefuted`, and a batch answer that does
/// not judge a finding records it for that finding alone. Every one of these is
/// [`VerifierReach::Failed`]: nothing judged the finding, and #8904 withholds it.
/// What: a single-finding request is read by `parse_judgment`, a batch by
/// `parse_batch_judgments`. Returns exactly `size` entries in batch order.
/// Test: `verify_one_model_unavailable_emits_signal`,
/// `verify_truncated_response_is_withheld`,
/// `verify_transient_error_is_not_plain_refuted` (#1876),
/// `verify_transient_failure_is_retried_until_it_succeeds` (#4459),
/// `verify_permanent_transport_failure_is_withheld` (#4459, #8904),
/// `verify_batch_answer_missing_a_finding_withholds_only_that_finding`.
async fn verify_batch(
    verifier: &Arc<dyn LlmProvider>,
    req: crate::llm::LlmRequest,
    size: usize,
    policy: VerifyPolicy,
) -> Vec<(VerifyOutcome, VerifierReach)> {
    let model = req.model.clone();
    let attempts = policy.max_attempts.max(1);
    let mut last_class = String::new();
    for attempt in 1..=attempts {
        match verifier.complete(req.clone()).await {
            Ok(resp) => return judgments_for(&resp.text, size),
            Err(e) if e.is_alarm() => {
                // Config/lifecycle failure: loud, and deterministic, so never retried.
                let error_class = error_class(&e);
                emit_verification_model_error(&model, &error_class, &e);
                let outcome = VerifyOutcome::ErrorRefuted { error_class };
                return vec![(outcome, VerifierReach::Failed); size];
            }
            Err(e) => {
                last_class = error_class(&e);
                debug!(error_class = %last_class, "verifier call failed (retryable): {e}");
                if attempt < attempts {
                    let backoff = policy.backoff(attempt + 1);
                    warn!(
                        attempt,
                        max_attempts = attempts,
                        backoff_ms = backoff.as_millis(),
                        error_class = %last_class,
                        "verifier transient error — retrying (#4459)"
                    );
                    if !backoff.is_zero() {
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
    }
    warn!(
        attempts,
        error_class = %last_class,
        "verifier unreachable after every attempt — withholding the batch (#4459, #8904)"
    );
    let outcome = VerifyOutcome::Unverifiable {
        reason: format!(
            "the verifier could not be reached after {attempts} attempt(s) ({last_class}); \
             the finding was neither confirmed nor refuted"
        ),
    };
    vec![(outcome, VerifierReach::Failed); size]
}

/// Map a verifier answer to one outcome per finding.
///
/// What: CONFIRMED / REFUTED / UNVERIFIABLE (#5309) are `Judged`; an entry
/// the answer does not settle is `TruncationRefuted` and `Failed`.
fn judgments_for(text: &str, size: usize) -> Vec<(VerifyOutcome, VerifierReach)> {
    let parsed: Vec<Option<Judgment>> = if size == 1 {
        vec![parse_judgment(text)]
    } else {
        parse_batch_judgments(text, size)
            .into_iter()
            .map(|j| j.as_deref().and_then(judgment_from))
            .collect()
    };
    parsed
        .into_iter()
        .map(|judgment| match judgment {
            Some(Judgment::Confirmed) => (VerifyOutcome::Confirmed, VerifierReach::Judged),
            Some(Judgment::Refuted) => (VerifyOutcome::Refuted, VerifierReach::Judged),
            // #5309: the evidence is outside the diff — a judgment, not a failure.
            Some(Judgment::Unverifiable) => (
                VerifyOutcome::Unverifiable {
                    reason: VERIFIER_UNVERIFIABLE_REASON.to_string(),
                },
                VerifierReach::Judged,
            ),
            None => {
                warn!(
                    text = %truncate(text, 120),
                    "verifier answer did not judge a finding — withholding it (#8904)"
                );
                (VerifyOutcome::TruncationRefuted, VerifierReach::Failed)
            }
        })
        .collect()
}

/// Whether the verifier rendered a judgment on a finding (#8653).
///
/// Why: a retry-exhausted failure and a verifier-judged UNVERIFIABLE (#5309)
/// share one public `VerifyOutcome` variant; only the failure is withheld
/// (#8904). A typed flag tells them apart without widening the serialized enum
/// or reading the reason string.
/// What: `Judged` for any parseable answer; `Failed` for an alarm error, an
/// unparseable or truncated answer, or an exhausted retry budget.
/// Test: `verify_permanent_transport_failure_is_withheld`,
/// `verify_unverifiable_finding_is_withheld_not_posted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifierReach {
    /// The verifier answered with a parseable judgment.
    Judged,
    /// The verifier call failed or its answer was cut off.
    Failed,
}

/// Apply a verification outcome to a finding: record it and demote if refuted.
///
/// Why: the `verified` field records *why* a finding carries the weight it
/// does. #8904: the round drops every refuted or unjudged finding after this
/// runs (`verify_posted::enforce_outcomes`), counting and logging each drop;
/// the demotion still guards any other caller.
/// What: sets `finding.verified`; for any refutation variant
/// (`Refuted` / `ErrorRefuted` / `TruncationRefuted`) also clamps the confidence
/// down to `VERIFY_REFUTED_CONFIDENCE` (0.10), below every advisory / block gate.
/// For `Unverifiable` (#5309) applies
/// `evidence_admission::demote_to_unverifiable_advisory` instead — the finding is
/// not disproved, so its confidence is capped rather than floored, but it loses
/// `code_provable` and its High effort so it cannot drive the BLOCK floor. That
/// is the SAME demotion the hygiene passes apply when they pre-stamp
/// `Unverifiable`, so a claim carries the same weight whichever route classified
/// it. `Confirmed` and `Skipped` leave the finding untouched.
/// Test: `verify_confirmed_keeps_and_block_holds`,
/// `verify_refuted_drops_and_block_is_withheld`,
/// `apply_outcome_unverifiable_strips_block_floor_signals`.
pub fn apply_outcome(finding: &mut Finding, outcome: VerifyOutcome) {
    let is_refutation = matches!(
        outcome,
        VerifyOutcome::Refuted
            | VerifyOutcome::ErrorRefuted { .. }
            | VerifyOutcome::TruncationRefuted
    );
    if is_refutation {
        finding.confidence = VERIFY_REFUTED_CONFIDENCE;
    }
    // #5309: an unchecked claim must not wear the pipeline's escalation signals.
    // #5309: an unchecked claim must not wear the pipeline's escalation signals.
    if matches!(outcome, VerifyOutcome::Unverifiable { .. }) {
        crate::pipeline::evidence_admission::demote_to_unverifiable_advisory(finding);
    }
    finding.verified = Some(outcome);
}

/// Reason recorded when the VERIFIER itself declines to confirm (#5309).
///
/// Why: `VerifyOutcome::Unverifiable` carries a human-readable reason, and a
/// consumer must be able to tell "a hygiene pass pre-stamped this from the
/// finding's own admission" apart from "the verifier looked and said the
/// evidence is not in the diff".
/// What: the reason string for the latter case.
/// Test: `parse_judgment_unverifiable`,
/// `apply_outcome_unverifiable_strips_block_floor_signals`.
const VERIFIER_UNVERIFIABLE_REASON: &str = "the verifier could not settle this from the diff — the evidence it rests on is outside \
     the reviewed change";

/// A verifier judgment (#5309 made this tri-state; it was `bool` before).
///
/// Why: `Option<bool>` could express CONFIRMED / REFUTED / unparseable but had
/// no room for "the verifier examined it and could not tell", which is the whole
/// point of the third judgment.
/// What: mirrors the `judgment` enum in `verify_prompt::verify_response_schema`.
/// Test: `parse_judgment_unverifiable`,
/// `parse_judgment_truncated_refuted_json_is_refuted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgment {
    /// The finding is real and grounded in the diff.
    Confirmed,
    /// The finding is not defensible.
    Refuted,
    /// The evidence needed to settle the finding is not in the diff.
    Unverifiable,
}

/// Parse a single-finding verifier answer, or `None` when it judges nothing.
///
/// Why: the verifier output is forced JSON `{judgment, reason}`, but a
/// truncated answer or a provider that ignored the schema must fail closed
/// like the batched path (#8904): `{"judgment":"REFUTED","reason":"not
/// confirmed by` once parsed as CONFIRMED and was posted.
/// What: whole JSON first; then a structured `"judgment": "<X>"` token, which
/// outranks every keyword in the text around it; then [`keyword_judgment`].
/// Test: `parse_judgment_truncated_refuted_json_is_refuted`,
/// `parse_judgment_ambiguous_prose_never_confirms`,
/// `parse_judgment_unverifiable`.
fn parse_judgment(text: &str) -> Option<Judgment> {
    let trimmed = text.trim();
    if let Ok(j) = serde_json::from_str::<VerifyJudgment>(trimmed) {
        return judgment_from(&j.judgment);
    }
    if let Some(structured) = structured_judgment(trimmed) {
        return structured;
    }
    keyword_judgment(&trimmed.to_uppercase())
}

/// The `"judgment": "<X>"` value of an answer that is not whole JSON (#8904).
///
/// What: `None` when no `"judgment"` key appears. `Some(None)` when a key's
/// value is cut off or is not a judgment token, or when two keys disagree.
/// `Some(Some(j))` otherwise.
fn structured_judgment(text: &str) -> Option<Option<Judgment>> {
    const KEY: &str = "\"judgment\"";
    let mut found: Option<Option<Judgment>> = None;
    for (at, _) in text.match_indices(KEY) {
        let value = text[at + KEY.len()..]
            .trim_start()
            .strip_prefix(':')
            .and_then(|v| v.trim_start().strip_prefix('"'))
            .and_then(|v| v.split_once('"'))
            .and_then(|(token, _)| judgment_from(token));
        found = match found {
            None => Some(value),
            Some(prev) if prev == value => Some(prev),
            Some(_) => Some(None),
        };
    }
    found
}

/// Words that make a verdict keyword ambiguous (#8904; `UNREFUTED`, #9292).
const NEGATIONS: &[&str] = &[
    "NOT",
    "NO",
    "NOR",
    "NEVER",
    "CANNOT",
    "UNCONFIRMED",
    "UNREFUTED",
];

/// Characters that end the clause a negation scopes over (#9292).
const CLAUSE_BREAKS: &[char] = &[
    '.', ',', ';', ':', '!', '?', '\n', '(', ')', '\u{2013}', '\u{2014}',
];

/// Keyword fallback for a provider that ignored the forced schema (#8904).
///
/// Why: under toolChoice auto (#9292) the verifier can answer in free text, and
/// a substring match read "not REFUTED" as a refutation that dropped the finding.
/// What: UNVERIFIABLE wins, then REFUTED — each only as a whole word with no
/// negation earlier in its clause (see [`keyword_holds`]). CONFIRMED is returned
/// only when neither holds and the text carries no negation anywhere ("not
/// confirmed", "unconfirmed", "can't"). An ambiguous or negated answer is
/// `None`, which the round withholds as unjudged. `None` when no token appears.
/// Test: `parse_judgment_negated_verdict_is_not_that_verdict`,
/// `parse_judgment_plain_verdicts_unchanged`,
/// `parse_judgment_ambiguous_prose_never_confirms`.
fn keyword_judgment(upper: &str) -> Option<Judgment> {
    // #9292: a negated UNVERIFIABLE/REFUTED is not that verdict.
    if keyword_holds(upper, "UNVERIFIABLE") {
        return Some(Judgment::Unverifiable);
    }
    if keyword_holds(upper, "REFUTED") {
        return Some(Judgment::Refuted);
    }
    if !upper.contains("CONFIRMED") {
        return None;
    }
    let negated = words(upper).any(is_negation);
    (!negated).then_some(Judgment::Confirmed)
}

/// Whether `keyword` appears as a whole word and no occurrence follows a
/// negation in the same clause (#9292). Any negated occurrence makes it `false`.
fn keyword_holds(upper: &str, keyword: &str) -> bool {
    let mut seen = false;
    for clause in upper.split(CLAUSE_BREAKS) {
        let mut negated = false;
        for word in words(clause) {
            if word == keyword {
                if negated {
                    return false;
                }
                seen = true;
            }
            negated |= is_negation(word);
        }
    }
    seen
}

/// The words of `text`, keeping apostrophes so "CAN'T" stays one word.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .filter(|w| !w.is_empty())
}

/// Whether `word` (uppercase) negates a verdict keyword.
fn is_negation(word: &str) -> bool {
    NEGATIONS.contains(&word) || word.ends_with("N'T") || word.ends_with("N\u{2019}T")
}

/// Map an exact judgment token (any case, trimmed) to a [`Judgment`].
fn judgment_from(token: &str) -> Option<Judgment> {
    match token.trim().to_uppercase().as_str() {
        "CONFIRMED" => Some(Judgment::Confirmed),
        "REFUTED" => Some(Judgment::Refuted),
        "UNVERIFIABLE" => Some(Judgment::Unverifiable),
        _ => None,
    }
}

// The startup liveness gate (`LivenessDecision`, `probe_verifier_liveness`)
// lives in the sibling `verify_liveness` module to keep this file under the
// 500-line cap.  Re-export here so callers and the verify test module reach the
// whole verification API through one path.
pub use crate::pipeline::verify_liveness::{LivenessDecision, probe_verifier_liveness};

// ─── Signal emission (alarm hook) ────────────────────────────────────────────

/// Emit the `verification_model_error` signal.
///
/// Why: a broken verifier model is an operational incident that must be visible.
/// The signal is the stable, queryable event the alarm/metrics backend will key
/// off in Phase 7.
/// What: emits a structured `tracing::error!` with a stable `event` field and
/// the error class/model.  This is the *only* sink today.
///
/// TODO(#554, Phase 7): wire this to the real metrics/alarm backend (counter +
/// alarm). Do NOT build that backend here — this phase ships only the structured
/// log signal. Until #554 lands, operators alarm on the `event="verification_model_error"`
/// log line.
/// Test: `verify_one_model_unavailable_emits_signal` (asserts the outcome, which
/// is the observable side effect; the log line itself is side-effect-only).
pub(crate) fn emit_verification_model_error(model: &str, error_class: &str, err: &LlmError) {
    error!(
        event = "verification_model_error",
        model = %model,
        error_class = %error_class,
        error = %err,
        "verifier model error — verification integrity compromised (see #554 for alarm backend)"
    );
}

/// Map an `LlmError` to a short, stable error-class string for the signal.
///
/// Why: the `VerifyOutcome::ErrorRefuted` variant and the signal both carry an
/// error class; deriving it in one place keeps them consistent.
/// What: returns a stable PascalCase token per alarm-class variant.
/// Test: `error_class_maps_alarm_variants`.
pub(crate) fn error_class(err: &LlmError) -> String {
    match err {
        LlmError::ModelNotFound(_) => "ModelNotFound",
        LlmError::ModelNotReady(_) => "ModelNotReady",
        LlmError::Validation(_) => "Validation",
        LlmError::AccessDenied(_) => "AccessDenied",
        LlmError::Transport(_) => "Transport",
        LlmError::RateLimited => "RateLimited",
        LlmError::Upstream { .. } => "Upstream",
    }
    .to_string()
}

/// Truncate a string to `max` chars for safe logging.
///
/// Why: verifier output is short, but a misbehaving provider could return a wall
/// of text; we cap it before it reaches a log line.
/// What: returns up to `max` chars, appending `…` when truncated.
/// Test: side-effect-only logging helper; covered transitively.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max).collect();
        format!("{prefix}…")
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

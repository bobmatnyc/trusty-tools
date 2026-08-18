//! Per-finding verification round (Phase 2, #583).
//!
//! Why: the reviewer LLM over-fires — calibration showed REQUEST_CHANGES/BLOCK
//! verdicts driven by speculative findings that do not survive scrutiny.  A
//! second, cheaper LLM pass that confirms or refutes each candidate finding
//! cuts those false-positive blocking verdicts before they are posted.  This is
//! the trusty-review port of the code-intelligence verifier protocol.
//!
//! What: `run_verification_round` selects candidate findings (per the primary
//! verdict), verifies each concurrently against the verifier model with a strict
//! CONFIRMED / REFUTED judgment, demotes REFUTED findings below the advisory
//! tier (without dropping them — the outcome is recorded on the finding), and
//! re-derives the final verdict so a BLOCK whose only blocking finding was
//! refuted relaxes correctly.  `probe_verifier_liveness` is the startup gate that
//! refuses live mode when the verifier model is unavailable.
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
//! unreachable after the last attempt is recorded `Unverifiable` — counted by
//! `ReviewResult::unverified_count` — rather than `ErrorRefuted`, which reads as
//! a judgment nothing made.
//!
//! Test: `verify_tests.rs` — candidate selection, CONFIRMED/REFUTED outcomes,
//! verdict re-derivation, truncation regression (#726), transient-error
//! fail-open regression (#1876), fan-out retry / UNVERIFIED accounting (#4459),
//! and liveness-gate logic.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::{
    config::ReviewConfig,
    config::constants::{
        BLOCK_VERDICT_MIN_CONFIDENCE, VERIFY_CANDIDATE_MIN_CONFIDENCE, VERIFY_REFUTED_CONFIDENCE,
    },
    config::verification::{DEFAULT_VERIFY_CONCURRENCY, DEFAULT_VERIFY_MAX_ATTEMPTS},
    llm::{LlmError, LlmProvider},
    models::{Finding, Verdict, VerifyOutcome},
    pipeline::{
        grade::{derive_verdict, drives_block_floor},
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
/// Test: `verify_transient_failure_is_retried_until_it_succeeds`,
/// `verify_round_never_exceeds_the_configured_concurrency`,
/// `policy_from_config_clamps_zero_counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyPolicy {
    /// Verifier calls in flight at once.
    pub concurrency: usize,
    /// Total attempts per finding, first call included.
    pub max_attempts: u32,
    /// Sleep before the second attempt; doubles each further attempt.
    pub backoff_base_ms: u64,
}

impl Default for VerifyPolicy {
    /// The pre-#4459 fan-out width, plus the retry ladder that width needs.
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_VERIFY_CONCURRENCY,
            max_attempts: DEFAULT_VERIFY_MAX_ATTEMPTS,
            backoff_base_ms: VERIFY_BACKOFF_BASE_MS,
        }
    }
}

impl VerifyPolicy {
    /// Read the policy out of resolved verification config.
    ///
    /// Why: `VerificationConfig` owns the env/file precedence; this is the only
    /// place the pipeline turns it into a fan-out decision.
    /// What: clamps both counts to ≥ 1 and keeps the default backoff base.
    /// Test: `policy_from_config_clamps_zero_counts`.
    pub fn from_config(config: &crate::config::VerificationConfig) -> Self {
        Self {
            concurrency: config.concurrency.max(1),
            max_attempts: config.max_attempts.max(1),
            backoff_base_ms: VERIFY_BACKOFF_BASE_MS,
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

/// Run the verification round if enabled and a verifier is wired, else return
/// the verdict unchanged.
///
/// Why: this is the single gating seam the runner calls so the enabled /
/// verifier-wired checks live with the rest of the verification logic instead of
/// cluttering the orchestration loop.  Keeping it here also keeps `runner.rs`
/// under the 500-line cap.
/// What: when `config.verification.enabled` and a `verifier` provider is present,
/// delegates to `run_verification_round` with the resolved verifier role config;
/// otherwise logs why it was skipped and returns `verdict` unchanged (findings
/// untouched).
/// Test: runner-level `run_review_verification_*` tests; the disabled path is
/// `run_review_verification_disabled_skips_round`.
pub async fn maybe_verify(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
    diff: &str,
    verdict: Verdict,
    findings: &mut [Finding],
    author_rationale: Option<&str>,
) -> Verdict {
    if !config.verification.enabled {
        debug!("verification disabled by config — skipping round");
        return verdict;
    }
    let Some(verifier) = verifier else {
        debug!("verification enabled but no verifier provider wired — skipping");
        return verdict;
    };
    let role = &config.role_models.verifier;
    run_verification_round_with_policy(
        verifier,
        &role.model,
        diff,
        verdict,
        findings,
        Some(role.temperature),
        Some(role.max_tokens),
        author_rationale,
        VerifyPolicy::from_config(&config.verification),
    )
    .await
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Run the per-finding verification round and return the re-derived verdict.
///
/// Why: this is the single seam the runner calls between verdict parse and
/// finalisation.  It mutates `findings` in place (recording each outcome and
/// demoting refuted findings) and returns the verdict re-derived from the
/// post-verification confidence distribution, so a blocking verdict whose only
/// blocking finding was refuted correctly relaxes.
/// What: selects candidates via `select_candidates`, verifies each concurrently
/// (bounded), applies the outcome (CONFIRMED keeps confidence, REFUTED demotes
/// below the advisory tier), then returns `derive_verdict(primary, findings)`.
/// When there are no candidates the findings are left untouched and the primary
/// verdict is re-derived unchanged.
/// Test: `verify_confirmed_keeps_and_block_holds`,
/// `verify_refuted_demotes_and_block_relaxes`,
/// `verify_no_candidates_is_noop`.
// 8 args: verifier + model + diff + verdict + findings + temp/tokens overrides +
// author_rationale (#1618).  Each is an independent input to the round; bundling
// them into a struct would add indirection without clarity for a single caller.
#[allow(clippy::too_many_arguments)]
pub async fn run_verification_round(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
    diff: &str,
    primary_verdict: Verdict,
    findings: &mut [Finding],
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
}

/// Run the verification round under an explicit fan-out policy (#4459).
///
/// Why: [`run_verification_round`] is the stable seam its existing callers use;
/// this is the same round with the concurrency ceiling and retry budget passed
/// in, so `maybe_verify` can honour operator config and a test can run the
/// ladder with the sleep set to zero.
/// What: identical to [`run_verification_round`] except that `policy` replaces
/// the defaults.
/// Test: `verify_transient_failure_is_retried_until_it_succeeds`,
/// `verify_permanent_transport_failure_lands_in_unverified`,
/// `verify_round_never_exceeds_the_configured_concurrency`.
#[allow(clippy::too_many_arguments)]
pub async fn run_verification_round_with_policy(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
    diff: &str,
    primary_verdict: Verdict,
    findings: &mut [Finding],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    author_rationale: Option<&str>,
    policy: VerifyPolicy,
) -> Verdict {
    // UNKNOWN is terminal — the diff was unassessable, so there is nothing to
    // verify and no verdict to re-derive.
    if primary_verdict == Verdict::Unknown {
        return Verdict::Unknown;
    }

    let candidate_idxs = select_candidates(primary_verdict.clone(), findings);
    if candidate_idxs.is_empty() {
        // Nothing was verified — leave findings and verdict exactly as graded.
        debug!("verification: no candidate findings — verdict unchanged");
        return primary_verdict;
    }

    info!(
        candidates = candidate_idxs.len(),
        total = findings.len(),
        primary = %primary_verdict,
        concurrency = policy.concurrency,
        max_attempts = policy.max_attempts,
        "verification round: verifying candidate findings"
    );

    // Verify candidates concurrently (bounded).  Each task borrows the finding
    // immutably to build its request; the outcome is applied afterwards so we
    // never hold a mutable borrow across the await points.
    let outcomes: Vec<(usize, VerifyOutcome)> = stream::iter(candidate_idxs)
        .map(|idx| {
            let req = build_verify_request(
                verifier_model,
                diff,
                &findings[idx],
                temperature,
                max_tokens,
                author_rationale,
            );
            async move {
                let outcome = verify_one(verifier, req, policy).await;
                (idx, outcome)
            }
        })
        .buffer_unordered(policy.concurrency.max(1))
        .collect()
        .await;

    // Apply outcomes: record on the finding and demote refuted ones.
    let mut any_confirmed = false;
    let mut any_clean_refuted = false;
    let mut unverified = 0usize;
    for (idx, outcome) in outcomes {
        match &outcome {
            VerifyOutcome::Confirmed => any_confirmed = true,
            VerifyOutcome::Refuted => any_clean_refuted = true,
            _ => {}
        }
        if outcome.is_unverified() {
            unverified += 1;
        }
        apply_outcome(&mut findings[idx], outcome);
    }

    // Re-derive the verdict from the SURVIVING findings (refuted ones excluded).
    let final_verdict = rederive_verdict(
        primary_verdict.clone(),
        any_confirmed,
        any_clean_refuted,
        findings,
    );
    info!(
        primary = %primary_verdict,
        final = %final_verdict,
        any_confirmed,
        any_clean_refuted,
        unverified,
        "verification round complete — verdict re-derived"
    );
    final_verdict
}

/// Re-derive the final verdict from the surviving (non-refuted) findings.
///
/// Why: refuted findings can no longer justify a blocking verdict; `derive_verdict`
/// treats its model_proposed as a lower bound, so always passing the original
/// BLOCK would pin the result even when every blocking finding was refuted.
///
/// Four-way baseline selection:
///   a)  confirmed + at least one confirmed High-effort finding
///       → keep `primary_verdict` (grounded critical evidence, e.g. BLOCK stays BLOCK)
///   a2) confirmed, but only Medium/Low-effort findings confirmed (#1015 + #1343)
///       → CAP (ceiling) the baseline at APPROVE*: `min(primary_verdict, APPROVE*)`.
///         BUT this is a ceiling on the *baseline input*, not on the final result:
///         `derive_verdict(baseline, survivors)` below independently re-derives the
///         severity floor from the surviving findings, and as of #1876 a single
///         confirmed Medium finding with confidence > FLOOR_MIN_CONFIDENCE (0.80)
///         floors to REQUEST_CHANGES on its own merits (see
///         `grade::correctness_floor`) — so `stricter_of(baseline, floor)` still
///         lands on REQUEST_CHANGES even though the *baseline* was capped at
///         APPROVE*.  The cap's remaining purpose is narrower after #1876: it
///         still stops a confirmed non-High finding from *raising* a clean model
///         APPROVE baseline to APPROVE* on its own (#1343 runtime residual), and it
///         still provides a floor of APPROVE* for a confirmed finding that does
///         NOT individually clear the severity-floor confidence gate.
///   b)  clean model REFUTED, nothing confirmed
///       → drop to APPROVE baseline (escalation rested on refuted evidence)
///   c)  nothing was confirmed and nothing was cleanly refuted — every outcome
///       was an infra / unable-to-verify failure (TruncationRefuted,
///       ErrorRefuted, or, since #4459, `Unverifiable` after an exhausted retry
///       budget) → preserve `primary_verdict` (do not fail-open to APPROVE on
///       verifier infra failure, #726 + #1876). Since #4459 that preservation is
///       enforced on the RESULT (`verdict_max` against `primary_verdict`), not
///       just handed to `derive_verdict` as a baseline: an `Unverifiable`
///       finding SURVIVES into the survivor set, and an all-advisory survivor
///       set trips `grade`'s low-confidence collapse, which dissolved the
///       model's own BLOCK. Surviving findings may still escalate.
///
/// `UNKNOWN` is handled by the caller and never reaches here.
/// What: filters survivors (non-refuted), selects baseline (path a2 takes the
/// severity-min of `primary_verdict` and APPROVE*), calls
/// `derive_verdict(baseline, survivors)`.
/// Test: `rederive_excludes_refuted_relaxes` (b), `rederive_keeps_confirmed_block` (a),
/// `rederive_confirmed_medium_still_escalates_to_request_changes` (a2 — #1876,
/// supersedes the pre-#1876 `..._caps_at_approve_star` expectation),
/// `rederive_confirmed_praise_keeps_clean_approve` (a2 — #1343 runtime residual),
/// `rederive_refuted_finding_does_not_clear_standing_medium_finding` (a2 — #1876),
/// `rederive_error_refuted_preserves_primary_verdict` (c — #726),
/// `rederive_truncation_refuted_preserves_primary_verdict` (c).
fn rederive_verdict(
    primary_verdict: Verdict,
    any_confirmed: bool,
    any_clean_refuted: bool,
    findings: &[Finding],
) -> Verdict {
    let survivors: Vec<Finding> = findings
        .iter()
        .filter(|f| {
            !matches!(
                f.verified,
                Some(VerifyOutcome::Refuted)
                    | Some(VerifyOutcome::ErrorRefuted { .. })
                    | Some(VerifyOutcome::TruncationRefuted)
            )
        })
        .cloned()
        .collect();

    // Does any confirmed (surviving) finding drive the BLOCK floor — i.e. is it
    // High-effort AND escalation-eligible (cited or diff-provable)?
    //
    // #PR84 adversarial-review follow-up: this previously used a bare
    // `f.effort == Effort::High` check, so a CONFIRMED-but-disqualified (uncited,
    // non-diff-provable) High finding — exactly PR #84's shape post-verification
    // (`verified: Confirmed`, no citation) — routed to path (a) below and pinned
    // `primary_verdict` (e.g. a self-reported BLOCK) as a HARD floor.
    // `derive_verdict`'s own #PR84 gate (in `grade.rs`) already prevents that
    // baseline from surviving as an outright ungated BLOCK, but path (a) vs (a2)
    // selection should agree with the unified path's citability rule on its own
    // merits — using `drives_block_floor` here keeps this call site consistent
    // with `correctness_floor` / the map-reduce synthesis floor rather than
    // relying solely on the downstream `derive_verdict` safety net.
    let any_confirmed_high = survivors
        .iter()
        .filter(|f| matches!(f.verified, Some(VerifyOutcome::Confirmed)))
        .any(drives_block_floor);

    // Four-way baseline selection (see Why above):
    //  a)  confirmed + at least one High-effort confirmed
    //      → keep primary_verdict as lower bound (grounded critical evidence)
    //  a2) confirmed, but only Medium/Low confirmed
    //      → CAP the baseline at APPROVE* via severity-min(primary, APPROVE*); don't
    //         let a floor-driven REQUEST_CHANGES pin the verdict when the confirmed
    //         finding is merely Medium-effort (#1015), and don't let a confirmed
    //         non-High finding *raise* a clean APPROVE to APPROVE* (#1343 residual).
    //  b)  clean refuted, nothing confirmed
    //      → drop to APPROVE; let survivors alone decide
    //  c)  infra-only fail (TruncationRefuted / ErrorRefuted), nothing confirmed
    //      → preserve primary_verdict (don't discard on infra failure #726)
    // #4459: path (c) below says "preserve primary_verdict", and until now it
    // said so by passing `primary_verdict` as the baseline and trusting
    // `derive_verdict` to treat it as a lower bound. That holds only while the
    // survivor set is empty. Once an unable-to-verify finding SURVIVES — which
    // is what `Unverifiable` does, unlike the refutation variants — an
    // all-advisory survivor set trips `grade`'s low-confidence collapse and
    // dissolves the model's own BLOCK, which is the fail-open this issue is
    // about, arriving by a new route. Remember whether this round rendered any
    // judgment at all; nothing may relax the verdict when nothing did.
    let no_judgment_rendered = !any_confirmed && !any_clean_refuted;

    let baseline = if any_confirmed && any_confirmed_high {
        // Path (a): confirmed High-effort evidence supports the escalation fully.
        primary_verdict.clone()
    } else if any_confirmed {
        // Path (a2): confirmed evidence, but only Medium/Low tier.  Take the
        // severity-MIN of the model's own verdict and APPROVE* (the advisory tier)
        // as the BASELINE (not the final answer — see the Why above for #1876):
        //   - primary=REQUEST_CHANGES/BLOCK → baseline capped down to APPROVE*
        //     (#1015); `derive_verdict` below still re-escalates to REQUEST_CHANGES
        //     when the surviving confirmed Medium clears FLOOR_MIN_CONFIDENCE (#1876).
        //   - primary=APPROVE → stays APPROVE (#1343 runtime residual): confirming a
        //     low-effort `praise` finding must NOT harden the verdict to APPROVE* nor
        //     downgrade the grade.  This is the same source-of-truth reconciliation
        //     grade.rs applies — the model's APPROVE review_body is authoritative.
        // `derive_verdict(baseline, survivors)` will still escalate further if the
        // surviving findings warrant it (e.g. a surviving High → BLOCK, or as of
        // #1876 a surviving confident Medium → REQUEST_CHANGES).
        verdict_min(primary_verdict.clone(), Verdict::ApproveWithReservations)
    } else if any_clean_refuted {
        // Path (b): at least one clean REFUTED from the model — escalation rested
        // on refuted evidence; let survivors alone decide.
        Verdict::Approve
    } else {
        // Path (c): all demotions were infrastructure failures (TruncationRefuted /
        // ErrorRefuted) — preserve the model's escalation rather than silently
        // collapsing to APPROVE due to verifier infra failure.
        primary_verdict.clone()
    };

    let rederived = derive_verdict(baseline, &survivors);
    if no_judgment_rendered {
        // Path (c): the round reached no judgment on anything. Surviving
        // findings may still ESCALATE the verdict, but none of them may lower
        // what the model itself said.
        return verdict_max(rederived, primary_verdict);
    }
    rederived
}

/// Return the *more severe* (severity-max) of two verdicts.
///
/// Why (#4459): the mirror of [`verdict_min`], used by path (c) so an
/// infra-only round can escalate on surviving evidence but can never relax the
/// model's own verdict — nothing in that round examined anything.
/// What: compares via `Verdict::ordinal`, the single source of truth shared with
/// `grade.rs`.
/// Test: `verify_permanent_transport_failure_lands_in_unverified`,
/// `rederive_error_refuted_preserves_primary_verdict`.
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

/// Select the indices of findings to send to the verifier for a given verdict.
///
/// Why: verifying every finding is wasteful; the candidate set depends on the
/// primary verdict (#583 work item (b)).  On a blocking verdict we cast a wide
/// net — any finding ≥ `VERIFY_CANDIDATE_MIN_CONFIDENCE` could be the sole reason
/// the verdict escalated, so each must be confirmed before it is allowed to
/// drive a block.  On an approving verdict only the blocking-tier findings (the
/// ones that could *escalate* if confirmed) are worth the verifier's time.
/// What: returns indices into `findings`.  For REQUEST_CHANGES / BLOCK: every
/// finding with `confidence >= VERIFY_CANDIDATE_MIN_CONFIDENCE` (0.50).  For
/// APPROVE / APPROVE*: only findings with `confidence >= BLOCK_VERDICT_MIN_CONFIDENCE`
/// (0.90).  UNKNOWN never reaches here (handled by the caller).
///
/// A finding that ALREADY carries a `verified` outcome is never a candidate
/// (#4081).  Nothing set that field before the round until `claim_grounding`
/// began pre-stamping `Unverifiable` on package-registry claims the pipeline
/// cannot check; sending one to the verifier would let a second model with the
/// same stale training knowledge launder the recollection into
/// `verified: "confirmed"` — the exact trust-signal inversion #4081 reports.
/// The rule is stated generally rather than as an `Unverifiable` special case:
/// an outcome that is already decided is not a question worth re-asking.
/// Test: `select_candidates_block_uses_wide_net`,
/// `select_candidates_approve_uses_block_tier_only`,
/// `select_candidates_skips_findings_with_a_decided_outcome`.
pub fn select_candidates(primary_verdict: Verdict, findings: &[Finding]) -> Vec<usize> {
    let floor = match primary_verdict {
        Verdict::RequestChanges | Verdict::Block => VERIFY_CANDIDATE_MIN_CONFIDENCE,
        Verdict::Approve | Verdict::ApproveWithReservations => BLOCK_VERDICT_MIN_CONFIDENCE,
        // UNKNOWN is filtered before this is called; treat defensively as "no
        // candidates" so a stray UNKNOWN never triggers verifier calls.
        Verdict::Unknown => return Vec::new(),
    };
    findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.verified.is_none() && f.confidence >= floor)
        .map(|(i, _)| i)
        .collect()
}

// ─── Single-finding verification ─────────────────────────────────────────────

/// Verifier JSON output (forced via `response_schema`).
///
/// Why: the verifier is forced to emit `{judgment, reason}`; parsing it into a
/// typed struct lets the outcome mapping be exhaustive instead of string-sniffing.
/// What: `judgment` is `"CONFIRMED"` / `"REFUTED"`; `reason` is advisory.
/// Test: covered by `verify_one` behaviour in `verify_tests.rs`.
#[derive(Debug, Deserialize)]
struct VerifyJudgment {
    judgment: String,
    #[serde(default)]
    #[allow(dead_code)]
    reason: String,
}

/// Verify one finding and map the provider result to a `VerifyOutcome`.
///
/// Why: this is where the safety-critical error handling lives.  A config/
/// lifecycle error (`is_alarm`) from the verifier model must NOT be silently
/// swallowed as a plain refutation — that is exactly the incident this phase
/// guards against.  Such errors map to `ErrorRefuted { error_class }` AND emit
/// the `verification_model_error` signal.  An unparseable/truncated response maps
/// to `TruncationRefuted` (distinct from a clean model `Refuted`) so
/// `rederive_verdict` can tell apart "the model said REFUTED" from "the provider
/// returned garbage", and preserve the model's escalation in the latter case.
/// What: calls the verifier, parses the forced JSON judgment, and returns
/// `Confirmed` / `Refuted` accordingly.  On an alarm-class `LlmError`, emits the
/// signal and returns `ErrorRefuted`.  On a transient error ALSO returns
/// `ErrorRefuted` (#1876 — see the Why below), never plain `Refuted`, because a
/// transient fault is "unable to verify", not "the model refuted this".  On a
/// successful call that returns unparseable output returns `TruncationRefuted`
/// (structurally distinct from a clean REFUTED judgment).
///
/// #1876 fail-open fix: prior to this change, a transient error (rate limit,
/// transport blip, upstream 5xx) mapped to plain `VerifyOutcome::Refuted` —
/// structurally identical to a clean model REFUTED judgment. That made
/// `rederive_verdict` treat "we could not reach the verifier" the same as "the
/// verifier examined this and found it wrong": both set `any_clean_refuted =
/// true`, which collapses the review's baseline to APPROVE (path b) when
/// nothing else was confirmed. A shadow-eval showed this fail-open behavior
/// contributing to REQUEST_CHANGES being silently downgraded to APPROVE. Mapping
/// transient errors to `ErrorRefuted` instead routes them through
/// `rederive_verdict` path (c) — "unable to verify" — which PRESERVES
/// `primary_verdict` rather than discarding it, matching the existing #726
/// treatment of config/lifecycle errors and truncated responses. The finding
/// itself is still excluded from the severity floor either way (an unverified
/// finding must not drive escalation on its own); only the *fail-open-to-APPROVE*
/// side effect on the surrounding review is fixed.
/// #4459 retry + honest UNVERIFIED: a transient error is now retried up to
/// `policy.max_attempts` times with exponential backoff and jitter before any
/// outcome is recorded, because the transport errors that disabled this pass in
/// production came from the round's OWN fan-out — a single call to the same
/// model in isolation succeeded in ~845 ms while 27 of 29 findings in a
/// concurrent round came back `Transport`. When the budget is exhausted the
/// finding is recorded as `Unverifiable`, NOT `ErrorRefuted`: nothing examined
/// it, so calling the result a refutation (and clamping its confidence to 0.10
/// as `apply_outcome` does for every refutation variant) states a judgment the
/// pipeline never made. `Unverifiable` keeps the finding visible as an advisory
/// that cannot escalate, and `ReviewResult::unverified_count` reports how many
/// there were. Alarm-class errors keep the `ErrorRefuted` + alarm treatment from
/// #726 — a broken deployment is a different fact from an unreachable call, and
/// retrying a deterministic ModelNotFound only delays the alarm.
/// Test: `verify_one_confirmed`, `verify_one_refuted`,
/// `verify_one_model_unavailable_emits_signal`,
/// `verify_truncated_response_is_truncation_refuted`,
/// `verify_transient_error_is_not_plain_refuted` (#1876),
/// `verify_transient_failure_is_retried_until_it_succeeds` (#4459),
/// `verify_permanent_transport_failure_lands_in_unverified` (#4459).
async fn verify_one(
    verifier: &Arc<dyn LlmProvider>,
    req: crate::llm::LlmRequest,
    policy: VerifyPolicy,
) -> VerifyOutcome {
    let model = req.model.clone();
    let attempts = policy.max_attempts.max(1);
    let mut last_class = String::new();
    for attempt in 1..=attempts {
        match attempt_verify(verifier, req.clone(), &model).await {
            Ok(outcome) => return outcome,
            Err(AttemptError::Alarm(outcome)) => return outcome,
            Err(AttemptError::Transient(class)) => {
                last_class = class;
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
        "verifier unreachable after every attempt — recording the finding as UNVERIFIED (#4459)"
    );
    VerifyOutcome::Unverifiable {
        reason: format!(
            "the verifier could not be reached after {attempts} attempt(s) ({last_class}); \
             the finding was neither confirmed nor refuted"
        ),
    }
}

/// Why one attempt failed, when it did (#4459).
///
/// Why: the retry loop must tell "retry this" from "stop now, the deployment is
/// broken" without re-deriving the classification the alarm branch already made.
/// What: `Alarm` carries the terminal outcome to return as-is; `Transient`
/// carries the error class for the log line and the eventual reason string.
enum AttemptError {
    Alarm(VerifyOutcome),
    Transient(String),
}

/// One verifier call, classified.
///
/// Why: keeps `verify_one`'s loop readable — the response-parsing arms are the
/// same on every attempt and only the error arms decide whether to loop.
/// What: `Ok` on any response the pipeline can act on (including a truncated
/// one, which is a provider fault the same request will reproduce); `Err` on a
/// call that failed.
/// Test: covered through `verify_one` by the tests it lists.
async fn attempt_verify(
    verifier: &Arc<dyn LlmProvider>,
    req: crate::llm::LlmRequest,
    model: &str,
) -> Result<VerifyOutcome, AttemptError> {
    match verifier.complete(req).await {
        Ok(resp) => Ok(match parse_judgment(&resp.text) {
            Some(Judgment::Confirmed) => VerifyOutcome::Confirmed,
            Some(Judgment::Refuted) => VerifyOutcome::Refuted,
            // #5309: the verifier declined to confirm a claim the diff cannot
            // settle.  Not a refutation — the finding may well be real — so it
            // survives as an advisory note rather than being disproved.
            Some(Judgment::Unverifiable) => VerifyOutcome::Unverifiable {
                reason: VERIFIER_UNVERIFIABLE_REASON.to_string(),
            },
            None => {
                warn!(
                    text = %truncate(&resp.text, 120),
                    "verifier returned unparseable/truncated judgment — recording TruncationRefuted"
                );
                // Use a structurally distinct variant so rederive_verdict can
                // distinguish "model said REFUTED" from "provider returned garbage".
                VerifyOutcome::TruncationRefuted
            }
        }),
        Err(e) if e.is_alarm() => {
            // Config/lifecycle failure: the verifier model is broken.  This is
            // the incident path — make it loud, do not pretend the finding was
            // refuted on its merits.  Deterministic, so it is never retried.
            let error_class = error_class(&e);
            emit_verification_model_error(model, &error_class, &e);
            Err(AttemptError::Alarm(VerifyOutcome::ErrorRefuted {
                error_class,
            }))
        }
        Err(e) => {
            // Transient failure (#1876, #4459): we could not verify this
            // finding, but the deployment is not broken and the model never
            // rendered a judgment.  Hand it back to the retry ladder; only an
            // exhausted budget decides an outcome.  This is not an alarm-worthy
            // incident (no emit_verification_model_error call): rate limits and
            // transport blips are expected operational noise.
            let error_class = error_class(&e);
            debug!(
                error_class = %error_class,
                "verifier call failed (retryable): {e}"
            );
            Err(AttemptError::Transient(error_class))
        }
    }
}

/// Apply a verification outcome to a finding: record it and demote if refuted.
///
/// Why: the spec (REV-606) forbids silently dropping a refuted finding — its
/// outcome must stay on the result for transparency.  Demoting the confidence
/// (rather than deleting the finding) makes `derive_verdict` treat it as noise
/// while the `verified` field records *why*.
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
/// `verify_refuted_demotes_and_block_relaxes`,
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
/// Test: `parse_judgment_confirmed`, `parse_judgment_refuted`,
/// `parse_judgment_unverifiable`, `parse_judgment_unparseable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgment {
    /// The finding is real and grounded in the diff.
    Confirmed,
    /// The finding is not defensible.
    Refuted,
    /// The evidence needed to settle the finding is not in the diff.
    Unverifiable,
}

/// Parse the verifier's forced JSON judgment, or `None` if unparseable.
///
/// Why: the verifier output is forced JSON `{judgment, reason}`; a robust parse
/// (with a keyword fallback for non-structured providers) keeps the outcome
/// deterministic.
/// What: tries direct JSON deserialisation first; falls back to a case-insensitive
/// keyword scan so a provider that ignored the schema still produces a decision.
/// UNVERIFIABLE is scanned FIRST in the fallback because it is the only token
/// that could be swallowed by a substring match on another — a prose answer
/// reading "not confirmed, unverifiable from this diff" contains both tokens, and
/// the safe reading of an ambiguous answer is the one that does not confirm.
/// Returns `None` only when no token appears.
/// Test: `parse_judgment_confirmed`, `parse_judgment_refuted`,
/// `parse_judgment_unverifiable`, `parse_judgment_unparseable`.
fn parse_judgment(text: &str) -> Option<Judgment> {
    let trimmed = text.trim();
    if let Ok(j) = serde_json::from_str::<VerifyJudgment>(trimmed) {
        return match j.judgment.trim().to_uppercase().as_str() {
            "CONFIRMED" => Some(Judgment::Confirmed),
            "REFUTED" => Some(Judgment::Refuted),
            "UNVERIFIABLE" => Some(Judgment::Unverifiable),
            _ => None,
        };
    }
    // Fallback keyword scan for providers that ignored the forced schema.
    let upper = trimmed.to_uppercase();
    if upper.contains("UNVERIFIABLE") {
        return Some(Judgment::Unverifiable);
    }
    if upper.contains("CONFIRMED") {
        return Some(Judgment::Confirmed);
    }
    if upper.contains("REFUTED") {
        return Some(Judgment::Refuted);
    }
    None
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

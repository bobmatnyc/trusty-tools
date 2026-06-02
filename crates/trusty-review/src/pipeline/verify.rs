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
//! ## Why the liveness gate exists (incident rationale)
//! code-intelligence suffered a production incident where a **stale verifier
//! model** (an inference profile that had been deactivated) caused every
//! verification call to fail with `ResourceNotFoundException`.  Those errors were
//! swallowed as REFUTED, so EVERY finding was silently auto-refuted and EVERY
//! review collapsed to APPROVE — the reviewer was effectively neutered with no
//! visible signal.  The startup liveness probe (`probe_verifier_liveness`) exists
//! specifically to make that failure mode loud: in live mode we refuse to start
//! when the verifier model is dead, rather than running a defanged reviewer.
//!
//! Test: see `verify_tests.rs` — candidate selection per verdict, CONFIRMED
//! keeps / REFUTED demotes, verdict re-derivation after demotion, and the
//! liveness-gate decision logic with an injected model-unavailable provider.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::{
    config::ReviewConfig,
    config::constants::{
        BLOCK_VERDICT_MIN_CONFIDENCE, VERIFY_CANDIDATE_MIN_CONFIDENCE, VERIFY_REFUTED_CONFIDENCE,
    },
    llm::{LlmError, LlmProvider},
    models::{Finding, Verdict, VerifyOutcome},
    pipeline::{grade::derive_verdict, verify_prompt::build_verify_request},
};

/// Maximum number of verifier calls to run concurrently.
///
/// Why: verifications are independent per finding, so running them concurrently
/// cuts wall-clock latency; the bound caps provider concurrency so a PR with
/// many findings does not burst the verifier model's rate limit.
/// What: `buffer_unordered(VERIFY_CONCURRENCY)` over the candidate stream.
const VERIFY_CONCURRENCY: usize = 4;

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
    run_verification_round(
        verifier,
        &role.model,
        diff,
        verdict,
        findings,
        Some(role.temperature),
        Some(role.max_tokens),
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
pub async fn run_verification_round(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
    diff: &str,
    primary_verdict: Verdict,
    findings: &mut [Finding],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
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
            );
            async move {
                let outcome = verify_one(verifier, req).await;
                (idx, outcome)
            }
        })
        .buffer_unordered(VERIFY_CONCURRENCY)
        .collect()
        .await;

    // Apply outcomes: record on the finding and demote refuted ones.  Track
    // whether ANY candidate was confirmed — that decides whether the model's
    // original escalation is still grounded (see `rederive_verdict`).
    let mut any_confirmed = false;
    for (idx, outcome) in outcomes {
        if matches!(outcome, VerifyOutcome::Confirmed) {
            any_confirmed = true;
        }
        apply_outcome(&mut findings[idx], outcome);
    }

    // Re-derive the verdict from the SURVIVING findings (refuted ones excluded).
    let final_verdict = rederive_verdict(primary_verdict.clone(), any_confirmed, findings);
    info!(
        primary = %primary_verdict,
        final = %final_verdict,
        any_confirmed,
        "verification round complete — verdict re-derived"
    );
    final_verdict
}

/// Re-derive the final verdict from the surviving (non-refuted) findings.
///
/// Why: after verification, the *surviving* findings are the ground truth — a
/// refuted finding can no longer justify a blocking verdict.  Two facts make a
/// naive `derive_verdict` call insufficient:
///   1. the severity floor is keyed on a finding's `Effort`, so a refuted
///      High-effort finding would still force a BLOCK floor on its tier alone;
///   2. `derive_verdict` also treats its `model_proposed` argument as a lower
///      bound, so always passing the original BLOCK would pin the result at
///      BLOCK even when every blocking finding was refuted.
///
/// The rule, designed to satisfy the ticket's example ("a BLOCK whose only
/// blocking finding was REFUTED should fall back") without throwing away
/// legitimate model escalation on *confirmed* findings:
///   - Always exclude refuted findings from the floor computation.
///   - If at least one candidate was CONFIRMED, the model's escalation is still
///     grounded, so use the original `primary_verdict` as the lower bound:
///     `max(primary_verdict, survivor_floor)` (preserves e.g. a model
///     REQUEST_CHANGES backed by a confirmed Medium finding).
///   - If NO candidate was confirmed (all refuted), the escalation rested on
///     refuted evidence, so drop the lower bound to a neutral `APPROVE` baseline
///     and let the surviving findings alone decide — relaxing the verdict.
///
/// `UNKNOWN` is handled by the caller and never reaches here.
/// What: filters out refutation-variant findings, picks the baseline per the
/// rule above, then calls `derive_verdict(baseline, survivors)`.
/// Test: `rederive_excludes_refuted_relaxes`, `rederive_keeps_confirmed_block`,
/// `rederive_mixed_keeps_only_surviving_floor`, and the end-to-end
/// `verify_refuted_demotes_and_block_relaxes` /
/// `run_review_verification_confirms_and_preserves_verdict`.
fn rederive_verdict(
    primary_verdict: Verdict,
    any_confirmed: bool,
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
    // Confirmed evidence keeps the model's escalation as a lower bound; an
    // all-refuted candidate set drops to a neutral baseline so the verdict relaxes.
    let baseline = if any_confirmed {
        primary_verdict
    } else {
        Verdict::Approve
    };
    derive_verdict(baseline, &survivors)
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
/// Test: `select_candidates_block_uses_wide_net`,
/// `select_candidates_approve_uses_block_tier_only`.
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
        .filter(|(_, f)| f.confidence >= floor)
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
/// the `verification_model_error` signal; only a clean CONFIRMED/REFUTED judgment
/// (or a transient error, which is conservatively treated as unverifiable) maps
/// to the ordinary outcomes.
/// What: calls the verifier, parses the forced JSON judgment, and returns
/// `Confirmed` / `Refuted` accordingly.  On an alarm-class `LlmError`, emits the
/// signal and returns `ErrorRefuted`.  On a transient error or unparseable
/// output, returns `Refuted` (conservative: an unverifiable finding must not be
/// allowed to block) without raising the alarm.
/// Test: `verify_one_confirmed`, `verify_one_refuted`,
/// `verify_one_model_unavailable_emits_signal`.
async fn verify_one(verifier: &Arc<dyn LlmProvider>, req: crate::llm::LlmRequest) -> VerifyOutcome {
    let model = req.model.clone();
    match verifier.complete(req).await {
        Ok(resp) => match parse_judgment(&resp.text) {
            Some(true) => VerifyOutcome::Confirmed,
            Some(false) => VerifyOutcome::Refuted,
            None => {
                warn!(
                    text = %truncate(&resp.text, 120),
                    "verifier returned unparseable judgment — treating as REFUTED (conservative)"
                );
                VerifyOutcome::Refuted
            }
        },
        Err(e) if e.is_alarm() => {
            // Config/lifecycle failure: the verifier model is broken.  This is
            // the incident path — make it loud, do not pretend the finding was
            // refuted on its merits.
            let error_class = error_class(&e);
            emit_verification_model_error(&model, &error_class, &e);
            VerifyOutcome::ErrorRefuted { error_class }
        }
        Err(e) => {
            // Transient failure: we could not verify this finding, but the
            // deployment is not broken.  Conservatively refuse to let an
            // unverified finding drive a block.
            warn!("verifier transient error (treating as REFUTED): {e}");
            VerifyOutcome::Refuted
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
/// `Confirmed` and `Skipped` leave the confidence untouched.
/// Test: `verify_confirmed_keeps_and_block_holds`,
/// `verify_refuted_demotes_and_block_relaxes`.
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
    finding.verified = Some(outcome);
}

/// Parse the verifier's forced JSON judgment into `Some(true)`=CONFIRMED,
/// `Some(false)`=REFUTED, or `None` if unparseable.
///
/// Why: the verifier output is forced JSON `{judgment, reason}`; a robust parse
/// (with a keyword fallback for non-structured providers) keeps the outcome
/// deterministic.
/// What: tries direct JSON deserialisation first; falls back to a case-insensitive
/// keyword scan (CONFIRMED before REFUTED) so a provider that ignored the schema
/// still produces a decision.  Returns `None` only when neither token appears.
/// Test: `parse_judgment_confirmed`, `parse_judgment_refuted`,
/// `parse_judgment_unparseable`.
fn parse_judgment(text: &str) -> Option<bool> {
    let trimmed = text.trim();
    if let Ok(j) = serde_json::from_str::<VerifyJudgment>(trimmed) {
        return match j.judgment.trim().to_uppercase().as_str() {
            "CONFIRMED" => Some(true),
            "REFUTED" => Some(false),
            _ => None,
        };
    }
    // Fallback keyword scan for providers that ignored the forced schema.
    let upper = trimmed.to_uppercase();
    if upper.contains("CONFIRMED") {
        return Some(true);
    }
    if upper.contains("REFUTED") {
        return Some(false);
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

//! Unit tests for `pipeline::verify` (Phase 2, #583, #726).
//!
//! Why: split from `verify.rs` to keep that file under the 500-line cap.
//! What: covers candidate selection, outcome application, verdict re-derivation
//! (paths a/b/c), end-to-end rounds, and truncation regression (#726).
//! Test: this is the test module; each function is a self-contained unit test.

use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::{
    config::constants::VERIFY_REFUTED_CONFIDENCE,
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    models::{Effort, Finding, Verdict, VerifyOutcome},
};

// ── Deterministic fake verifier providers ─────────────────────────────────────

/// A verifier that always returns the same fixed judgment text.
struct FixedVerifier {
    text: String,
}

impl FixedVerifier {
    fn confirmed() -> Self {
        Self {
            text: r#"{"judgment":"CONFIRMED","reason":"present in diff"}"#.to_string(),
        }
    }
    fn refuted() -> Self {
        Self {
            text: r#"{"judgment":"REFUTED","reason":"not in diff"}"#.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for FixedVerifier {
    fn name(&self) -> &str {
        "fixed-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // #8904: batch-aware — answer every finding in the request with the
        // judgment `text` carries.
        let judgment = serde_json::from_str::<serde_json::Value>(&self.text)
            .ok()
            .and_then(|v| v["judgment"].as_str().map(str::to_string))
            .unwrap_or_default();
        Ok(LlmResponse {
            text: crate::pipeline::verify_batch::test_support::answer(&req, |_| judgment.clone()),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A verifier that always returns the same fixed judgment text.
struct TruncatedVerifier;

#[async_trait]
impl LlmProvider for TruncatedVerifier {
    fn name(&self) -> &str {
        "truncated-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Simulate a response truncated mid-JSON (as seen with max_tokens=16).
        Ok(LlmResponse {
            text: r#"{"judg"#.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A verifier that always fails with a configurable `LlmError`.
struct FailingVerifier {
    make_err: fn() -> LlmError,
}

#[async_trait]
impl LlmProvider for FailingVerifier {
    fn name(&self) -> &str {
        "failing-verifier"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err((self.make_err)())
    }
}

fn finding(effort: Effort, confidence: f32) -> Finding {
    let mut f = Finding::new("src/a.rs", "logic", "a bug", "fix it", confidence, effort);
    f.line = Some(10);
    // #PR84: a genuine High finding must be escalation-eligible (cited or
    // diff-provable) to drive the BLOCK floor.  These verification tests model
    // real, diff-provable correctness bugs, so mark them `code_provable` to
    // preserve their pre-#PR84 BLOCK-floor behaviour.
    f.code_provable = true;
    f
}

fn confirmed_provider() -> Arc<dyn LlmProvider> {
    Arc::new(FixedVerifier::confirmed())
}
fn refuted_provider() -> Arc<dyn LlmProvider> {
    Arc::new(FixedVerifier::refuted())
}
fn truncated_provider() -> Arc<dyn LlmProvider> {
    Arc::new(TruncatedVerifier)
}

// ── Candidate selection ───────────────────────────────────────────────────────

#[test]
fn select_candidates_takes_every_undecided_finding() {
    // #8904: every finding is a candidate, whatever its confidence — the old
    // verdict-dependent floor (0.90 on APPROVE*, 0.50 on BLOCK) left 9 of 10
    // reviews unverified.
    let findings = vec![
        finding(Effort::High, 0.95),
        finding(Effort::Medium, 0.80),
        finding(Effort::Low, 0.30),
    ];
    assert_eq!(select_candidates(&findings), vec![0, 1, 2]);
}

#[test]
fn select_candidates_skips_findings_with_a_decided_outcome() {
    // #4081: a finding whose `verified` outcome is already decided is not a
    // question worth re-asking. `claim_grounding` pre-stamps `Unverifiable` on a
    // package-registry claim precisely so the verifier — a second model with the
    // same stale training knowledge — can never launder it into `Confirmed`.
    let mut findings = vec![finding(Effort::High, 0.95), finding(Effort::High, 0.95)];
    assert_eq!(select_candidates(&findings), vec![0, 1]);

    findings[0].verified = Some(VerifyOutcome::Unverifiable {
        reason: "no registry lookup performed".to_string(),
    });
    assert_eq!(
        select_candidates(&findings),
        vec![1],
        "an already-decided outcome must not be re-verified"
    );
}

// ── Outcome application ───────────────────────────────────────────────────────

#[test]
fn apply_outcome_confirmed_keeps_confidence() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    assert!(
        (f.confidence - 0.95).abs() < f32::EPSILON,
        "CONFIRMED keeps confidence"
    );
    assert!(matches!(f.verified, Some(VerifyOutcome::Confirmed)));
}

#[test]
fn apply_outcome_refuted_demotes_below_advisory() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Refuted);
    assert!(
        (f.confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON,
        "REFUTED demotes confidence below the advisory tier"
    );
    assert!(matches!(f.verified, Some(VerifyOutcome::Refuted)));
}

#[test]
fn apply_outcome_error_refuted_also_demotes() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(
        &mut f,
        VerifyOutcome::ErrorRefuted {
            error_class: "ModelNotFound".to_string(),
        },
    );
    assert!((f.confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON);
    assert!(matches!(
        f.verified,
        Some(VerifyOutcome::ErrorRefuted { .. })
    ));
}

// ── Verdict re-derivation (refuted exclusion) ─────────────────────────────────

#[test]
fn rederive_keeps_confirmed_block() {
    // Path (a): one High finding, confirmed → survives → BLOCK floor.
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Block, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a confirmed High finding must keep the BLOCK floor (path a)"
    );
}

/// #PR84 adversarial-review follow-up (item 6): a CONFIRMED High finding that
/// FAILS the citability gate (uncited, non-diff-provable — the exact PR #84
/// shape post-verification: `verified: Confirmed`, no `source_citation`, not
/// `code_provable`) must NOT pin `primary_verdict` as a hard BLOCK floor via
/// path (a) — `confirmed_floor_blocks` asks `derive_verdict` (#4044), so this
/// routes to path (a2) instead, which independently re-derives via
/// `derive_verdict` rather than treating the disqualified confirmation as an
/// unconditional floor.
///
/// Why: previously the path (a) selector used a bare `f.effort == Effort::High`
/// check, so this exact scenario selected path (a) and pinned `primary_verdict`
/// (here, a self-reported BLOCK) regardless of citability.  `derive_verdict`'s
/// own #PR84 RULE 2 gate provides a downstream safety net that already prevents
/// an outright ungated BLOCK from this path, but the path (a) vs (a2) baseline
/// selection should independently agree with the citability rule rather than
/// relying solely on that downstream net.
#[test]
fn rederive_confirmed_but_disqualified_high_does_not_pin_block() {
    let mut f = finding(Effort::High, 0.95);
    f.code_provable = false; // disqualify: no citation, not diff-provable
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Block, &[f]);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: a confirmed-but-disqualified (uncited, non-diff-provable) High \
         finding must not pin primary_verdict as a hard BLOCK floor via path (a)"
    );
}

#[test]
fn rederive_confirmed_medium_still_escalates_to_request_changes() {
    // Path (a2) — #1876 (supersedes the pre-#1876 #1015 expectation): the
    // BASELINE is capped at APPROVE*, but `derive_verdict(baseline, survivors)`
    // independently re-derives the severity floor from the surviving findings.
    // A single confirmed Medium@0.85 (> FLOOR_MIN_CONFIDENCE) now floors to
    // REQUEST_CHANGES on its own merits (`correctness_floor` Tier 2, #1876), so
    // `stricter_of(baseline=APPROVE*, floor=REQUEST_CHANGES)` lands on
    // REQUEST_CHANGES even though the baseline itself was capped.
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::RequestChanges, &[med]);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a single confirmed high-confidence Medium must still escalate to \
         REQUEST_CHANGES (path a2 + #1876 floor, supersedes the pre-#1876 \
         APPROVE*-cap expectation)"
    );
}

#[test]
fn rederive_confirmed_praise_keeps_clean_approve() {
    // Path (a2) — #1343 runtime residual: the model itself emitted a clean APPROVE
    // (grade A-).  The verifier CONFIRMS a confidence=1.0 low-effort `praise`
    // finding.  Confirming a non-High finding must NOT raise the baseline to
    // APPROVE* — the source-of-truth APPROVE review_body wins.  Before the fix,
    // path (a2) hard-coded the baseline to APPROVE*, yielding APPROVE* and
    // clamping the A- grade down to C+.  After the fix, severity-min(APPROVE,
    // APPROVE*) = APPROVE, so the verdict stays APPROVE and the grade stays A-.
    let mut praise = finding(Effort::Low, 1.0);
    apply_outcome(&mut praise, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Approve, &[praise]);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a confirmed low-effort praise finding must NOT harden a clean APPROVE to \
         APPROVE* (path a2 — #1343 runtime residual)"
    );

    // And the grade clamp must keep A- (not downgrade to C+): clamp_grade_to_verdict
    // of A- against APPROVE is a no-op, whereas against APPROVE* it would drop to C+.
    use crate::pipeline::letter_grade::{Grade, clamp_grade_to_verdict};
    let clamped = clamp_grade_to_verdict(Grade::AMinus, &verdict);
    assert_eq!(
        clamped,
        Grade::AMinus,
        "grade must stay A- when verdict stays APPROVE (#1343 runtime residual)"
    );
}

#[test]
fn rederive_confirmed_high_effort_still_escalates_from_approve() {
    // Guard: the #1343 fix must NOT defang the safety net.  A CONFIRMED High-effort
    // (critical) finding still escalates even when the model said APPROVE — path (a)
    // keeps primary_verdict, and derive_verdict's BLOCK floor then escalates.
    let mut high = finding(Effort::High, 0.95);
    apply_outcome(&mut high, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Approve, &[high]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a confirmed High-effort finding still escalates APPROVE to BLOCK (path a)"
    );
}

/// Path (c), #8904 form: a round that confirmed nothing — every survivor was
/// judged UNVERIFIABLE — never relaxes the model's verdict.
#[test]
fn rederive_unverifiable_only_preserves_primary_verdict() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(
        &mut f,
        VerifyOutcome::Unverifiable {
            reason: "outside the diff".to_string(),
        },
    );
    assert_eq!(rederive_verdict(Verdict::Block, &[f]), Verdict::Block);
}

// ── End-to-end verification round ─────────────────────────────────────────────

#[tokio::test]
async fn verify_confirmed_keeps_and_block_holds() {
    // A single High-effort, high-confidence finding that the verifier CONFIRMS:
    // confidence is kept and the BLOCK verdict holds.
    let verifier = confirmed_provider();
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "us.anthropic.claude-haiku-4-5",
        "+ some diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Block,
        "confirmed High finding must hold BLOCK"
    );
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert!((findings[0].confidence - 0.95).abs() < f32::EPSILON);
}

/// A policy with no backoff sleep, so failure-path tests run instantly.
fn fast_policy() -> VerifyPolicy {
    VerifyPolicy {
        backoff_base_ms: 0,
        ..VerifyPolicy::default()
    }
}

/// Run a round under [`fast_policy`] and return its report.
async fn round(
    verifier: &Arc<dyn LlmProvider>,
    primary: Verdict,
    findings: &mut Vec<Finding>,
) -> crate::pipeline::verify_posted::VerifyReport {
    run_verification_round_with_policy(
        verifier,
        "m",
        "+ diff",
        primary,
        findings,
        None,
        None,
        None,
        fast_policy(),
    )
    .await
}

#[tokio::test]
async fn verify_refuted_drops_and_block_is_withheld() {
    // #8904: the ONLY finding is REFUTED → it is dropped, not demoted, and the
    // #8905 withhold policy settles the verdict: no survivors → UNKNOWN, never
    // the pre-#8904 APPROVE.
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = round(&refuted_provider(), Verdict::Block, &mut findings).await;
    assert!(findings.is_empty(), "a refuted finding is never posted");
    assert_eq!(report.refuted, 1);
    assert_eq!(report.verdict, Verdict::Unknown);
}

#[tokio::test]
async fn verify_no_findings_is_noop() {
    let mut findings: Vec<Finding> = Vec::new();
    let report = round(&refuted_provider(), Verdict::Approve, &mut findings).await;
    assert_eq!(report.verdict, Verdict::Approve);
    assert_eq!(report.calls, 0, "no finding, no verifier call");
}

#[tokio::test]
async fn verify_unknown_still_verifies_and_stays_unknown() {
    // #8904: an UNKNOWN review's findings are posted too, so they are verified;
    // the verdict stays UNKNOWN.
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = round(&refuted_provider(), Verdict::Unknown, &mut findings).await;
    assert!(findings.is_empty(), "a refuted finding is never posted");
    assert_eq!(report.verdict, Verdict::Unknown);
}

#[tokio::test]
async fn verify_one_model_unavailable_emits_signal() {
    // ModelNotFound (#726) → ErrorRefuted → #8904: withheld, never APPROVE.
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::ModelNotFound("stale-verifier".to_string()),
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = round(&verifier, Verdict::Block, &mut findings).await;
    assert!(findings.is_empty(), "an unjudged finding is never posted");
    assert_eq!((report.refuted, report.unjudged), (0, 1));
    assert_eq!(report.verdict, Verdict::Unknown);
}

/// #1876 fail-open regression, #8904 form: a TRANSIENT verifier error is never a
/// refutation and never fail-opens the review to APPROVE. Since #8904 the
/// finding is withheld (fail closed) and counted as unjudged, not refuted.
/// Test: this test itself.
#[tokio::test]
async fn verify_transient_error_is_not_plain_refuted() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::RateLimited,
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = round(&verifier, Verdict::Block, &mut findings).await;
    assert_eq!(report.refuted, 0, "a transient error is not a refutation");
    assert_eq!(report.unjudged, 1);
    assert!(findings.is_empty());
    assert_ne!(report.verdict, Verdict::Approve, "#1876: never fail open");
}

// ── Truncation path (#726 regression) ─────────────────────────────────────────

#[tokio::test]
async fn verify_truncated_response_is_withheld() {
    // Unparseable/truncated verifier output (#726) is not a judgment: #8904
    // withholds the finding and never relaxes to APPROVE.
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = round(&truncated_provider(), Verdict::Block, &mut findings).await;
    assert!(findings.is_empty());
    assert_eq!((report.refuted, report.unjudged), (0, 1));
    assert_eq!(report.verdict, Verdict::Unknown);
}

/// Regression for the dropped-JoinHandle true-positive (PR #720, #726 incident).
/// Why: (a) CONFIRMED Medium → REQUEST_CHANGES (path a2 baseline capped at
/// APPROVE*, the #1876 confidence-gated floor re-escalates a single confident
/// Medium); (b) a truncated answer must NOT collapse to APPROVE (#726) — since
/// #8904 the finding is withheld and the review is UNKNOWN.
/// Test: this test itself.
#[tokio::test]
async fn verify_join_handle_regression_pr720() {
    let mut f = Finding::new(
        "crates/trusty-search/src/startup.rs",
        "resource-leak",
        "JoinHandle dropped; spawned task detached, risking pool exhaustion",
        "Store the JoinHandle and await it in graceful shutdown",
        0.85,
        Effort::Medium,
    );
    f.line = Some(47);
    let diff = "+pub fn spawn_warm_boot_task() {\n\
                +    tokio::spawn(async move { warm_boot().await });\n\
                +}\n";

    let mut findings_1 = vec![f.clone()];
    let v1 = run_verification_round(
        &confirmed_provider(),
        "us.anthropic.claude-sonnet-4-6",
        diff,
        Verdict::RequestChanges,
        &mut findings_1,
        None,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings_1[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert_eq!(v1, Verdict::RequestChanges);

    let mut findings_2 = vec![f];
    let v2 = run_verification_round(
        &truncated_provider(),
        "us.anthropic.claude-sonnet-4-6",
        diff,
        Verdict::RequestChanges,
        &mut findings_2,
        None,
        None,
        None,
    )
    .await;
    assert!(findings_2.is_empty(), "#8904: a truncated answer withholds");
    assert_eq!(v2, Verdict::Unknown, "never APPROVE (#726, #8904)");
}

// ── #1015 regression ──────────────────────────────────────────────────────────

/// Regression: APPROVE + two Medium@0.70 must stay APPROVE (#1015).
/// Advisory Mediums (≤ 0.80) excluded from floor count; APPROVE stays APPROVE.
#[tokio::test]
async fn verify_approve_two_advisory_medium_stays_approve() {
    let verifier = confirmed_provider();
    let mut findings = vec![finding(Effort::Medium, 0.70), finding(Effort::Medium, 0.70)];
    let verdict = run_verification_round(
        &verifier,
        "m",
        "+ advisory diff",
        Verdict::Approve,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Approve,
        "advisory Medium@0.70 must not escalate APPROVE to REQUEST_CHANGES (#1015)"
    );
}

// ── Verify-path schema deserialization (#1235 strict-mode regression guard) ────
//
// Symmetric to the review path's `parse_direct_json_strict_full_shape`
// (`parser_tests.rs`). The #1235 strict-mode fix makes `reason` a REQUIRED
// property on the OpenAI verify schema; `#[serde(default)]` on
// `VerifyJudgment::reason` is what keeps lenient providers (Bedrock / Anthropic /
// Gemini) that OMIT `reason` deserializing instead of silently failing the
// verify path. These tests pin that invariant so a future edit that drops the
// `#[serde(default)]` fails loudly.

/// Full-shape verify response (`judgment` + `reason`) deserializes and is parsed.
///
/// Why: proves the happy path for strict providers that emit every required
/// field round-trips into `VerifyJudgment` and maps to the right decision.
/// What: deserializes `{"judgment":"CONFIRMED","reason":...}` and confirms both
/// the typed struct fields and `parse_judgment` agree it is CONFIRMED.
/// Test: this is the test.
#[test]
fn verify_judgment_full_shape_deserializes() {
    let body = serde_json::json!({
        "judgment": "CONFIRMED",
        "reason": "the finding is present in the diff at the cited line",
    })
    .to_string();

    let parsed: VerifyJudgment =
        serde_json::from_str(&body).expect("full-shape verify response must deserialize");
    assert_eq!(parsed.judgment, "CONFIRMED");
    assert_eq!(
        parsed.reason,
        "the finding is present in the diff at the cited line"
    );

    // End-to-end through the public parser entry point.
    assert_eq!(parse_judgment(&body), Some(Judgment::Confirmed));
}

/// Verify response that OMITS `reason` still deserializes (proves `#[serde(default)]`).
///
/// Why: lenient providers (Bedrock / Anthropic / Gemini) ignore the strict
/// schema and may omit `reason`. Without `#[serde(default)]` on
/// `VerifyJudgment::reason` this would fail to deserialize and silently break
/// the verify path — the #1235 regression this PR guards against.
/// What: deserializes `{"judgment":"REFUTED"}` (no `reason`), asserts `reason`
/// defaults to the empty string, and that `parse_judgment` still maps REFUTED.
/// Test: this is the test.
#[test]
fn verify_judgment_omits_reason_still_deserializes() {
    let body = serde_json::json!({ "judgment": "REFUTED" }).to_string();

    let parsed: VerifyJudgment = serde_json::from_str(&body)
        .expect("verify response omitting `reason` must still deserialize (#[serde(default)])");
    assert_eq!(parsed.judgment, "REFUTED");
    assert_eq!(
        parsed.reason, "",
        "omitted `reason` must default to empty string"
    );

    // End-to-end: a reason-less judgment still yields a clean decision.
    assert_eq!(parse_judgment(&body), Some(Judgment::Refuted));
}

// Liveness gate decision logic is tested in `verify_liveness.rs::tests`
// (`liveness_alive_allows_start`, `liveness_model_unavailable_refuses`, etc.)
// to keep this file under the 500-line cap and respect module ownership.

// ── Third judgment (#5309) ────────────────────────────────────────────────────

/// The verifier's UNVERIFIABLE answer must parse as its own judgment, not fall
/// through to the CONFIRMED keyword scan.
#[test]
fn parse_judgment_unverifiable() {
    let body = serde_json::json!({ "judgment": "UNVERIFIABLE", "reason": "signature is outside the diff" })
        .to_string();
    assert_eq!(parse_judgment(&body), Some(Judgment::Unverifiable));

    // A provider that ignored the schema and answered in prose containing both
    // tokens must still resolve to the reading that does not confirm.
    assert_eq!(
        parse_judgment("not CONFIRMED — UNVERIFIABLE from this diff"),
        Some(Judgment::Unverifiable)
    );
}

/// #8904: a truncated single-finding answer whose structured token is REFUTED
/// must not be read as CONFIRMED because its cut-off reason says "confirmed".
#[test]
fn parse_judgment_truncated_refuted_json_is_refuted() {
    assert_eq!(
        parse_judgment(r#"{"judgment":"REFUTED","reason":"not confirmed by"#),
        Some(Judgment::Refuted)
    );
    // The structured token wins over the keywords around it.
    assert_eq!(
        parse_judgment(r#"{"judgment": "REFUTED", "reason": "CONFIRMED elsewhere"#),
        Some(Judgment::Refuted)
    );
    // A cut-off or conflicting token judges nothing (withheld, not posted).
    assert_eq!(parse_judgment(r#"{"judgment":"CONF"#), None);
    assert_eq!(
        parse_judgment(r#"{"judgment":"CONFIRMED"} {"judgment":"REFUTED"}"#),
        None
    );
}

/// #8904: the keyword fallback never resolves an ambiguous answer to CONFIRMED.
#[test]
fn parse_judgment_ambiguous_prose_never_confirms() {
    assert_eq!(
        parse_judgment("REFUTED, not confirmed"),
        Some(Judgment::Refuted)
    );
    for ambiguous in [
        "not confirmed",
        "This is UNCONFIRMED.",
        "I can't say it is confirmed",
        "Confirmed? No.",
    ] {
        assert_ne!(
            parse_judgment(ambiguous),
            Some(Judgment::Confirmed),
            "{ambiguous:?}"
        );
    }
    // An unambiguous prose confirmation still parses.
    assert_eq!(
        parse_judgment("CONFIRMED: the handle is dereferenced first"),
        Some(Judgment::Confirmed)
    );
}

/// #9292: under toolChoice auto the verifier can answer in free text, and a
/// negated verdict keyword must never be read as that verdict. "not REFUTED"
/// once dropped a real finding as refuted; every negated row is now unjudged
/// (`None`), which the round withholds without relaxing the verdict.
#[test]
fn parse_judgment_negated_verdict_is_not_that_verdict() {
    for negated in [
        "not REFUTED",
        "NOT REFUTED",
        "The finding is not refuted by the diff.",
        "This cannot be refuted",
        "It isn't refuted",
        "UNREFUTED",
        "I would not say this is refuted",
        "not confirmed",
        "NOT CONFIRMED",
        "not UNVERIFIABLE",
    ] {
        assert_eq!(parse_judgment(negated), None, "{negated:?}");
    }
    // The unjudged reading is withheld as a verifier failure, never a refutation.
    let outcomes = judgments_for("not REFUTED", 1);
    assert!(
        matches!(
            outcomes.as_slice(),
            [(VerifyOutcome::TruncationRefuted, VerifierReach::Failed)]
        ),
        "{outcomes:?}"
    );
}

/// #9292: the negation check is clause-local, so un-negated verdicts — and a
/// verdict whose negation sits in another clause — parse exactly as before.
#[test]
fn parse_judgment_plain_verdicts_unchanged() {
    for (text, want) in [
        ("REFUTED", Judgment::Refuted),
        ("refuted", Judgment::Refuted),
        ("Verdict: REFUTED", Judgment::Refuted),
        ("Not a real bug. REFUTED.", Judgment::Refuted),
        ("No, this is refuted", Judgment::Refuted),
        ("CONFIRMED", Judgment::Confirmed),
        ("Verdict: CONFIRMED", Judgment::Confirmed),
        ("UNVERIFIABLE", Judgment::Unverifiable),
    ] {
        assert_eq!(parse_judgment(text), Some(want), "{text:?}");
    }
}

/// #5309: an `Unverifiable` outcome must strip the signals that let a finding
/// pin the BLOCK floor — the same demotion the hygiene passes apply when they
/// pre-stamp it — so a claim carries identical weight whichever route
/// classified it.
#[test]
fn apply_outcome_unverifiable_strips_block_floor_signals() {
    let mut f = finding(Effort::High, 0.72);
    assert!(
        crate::pipeline::grade::drives_block_floor(&f),
        "precondition: it would pin the BLOCK floor"
    );
    apply_outcome(
        &mut f,
        VerifyOutcome::Unverifiable {
            reason: "outside the diff".to_string(),
        },
    );
    assert!(
        !crate::pipeline::grade::drives_block_floor(&f),
        "an unchecked claim must not drive BLOCK"
    );
    assert!(!f.code_provable);
    assert!(
        f.confidence <= 0.65,
        "confidence capped, got {}",
        f.confidence
    );
}

// ── Fan-out retry and UNVERIFIED accounting (#4459) ───────────────────────────

/// A verifier that fails the first `fail_first` attempts at each distinct
/// request, then answers CONFIRMED — the shape of a transport blip under
/// fan-out, where the same call succeeds on a second try.
struct FlakyVerifier {
    fail_first: usize,
    attempts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    peak_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    /// Hold each call until this many are in flight at once, so the test
    /// observes the real ceiling instead of whatever the scheduler happened to
    /// interleave. `0` disables the rendezvous.
    rendezvous_at: usize,
}

impl FlakyVerifier {
    fn new(fail_first: usize) -> Self {
        Self {
            fail_first,
            attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            peak_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            rendezvous_at: 0,
        }
    }

    fn rendezvous_at(mut self, n: usize) -> Self {
        self.rendezvous_at = n;
        self
    }
}

#[async_trait]
impl LlmProvider for FlakyVerifier {
    fn name(&self) -> &str {
        "flaky-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        use std::sync::atomic::Ordering;
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);

        // Condition polling, never a sleep: yield until the round has as many
        // calls in flight as the rendezvous asks for, bounded so a wrong
        // expectation fails the test instead of hanging it.
        if self.rendezvous_at > 0 {
            let mut spins = 0;
            while self.in_flight.load(Ordering::SeqCst) < self.rendezvous_at && spins < 10_000 {
                spins += 1;
                tokio::task::yield_now().await;
            }
        }

        let key = format!("{:?}", req.messages);
        let seen = {
            let mut map = self.attempts.lock().expect("attempt map poisoned");
            let e = map.entry(key).or_insert(0);
            *e += 1;
            *e
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        if seen <= self.fail_first {
            return Err(LlmError::Transport("connection reset by peer".into()));
        }
        Ok(LlmResponse {
            // #8904: batch-aware.
            text: crate::pipeline::verify_batch::test_support::answer(&req, |_| {
                "CONFIRMED".to_string()
            }),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Distinct findings, so each one produces a distinct verifier request.
fn findings_for_fan_out(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| {
            let mut f = Finding::new(
                format!("src/f{i}.rs"),
                "logic",
                format!("bug number {i}"),
                "fix it",
                0.95,
                Effort::High,
            );
            f.line = Some(10 + i as u32);
            f.code_provable = true;
            f
        })
        .collect()
}

/// Zero-sleep policy: the ladder still runs, the test just does not wait on it.
fn test_policy(concurrency: usize, max_attempts: u32) -> VerifyPolicy {
    VerifyPolicy {
        concurrency,
        max_attempts,
        backoff_base_ms: 0,
        ..VerifyPolicy::default()
    }
}

/// #4459 (a): a transient transport failure must be RETRIED, and a finding that
/// succeeds on a later attempt must come back CONFIRMED.
///
/// Why: this is the whole bug. Before this fix `verify_one` recorded an outcome
/// on the FIRST error, so every finding whose call lost the race with the
/// round's own fan-out was written off unverified — 27 of 29 in the measured
/// incident — while the same call in isolation succeeded.
/// What: a verifier that fails each request twice and then answers CONFIRMED,
/// under a 3-attempt budget. On the pre-fix code this test fails: the outcome is
/// recorded from attempt 1, so the finding lands on `ErrorRefuted` and the
/// verifier is never called a second time.
/// Test: this test itself.
#[tokio::test]
async fn verify_transient_failure_is_retried_until_it_succeeds() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FlakyVerifier::new(2));
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(4, 3),
    )
    .await
    .verdict;
    assert!(
        matches!(findings[0].verified, Some(VerifyOutcome::Confirmed)),
        "a transient failure inside the attempt budget must be retried to a real \
         judgment, got {:?}",
        findings[0].verified
    );
    assert_eq!(
        verdict,
        Verdict::Block,
        "the confirmed High finding still holds BLOCK"
    );
}

/// #4459 (b), #8904 form: a finding the verifier never reaches, on any
/// attempt, is unjudged — not refuted — and #8904 withholds it (fail closed).
///
/// Why: nothing checked it, so posting it would post an unverified claim, and
/// withholding it must not read as "nothing wrong": the verdict is UNKNOWN.
/// What: a verifier that always returns `Transport`, under a 2-attempt budget.
/// Test: this test itself.
#[tokio::test]
async fn verify_permanent_transport_failure_is_withheld() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::Transport("connection refused".into()),
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let report = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(4, 2),
    )
    .await;
    assert!(findings.is_empty(), "an unjudged finding is never posted");
    assert_eq!((report.refuted, report.unjudged), (0, 1));
    assert_eq!(
        report.verdict,
        Verdict::Unknown,
        "never fail open to APPROVE"
    );
}

/// #4459 (c): the round honours the configured fan-out ceiling, and every
/// finding in a wide fan-out still reaches a real judgment.
///
/// Why: the ceiling was a hardcoded `4` with no way for an operator to relieve a
/// provider that was throttling the round. It is now config, and a round that
/// ignored it would recreate the burst this issue is about.
/// What: eight findings under a ceiling of 2, against a verifier that fails each
/// request once before answering. The provider rendezvouses at 2 concurrent
/// calls (condition polling, no sleep) so the peak is actually observed. On the
/// pre-fix code this test fails twice over: the width is fixed at 4, and the
/// single injected failure per finding is never retried.
/// Test: this test itself.
#[tokio::test]
async fn verify_round_never_exceeds_the_configured_concurrency() {
    use std::sync::atomic::Ordering;
    let flaky = FlakyVerifier::new(1).rendezvous_at(2);
    let peak = flaky.peak_in_flight.clone();
    let verifier: Arc<dyn LlmProvider> = Arc::new(flaky);

    let mut findings = findings_for_fan_out(8);
    let verdict = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(2, 3),
    )
    .await
    .verdict;

    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "the round must fan out to exactly the configured width — no wider, and \
         wide enough that the ceiling is the thing being measured"
    );
    for (i, f) in findings.iter().enumerate() {
        assert!(
            matches!(f.verified, Some(VerifyOutcome::Confirmed)),
            "finding {i} must survive its transient failure and be judged, got {:?}",
            f.verified
        );
    }
    assert_eq!(
        crate::pipeline::post::count_unverified(&findings),
        0,
        "nothing is unverified once every retry succeeded"
    );
    assert_eq!(verdict, Verdict::Block);
}

/// The policy reads operator config and refuses a zero on either count.
#[test]
fn policy_from_config_clamps_zero_counts() {
    let cfg = crate::config::VerificationConfig {
        enabled: true,
        liveness_check: true,
        concurrency: 0,
        max_attempts: 0,
        max_calls: 0,
        batch_size: 0,
    };
    let policy = VerifyPolicy::from_config(&cfg);
    assert_eq!(policy.concurrency, 1, "a 0 width must never verify nothing");
    assert_eq!(
        policy.max_attempts, 1,
        "a 0 budget must still make one call"
    );
    assert_eq!((policy.max_calls, policy.batch_size), (1, 1), "#8904");

    let cfg = crate::config::VerificationConfig {
        concurrency: 6,
        max_attempts: 5,
        ..Default::default()
    };
    let policy = VerifyPolicy::from_config(&cfg);
    assert_eq!(policy.concurrency, 6);
    assert_eq!(policy.max_attempts, 5);
}

/// The backoff doubles per attempt and stays inside its jitter band.
#[test]
fn backoff_grows_and_stays_within_its_jitter_band() {
    let policy = VerifyPolicy {
        concurrency: 4,
        max_attempts: 4,
        backoff_base_ms: 200,
        ..VerifyPolicy::default()
    };
    assert!(policy.backoff(1).is_zero(), "the first attempt never waits");
    for (attempt, base) in [(2u32, 200u64), (3, 400), (4, 800)] {
        let ms = policy.backoff(attempt).as_millis() as u64;
        assert!(
            (base..base + base / 4).contains(&ms),
            "attempt {attempt} must wait {base}ms plus up to 25% jitter, got {ms}ms"
        );
    }
    assert!(
        VerifyPolicy {
            backoff_base_ms: 0,
            ..policy
        }
        .backoff(3)
        .is_zero(),
        "a zero base disables the wait so tests do not sleep"
    );
}

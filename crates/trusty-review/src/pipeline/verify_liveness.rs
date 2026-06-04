//! Startup verifier-model liveness gate (Phase 2, #583).
//!
//! Why: see the incident rationale in `pipeline::verify` — a stale verifier
//! model that auto-refutes every finding must stop a live deployment from
//! starting, not silently neuter every review.  This gate is split into its own
//! module to keep `verify.rs` under the 500-line cap and to isolate the startup
//! decision logic from the per-finding round.
//!
//! What: exposes `LivenessDecision` and `probe_verifier_liveness`, a cheap single
//! verifier call whose outcome decides whether live mode may start.  Alarm-class
//! errors (`ModelNotFound` / `ModelNotReady` / `Validation` / `AccessDenied`)
//! refuse the start and emit the `verification_model_error` signal; transient
//! errors and successful responses allow it.
//!
//! Test: `liveness_alive_allows_start`, `liveness_model_unavailable_refuses`,
//! `liveness_access_denied_refuses`, `liveness_transient_allows_start`,
//! `liveness_rate_limited_allows_start` in the `tests` module below.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::{
    config::ReviewConfig,
    llm::LlmProvider,
    models::{Effort, Finding},
    pipeline::verify::{emit_verification_model_error, error_class},
    pipeline::verify_prompt::build_verify_request,
};

/// Decision returned by `probe_verifier_liveness`.
///
/// Why: the caller (the `serve` / `run --live` startup path) needs a typed
/// answer it can act on — proceed, or refuse to start with a reason — without
/// re-classifying the raw `LlmError`.
/// What: `Ok` means the verifier model is alive (or the probe was disabled);
/// `Refuse` carries the human-readable reason and the error class for the signal.
/// Test: `liveness_alive_allows_start`, `liveness_model_unavailable_refuses`,
/// `liveness_transient_allows_start` in `tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivenessDecision {
    /// The verifier model responded (or the probe was skipped) — start is allowed.
    Ok,
    /// The verifier model is unavailable — live mode must refuse to start.
    Refuse {
        /// Human-readable reason (used in the startup error message).
        reason: String,
        /// Error class for the `verification_model_error` signal.
        error_class: String,
    },
}

/// Probe the verifier model with a tiny stub call and decide whether live mode
/// may start.
///
/// Why: see the `verify` module-level incident rationale — a stale verifier model
/// must stop a live deployment from starting, not silently neuter every review.
/// The probe is a cheap single call; the cost is negligible next to the safety it
/// buys.
/// What: issues a minimal verify request against a throwaway finding.  A
/// successful response (any text) or a *transient* error (`is_retryable`) returns
/// `Ok` — a transient blip must not block startup, the reviewer will retry per
/// finding at run time.  Only an *alarm-class* error (`ModelNotFound` /
/// `ModelNotReady` / `Validation` / `AccessDenied`) returns `Refuse`, after
/// emitting the `verification_model_error` signal.
/// Test: `liveness_alive_allows_start`, `liveness_model_unavailable_refuses`,
/// `liveness_transient_allows_start` in `tests`.
pub async fn probe_verifier_liveness(
    verifier: &Arc<dyn LlmProvider>,
    verifier_model: &str,
) -> LivenessDecision {
    let probe_finding = stub_probe_finding();
    // Empty diff is intentional: we only care whether the MODEL responds, not
    // about the judgment.  The verifier will likely REFUTE (finding not in
    // diff) — that is a perfectly healthy response and means the model is alive.
    let req = build_verify_request(verifier_model, "", &probe_finding, None, Some(16));

    match verifier.complete(req).await {
        Ok(_) => {
            debug!(model = %verifier_model, "verifier liveness probe: model responded — OK");
            LivenessDecision::Ok
        }
        Err(e) if e.is_alarm() => {
            let error_class = error_class(&e);
            emit_verification_model_error(verifier_model, &error_class, &e);
            LivenessDecision::Refuse {
                reason: format!(
                    "verifier model '{verifier_model}' is unavailable ({error_class}: {e}); \
                     refusing to start in live mode. Fix the verifier model id / lifecycle \
                     state, or disable the gate with TRUSTY_REVIEW_VERIFIER_LIVENESS_CHECK=false \
                     (only if you accept running without verification)."
                ),
                error_class,
            }
        }
        Err(e) => {
            // Transient error during the probe — do not block startup; per-finding
            // verification will retry at run time.
            warn!(
                model = %verifier_model,
                "verifier liveness probe hit a transient error (allowing start): {e}"
            );
            LivenessDecision::Ok
        }
    }
}

/// Build a throwaway finding used only as the liveness-probe payload.
///
/// Why: the probe needs a well-formed finding to build a valid request; its
/// content is irrelevant because we only inspect whether the model responds.
/// What: a fixed, obviously-synthetic finding referencing a file that cannot be
/// in any diff.
/// Test: covered transitively by the liveness tests.
fn stub_probe_finding() -> Finding {
    Finding::new(
        "__liveness_probe__",
        "liveness",
        "startup verifier liveness probe — not a real finding",
        "",
        0.5,
        Effort::Low,
    )
}

/// Enforce the startup verifier-model liveness gate (Phase 2, #583).
///
/// Why: the code-intelligence incident — a stale verifier model auto-refuting
/// every finding — must be impossible in a live deployment.  This gate probes the
/// verifier model at startup and refuses to start live mode when it is dead.
/// Keeping the decision logic in the library (rather than the binary) makes it
/// directly unit-testable with injected fake providers.
/// What: a no-op (`Ok`) when verification or the liveness check is disabled.
/// In live (non-dry-run) mode a missing verifier or an alarm-class probe failure
/// returns `Err(reason)` so the daemon aborts startup; in dry-run mode the same
/// conditions only warn and return `Ok` (a dry-run daemon never posts, so a dead
/// verifier weakens the dry-run output but cannot corrupt a live review).
/// Test: `enforce_disabled_is_ok`, `enforce_live_missing_verifier_refuses`,
/// `enforce_live_model_unavailable_refuses`, `enforce_live_alive_ok`,
/// `enforce_dry_run_model_unavailable_allows`.
pub async fn enforce_verifier_liveness(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
) -> Result<(), String> {
    // Gate only applies when verification + the liveness check are both on.
    if !config.verification.enabled || !config.verification.liveness_check {
        return Ok(());
    }
    if config.dry_run {
        info!("dry-run mode — verifier liveness gate is informational only");
    }

    let Some(verifier) = verifier else {
        // Verification enabled but no verifier built: in live mode this is a
        // misconfiguration; refuse.  In dry-run, warn and continue.
        if config.dry_run {
            warn!("no verifier provider available — dry-run continues without verification");
            return Ok(());
        }
        return Err(
            "verification is enabled but no verifier provider could be built; \
             refusing to start in live mode"
                .to_string(),
        );
    };

    let model = &config.role_models.verifier.model;
    match probe_verifier_liveness(verifier, model).await {
        LivenessDecision::Ok => Ok(()),
        LivenessDecision::Refuse { reason, .. } => {
            if config.dry_run {
                // Dry-run: surface the problem but do not block startup.
                warn!("verifier liveness probe failed in dry-run (continuing): {reason}");
                Ok(())
            } else {
                Err(reason)
            }
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::llm::{LlmError, LlmRequest, LlmResponse};

    /// A verifier that always returns a fixed result/error for gate tests.
    struct StubVerifier {
        err: Option<fn() -> LlmError>,
    }

    #[async_trait]
    impl LlmProvider for StubVerifier {
        fn name(&self) -> &str {
            "stub"
        }
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            if let Some(make) = self.err {
                return Err(make());
            }
            Ok(LlmResponse {
                text: r#"{"judgment":"REFUTED"}"#.to_string(),
                model: req.model,
                input_tokens: 1,
                output_tokens: 1,
                latency_ms: 1,
                cost_usd: 0.0,
            })
        }
    }

    fn alive() -> Arc<dyn LlmProvider> {
        Arc::new(StubVerifier { err: None })
    }
    fn dead() -> Arc<dyn LlmProvider> {
        Arc::new(StubVerifier {
            err: Some(|| LlmError::ModelNotFound("stale".into())),
        })
    }

    /// Build a config with the verification flags + dry_run set as given.
    fn cfg(enabled: bool, liveness: bool, dry_run: bool) -> ReviewConfig {
        let mut c = ReviewConfig::load(None);
        c.verification.enabled = enabled;
        c.verification.liveness_check = liveness;
        c.dry_run = dry_run;
        c
    }

    #[tokio::test]
    async fn enforce_disabled_is_ok() {
        // Verification disabled → gate is a no-op even with a dead verifier.
        let c = cfg(false, true, false);
        assert!(enforce_verifier_liveness(&c, Some(&dead())).await.is_ok());
    }

    #[tokio::test]
    async fn enforce_liveness_check_off_is_ok() {
        let c = cfg(true, false, false);
        assert!(enforce_verifier_liveness(&c, Some(&dead())).await.is_ok());
    }

    #[tokio::test]
    async fn enforce_live_missing_verifier_refuses() {
        // Live mode, verification on, no verifier → refuse.
        let c = cfg(true, true, false);
        assert!(enforce_verifier_liveness(&c, None).await.is_err());
    }

    #[tokio::test]
    async fn enforce_live_model_unavailable_refuses() {
        // Live mode + dead verifier model → refuse (the incident path).
        let c = cfg(true, true, false);
        let res = enforce_verifier_liveness(&c, Some(&dead())).await;
        assert!(res.is_err(), "live mode must refuse a dead verifier");
        assert!(
            res.unwrap_err().contains("refusing to start"),
            "error must state the refusal"
        );
    }

    #[tokio::test]
    async fn enforce_live_alive_ok() {
        let c = cfg(true, true, false);
        assert!(
            enforce_verifier_liveness(&c, Some(&alive())).await.is_ok(),
            "a live verifier must allow start"
        );
    }

    #[tokio::test]
    async fn enforce_dry_run_model_unavailable_allows() {
        // Dry-run never posts, so a dead verifier only warns — start is allowed.
        let c = cfg(true, true, true);
        assert!(
            enforce_verifier_liveness(&c, Some(&dead())).await.is_ok(),
            "dry-run must not block startup on a dead verifier"
        );
    }

    #[tokio::test]
    async fn enforce_dry_run_missing_verifier_allows() {
        let c = cfg(true, true, true);
        assert!(enforce_verifier_liveness(&c, None).await.is_ok());
    }

    // ── probe_verifier_liveness direct tests ─────────────────────────────────

    #[tokio::test]
    async fn liveness_alive_allows_start() {
        // The verifier responds (any text) → Ok.
        let decision = probe_verifier_liveness(&alive(), "us.anthropic.claude-haiku-4-5").await;
        assert_eq!(
            decision,
            LivenessDecision::Ok,
            "a responding model allows start"
        );
    }

    #[tokio::test]
    async fn liveness_model_unavailable_refuses() {
        // ModelNotFound is an alarm-class error → Refuse (the incident path).
        let dead_model: Arc<dyn LlmProvider> = Arc::new(StubVerifier {
            err: Some(|| LlmError::ModelNotFound("no-such-profile".to_string())),
        });
        let decision = probe_verifier_liveness(&dead_model, "no-such-profile").await;
        match decision {
            LivenessDecision::Refuse {
                error_class,
                reason,
            } => {
                assert_eq!(error_class, "ModelNotFound");
                assert!(reason.contains("no-such-profile"), "reason names the model");
                assert!(
                    reason.contains("refusing to start"),
                    "reason must state the refusal"
                );
            }
            LivenessDecision::Ok => panic!("an unavailable verifier model must refuse start"),
        }
    }

    #[tokio::test]
    async fn liveness_access_denied_refuses() {
        let access_denied: Arc<dyn LlmProvider> = Arc::new(StubVerifier {
            err: Some(|| LlmError::AccessDenied("bad iam".to_string())),
        });
        let decision = probe_verifier_liveness(&access_denied, "m").await;
        assert!(
            matches!(decision, LivenessDecision::Refuse { .. }),
            "AccessDenied is alarm-class and must refuse start"
        );
    }

    #[tokio::test]
    async fn liveness_transient_allows_start() {
        // A transient error during the probe must NOT block startup.
        let transient: Arc<dyn LlmProvider> = Arc::new(StubVerifier {
            err: Some(|| LlmError::Transport("connection reset".to_string())),
        });
        let decision = probe_verifier_liveness(&transient, "m").await;
        assert_eq!(
            decision,
            LivenessDecision::Ok,
            "a transient probe error must not block startup"
        );
    }

    #[tokio::test]
    async fn liveness_rate_limited_allows_start() {
        let rate_limited: Arc<dyn LlmProvider> = Arc::new(StubVerifier {
            err: Some(|| LlmError::RateLimited),
        });
        let decision = probe_verifier_liveness(&rate_limited, "m").await;
        assert_eq!(
            decision,
            LivenessDecision::Ok,
            "rate-limit during probe is transient"
        );
    }
}

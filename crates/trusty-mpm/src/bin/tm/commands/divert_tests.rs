//! Unit tests for [`super`] (`tm divert bulk-read`, #6887).
//!
//! Why: split out of `divert.rs` to keep the production module under the
//! 500-SLOC cap.
//! What: drives [`super::bulk_read_answer`] against a SCRIPTED
//! [`LlmProvider`] — one that answers, one that errors — so the fall-through
//! branch the design marks BLOCKING is an executed path, not a claim. Also
//! covers the synthetic config's routing inputs (including the
//! provider/model contradiction the registry must catch) and the usage
//! payload's shape.
//! Test: this module IS the test suite for `super`.

use async_trait::async_trait;
use trusty_mpm::core::sm::providers::{LlmResponse, SmLlmError};

use super::*;

/// A scripted provider: replies with a canned response, or fails.
///
/// Why: the fall-through path must be tested without a network or a
/// credential, and the success path must populate the telemetry the usage
/// event reads.
/// What: `Ok`-variant returns the given [`LlmResponse`]; `Err`-variant returns
/// the given [`SmLlmError`].
struct ScriptedProvider(Result<LlmResponse, SmLlmError>);

#[async_trait]
impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn complete(&self, _request: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        match &self.0 {
            Ok(r) => Ok(r.clone()),
            Err(e) => Err(SmLlmError::Degraded(e.to_string())),
        }
    }
}

fn canned_response() -> LlmResponse {
    LlmResponse {
        text: "It parses TOML and returns a manifest.".to_string(),
        model: "claude-haiku".to_string(),
        input_tokens: 4200,
        output_tokens: 180,
        latency_ms: 900,
        cost_usd: 0.0031,
    }
}

/// Why (#6887 §9.9): the worker round trip must return the model's text AND
/// the token/cost telemetry the usage event is built from — a response that
/// dropped the counts would make every diversion report zero saved.
/// What: a scripted success reaches [`BulkReadOutcome::Answered`] with every
/// field carried through verbatim.
#[tokio::test]
async fn bulk_read_answer_returns_the_worker_text() {
    let provider = ScriptedProvider(Ok(canned_response()));
    let outcome = bulk_read_answer(
        &provider,
        "claude-haiku",
        "=== a.rs ===\nfn main() {}\n",
        "What does this do?",
        1024,
    )
    .await;

    let BulkReadOutcome::Answered {
        text,
        model,
        input_tokens,
        output_tokens,
        cost_usd,
    } = outcome
    else {
        panic!("a scripted success must answer, got {outcome:?}");
    };
    assert_eq!(text, "It parses TOML and returns a manifest.");
    assert_eq!(model, "claude-haiku");
    assert_eq!(input_tokens, 4200);
    assert_eq!(output_tokens, 180);
    assert!((cost_usd - 0.0031).abs() < f64::EPSILON);
}

/// Why (#6887, BLOCKING design precondition (b)): a provider failure must
/// produce a DISTINGUISHABLE fall-through signal, not a bare failure. A bare
/// error reads to the agent as "transient, retry", and it would loop against a
/// worker that can never answer.
/// What: a scripted `Err` reaches [`BulkReadOutcome::FallThrough`] carrying the
/// provider's own message, and the marker the hook's block reason quotes is the
/// literal the caller prints for it.
#[tokio::test]
async fn bulk_read_answer_signals_fall_through_on_provider_error() {
    let provider = ScriptedProvider(Err(SmLlmError::Degraded(
        "no ANTHROPIC_API_KEY, AWS credentials, or OPENROUTER_API_KEY available".to_string(),
    )));
    let outcome = bulk_read_answer(&provider, "claude-haiku", "content", "q", 1024).await;

    let BulkReadOutcome::FallThrough { reason } = outcome else {
        panic!("a provider error must fall through, got {outcome:?}");
    };
    assert!(
        reason.contains("ANTHROPIC_API_KEY"),
        "the provider's own message must survive: {reason}"
    );
    assert_eq!(FALLTHROUGH_MARKER, "divert: fall-through");
    assert_eq!(FALLTHROUGH_EXIT, 3);
}

/// Why: the hook's block reason quotes [`FALLTHROUGH_MARKER`] verbatim so the
/// agent has a literal to match. If the two drift, the recovery instruction
/// names a string the command never prints.
/// What: the hook's reason text contains the marker constant.
#[test]
fn fallthrough_marker_matches_the_hook_reason() {
    let reason = crate::commands::divert_check::block_reason("/f.rs", 900);
    assert!(
        reason.contains(FALLTHROUGH_MARKER),
        "the hook reason must quote the marker verbatim: {reason}"
    );
}

/// Why (#6887 §9.8): a contradiction between an explicit `provider` and a
/// provider-PREFIXED model must be caught by the registry's own
/// `resolve_provider_and_model`, not by a bespoke shortcut here. This asserts
/// the synthetic config feeds that machinery the inputs it needs.
/// What: `provider = "openrouter"` with a `bedrock/`-prefixed worker model
/// resolves to a validation error naming both, with no credentials involved.
#[tokio::test]
async fn resolve_worker_reports_a_provider_model_contradiction() {
    let cfg = worker_config(
        Some("bedrock/anthropic.claude-haiku".to_string()),
        Some("openrouter".to_string()),
    );
    assert_eq!(cfg.provider, "openrouter");
    assert_eq!(cfg.summary_model, "bedrock/anthropic.claude-haiku");

    let err = ProviderRegistry::default()
        .build(&cfg, SmModelTier::Summary)
        .await
        .expect_err("a provider/model contradiction must be an error");
    let text = err.to_string().to_lowercase();
    assert!(
        text.contains("bedrock") && text.contains("openrouter"),
        "the error must name both sides of the contradiction: {err}"
    );
}

/// Why: an empty `[divert] worker_model` must mean "the provider's cheap-tier
/// default", not "no model" — the latter would degrade every session that did
/// not name a model explicitly.
/// What: empty/absent inputs fall back to the [`SmInferenceConfig`] defaults;
/// non-empty inputs are carried through.
#[test]
fn worker_config_falls_back_to_the_tier_default() {
    let defaults = SmInferenceConfig::default();

    let empty = worker_config(None, None);
    assert_eq!(empty.provider, "auto");
    assert_eq!(empty.summary_model, defaults.summary_model);

    let blank = worker_config(Some("  ".to_string()), Some(String::new()));
    assert_eq!(blank.provider, "auto");
    assert_eq!(blank.summary_model, defaults.summary_model);

    let set = worker_config(
        Some("anthropic/claude-haiku".to_string()),
        Some("anthropic".to_string()),
    );
    assert_eq!(set.provider, "anthropic");
    assert_eq!(set.summary_model, "anthropic/claude-haiku");
}

/// Why (#6887 §7): #6873's ledger is not merged, so this payload IS the
/// interim record. Its `diversion: true` marker is how a later consumer tells
/// a diversion apart from ordinary token accounting, and the estimate must
/// never go negative when a worker answers at length.
/// What: asserts the four required keys and the saturating estimate.
#[test]
fn diversion_usage_payload_carries_the_diversion_marker() {
    let payload = diversion_usage_payload("claude-haiku", "anthropic", 4200, 180, 0.0031);
    assert_eq!(payload["diversion"], serde_json::json!(true));
    assert_eq!(payload["tokens_saved_estimate"], serde_json::json!(4020));
    assert_eq!(payload["worker_model"], serde_json::json!("claude-haiku"));
    assert_eq!(payload["worker_provider"], serde_json::json!("anthropic"));

    // A verbose worker reports zero saved, never a negative.
    let inverted = diversion_usage_payload("claude-haiku", "auto", 10, 900, 0.0);
    assert_eq!(inverted["tokens_saved_estimate"], serde_json::json!(0));
}

/// Why: the worker must be told which bytes came from which file, or its
/// answer cannot cite anything; and a named file it cannot read is a hard
/// error, because silently skipping it yields an answer about the wrong thing.
/// What: each file gets a `=== <path> ===` header; a missing file errors.
#[test]
fn read_sources_labels_each_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, "fn a() {}").expect("write a");
    std::fs::write(&b, "fn b() {}").expect("write b");

    let blob = read_sources(&[a.clone(), b.clone()]).expect("read");
    assert!(blob.contains(&format!("=== {} ===", a.display())));
    assert!(blob.contains(&format!("=== {} ===", b.display())));
    assert!(blob.contains("fn a() {}"));
    assert!(blob.contains("fn b() {}"));

    let missing = dir.path().join("absent.rs");
    let err = read_sources(&[missing]).expect_err("a missing file must be an error");
    assert!(err.to_string().contains("cannot read"));
}

/// Why: an unbounded blob would fail provider-side as a transport error, which
/// the caller cannot tell from a real outage — it would report fall-through
/// for a cause the operator cannot see.
/// What: content past the budget is cut at a char boundary and marked.
#[test]
fn read_sources_truncates_past_the_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = dir.path().join("big.rs");
    std::fs::write(&big, "x".repeat(MAX_CONTENT_BYTES + 10_000)).expect("write");

    let blob = read_sources(&[big]).expect("read");
    assert!(blob.contains("[truncated: content budget reached]"));
    assert!(
        blob.len() < MAX_CONTENT_BYTES + 200,
        "the blob must stay near the budget, got {}",
        blob.len()
    );
}

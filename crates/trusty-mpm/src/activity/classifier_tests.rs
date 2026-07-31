//! Unit tests for [`super::OpenRouterClassifier`] and its JSON helpers (#4427).
//!
//! Why: before the inference-adapter migration NOTHING in the repo executed
//! `OpenRouterClassifier::classify` — the 12 `activity::monitor` tests all drove
//! `ActivityMonitor` against hand-written `LlmClassifier` mocks, and the single
//! test naming the classifier only asserted an error string. `extract_json` and
//! `parse_state` had no direct coverage at all. That left the exact code #4427
//! rewrites unguarded, so this file exists to make a regression in it fail a
//! build. Every case is hermetic: a `ScriptedAdapter`/recording double, or an
//! empty `MemoryKeyStore` — no network, no real credential.
//! What: direct unit tests for [`super::extract_json`]/[`super::parse_state`],
//! the env model ladder, and four `classify` paths — happy path (verdict +
//! provider usage), missing credential, call error, unparseable model output.
//! Test: this file IS the test module.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serial_test::serial;
use trusty_common::inference::credentials::MemoryKeyStore;
use trusty_common::inference::registry::{ProviderCapabilities, ProviderId, capabilities};
use trusty_common::inference::test_support::ScriptedAdapter;
use trusty_common::inference::{
    AssistantMessage, ChatChoice, ChatRequest, ChatResponse, InferenceAdapter, InferenceError,
    UsageBlock,
};

use super::{
    CLASSIFIER_MODEL_ENV, DEFAULT_CLASSIFIER_MODEL, OpenRouterClassifier, Source, extract_json,
    parse_state, resolve_classifier_model,
};
use crate::activity::cache::ActivityState;
use crate::activity::monitor::{ActivityError, LlmClassifier};

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// The OpenRouter capability profile every double is built with.
fn caps() -> &'static ProviderCapabilities {
    capabilities(ProviderId::OpenRouter)
}

/// Build a `ChatResponse` whose single choice carries `content` and the given
/// token counts — the shape a real provider returns for this one-shot call.
fn response_with(content: &str, prompt_tokens: u32, completion_tokens: u32) -> ChatResponse {
    ChatResponse {
        id: "test".into(),
        model: DEFAULT_CLASSIFIER_MODEL.into(),
        choices: vec![ChatChoice {
            message: AssistantMessage {
                content: Some(content.to_owned()),
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".into()),
        }],
        usage: UsageBlock {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            ..Default::default()
        },
    }
}

/// A classifier bound to a `ScriptedAdapter` that returns exactly `outcome`.
fn classifier_returning(outcome: Result<ChatResponse, InferenceError>) -> OpenRouterClassifier {
    let scripted = match outcome {
        Ok(response) => ScriptedAdapter::new("scripted", caps()).with_response(response),
        Err(error) => ScriptedAdapter::new("scripted", caps()).with_error(error),
    };
    OpenRouterClassifier::with_adapter(Arc::new(scripted), DEFAULT_CLASSIFIER_MODEL)
}

/// Build a production-shaped credentialed classifier over an EXPLICIT store, so
/// the test never reads the developer's real keyring via `default_store()`.
fn credentialed_over(store: MemoryKeyStore, model: &str) -> OpenRouterClassifier {
    let mut configurator = trusty_common::inference::Configurator::new();
    trusty_common::inference::register_default_factories(&mut configurator);
    OpenRouterClassifier {
        model: model.to_owned(),
        source: Source::Credentialed {
            configurator,
            store: Box::new(store),
        },
    }
}

/// Clear every credential/model env var the resolver consults, so a credentialed
/// test sees only its injected store.
fn clear_credential_env() {
    for var in [
        CLASSIFIER_MODEL_ENV,
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
    ] {
        // SAFETY: every caller is guarded by `#[serial(inference_env)]`.
        unsafe { std::env::remove_var(var) };
    }
}

/// An adapter that records the requests it is asked to serve.
///
/// Why: `ScriptedAdapter` proves what comes BACK; this proves what goes OUT —
/// that the migration issues exactly one blocking `chat` (never a stream), with
/// the configured slug and a single `user` turn carrying the pane text.
/// What: pushes each [`ChatRequest`] onto a shared `Vec` and answers with one
/// canned response.
/// Test: `classify_sends_one_user_turn`.
struct RecordingAdapter {
    /// Every request seen, in call order.
    seen: Mutex<Vec<ChatRequest>>,
    /// The canned reply for every call.
    reply: ChatResponse,
}

#[async_trait]
impl InferenceAdapter for RecordingAdapter {
    fn name(&self) -> &str {
        "recording"
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        caps()
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(request.clone());
        Ok(self.reply.clone())
    }
}

// ── extract_json ─────────────────────────────────────────────────────────────

/// Why: the common case — a bare JSON object must pass through untouched.
/// Test: itself.
#[test]
fn extract_json_plain() {
    assert_eq!(
        extract_json(r#"{"state":"idle"}"#),
        Some(r#"{"state":"idle"}"#)
    );
}

/// Why: models routinely wrap the object in a fenced block with prose around it;
/// dropping the prose is the whole point of the helper. Without it every fenced
/// reply would fail as a `Serialization` error.
/// Test: itself.
#[test]
fn extract_json_from_fenced_prose() {
    let raw = "Sure! Here you go:\n```json\n{\"state\": \"working\"}\n```\nHope that helps.";
    assert_eq!(extract_json(raw), Some("{\"state\": \"working\"}"));
}

/// Why: pins the documented "first `{` to LAST `}`" span, which is what makes a
/// nested object survive extraction intact.
/// Test: itself.
#[test]
fn extract_json_spans_to_last_brace() {
    let raw = "noise {\"a\": {\"b\": 1}} trailing";
    assert_eq!(extract_json(raw), Some("{\"a\": {\"b\": 1}}"));
}

/// Why: a brace-free reply (a refusal, an empty body) must yield `None` so the
/// caller parses the raw text and reports a real parse error rather than
/// panicking on a slice.
/// Test: itself.
#[test]
fn extract_json_none_without_braces() {
    assert_eq!(extract_json("the session looks idle"), None);
    assert_eq!(extract_json(""), None);
    assert_eq!(extract_json("{ unterminated"), None);
}

/// Why: `}` before `{` would make `text[start..=end]` a reverse range and PANIC
/// on a byte slice. The ordering guard is load-bearing, not cosmetic.
/// Test: itself.
#[test]
fn extract_json_none_when_reversed() {
    assert_eq!(extract_json("} then {"), None);
    // Degenerate equal-index case: a lone brace can never be both ends.
    assert_eq!(extract_json("{"), None);
}

// ── parse_state ──────────────────────────────────────────────────────────────

/// Why: every state named in the prompt must round-trip to its enum variant — a
/// typo here silently reports `Unknown` for a real state and the TUI/circuit
/// breaker would never see a session as blocked.
/// Test: itself.
#[test]
fn parse_state_maps_every_documented_state() {
    assert_eq!(parse_state("working"), ActivityState::Working);
    assert_eq!(parse_state("idle"), ActivityState::Idle);
    assert_eq!(
        parse_state("blocked_on_permission"),
        ActivityState::BlockedOnPermission
    );
    assert_eq!(parse_state("errored"), ActivityState::Errored);
    assert_eq!(parse_state("done"), ActivityState::Done);
    assert_eq!(parse_state("unknown"), ActivityState::Unknown);
}

/// Why: models capitalise. Case-insensitivity is the documented contract.
/// Test: itself.
#[test]
fn parse_state_is_case_insensitive() {
    assert_eq!(parse_state("WORKING"), ActivityState::Working);
    assert_eq!(
        parse_state("Blocked_On_Permission"),
        ActivityState::BlockedOnPermission
    );
}

/// Why: an off-script or empty state must degrade to `Unknown`, never fail the
/// check. Note it is an EXACT match, not a substring one — `"not working"` is
/// not `Working`.
/// Test: itself.
#[test]
fn parse_state_unknown_for_garbage() {
    assert_eq!(parse_state(""), ActivityState::Unknown);
    assert_eq!(parse_state("busy"), ActivityState::Unknown);
    assert_eq!(parse_state("not working"), ActivityState::Unknown);
}

// ── model resolution ─────────────────────────────────────────────────────────

/// Why: the default slug is the operator-visible contract this migration
/// promised not to move — it must stay `openai/gpt-4o-mini`.
/// Test: itself.
#[test]
#[serial(inference_env)]
fn model_defaults_when_env_unset() {
    clear_credential_env();
    assert_eq!(resolve_classifier_model(), DEFAULT_CLASSIFIER_MODEL);
    assert_eq!(DEFAULT_CLASSIFIER_MODEL, "openai/gpt-4o-mini");
}

/// Why: the env override is how operators switch models without recompiling; a
/// blank value must not shadow the default with an empty slug.
/// Test: itself.
#[test]
#[serial(inference_env)]
fn model_reads_env_override() {
    clear_credential_env();
    // SAFETY: guarded by `#[serial(inference_env)]`.
    unsafe { std::env::set_var(CLASSIFIER_MODEL_ENV, "vendor/some-model") };
    assert_eq!(resolve_classifier_model(), "vendor/some-model");

    // SAFETY: guarded by `#[serial(inference_env)]`.
    unsafe { std::env::set_var(CLASSIFIER_MODEL_ENV, "   ") };
    assert_eq!(resolve_classifier_model(), DEFAULT_CLASSIFIER_MODEL);
    clear_credential_env();
}

// ── classify ─────────────────────────────────────────────────────────────────

/// Why: the happy path end-to-end through the migrated call — a well-formed
/// verdict must survive extraction, parsing, and field defaulting, AND the
/// provider's token counts must reach the caller. Pre-#4427 the SSE path could
/// not see usage and returned a hard-coded `(0, 0)`, so this is the regression
/// guard for both the parse and the newly-live cost accounting.
/// Test: itself.
#[tokio::test]
async fn classify_parses_verdict_and_usage() {
    let classifier = classifier_returning(Ok(response_with(
        r#"{"state": "blocked_on_permission", "summary": "awaiting approval", "confidence": 0.83}"#,
        123,
        45,
    )));
    let (verdict, input, output) = classifier.classify("pane").await.expect("classifies");
    assert_eq!(verdict.state, ActivityState::BlockedOnPermission);
    assert_eq!(verdict.summary, "awaiting approval");
    assert!((verdict.confidence - 0.83).abs() < 1e-6);
    assert_eq!((input, output), (123, 45));
}

/// Why: a reply missing optional fields must not fail the check — the documented
/// defaults (`unknown` / `no summary` / `0.5`) keep a terse model usable.
/// Test: itself.
#[tokio::test]
async fn classify_defaults_missing_fields() {
    let classifier = classifier_returning(Ok(response_with(r#"{"summary": "just text"}"#, 1, 1)));
    let (verdict, _, _) = classifier.classify("pane").await.expect("classifies");
    assert_eq!(verdict.state, ActivityState::Unknown);
    assert_eq!(verdict.summary, "just text");
    assert!((verdict.confidence - 0.5).abs() < 1e-6);
}

/// Why: proves the REQUEST side of the migration — exactly ONE blocking `chat`
/// call (not a stream, not a retry loop), carrying the configured model slug and
/// a single `user` turn that embeds the pane text. A future edit that adds a
/// second call or drops the pane text fails here.
/// Test: itself.
#[tokio::test]
async fn classify_sends_one_user_turn() {
    let recorder = Arc::new(RecordingAdapter {
        seen: Mutex::new(Vec::new()),
        reply: response_with(
            r#"{"state": "working", "summary": "ok", "confidence": 1.0}"#,
            7,
            3,
        ),
    });
    let classifier = OpenRouterClassifier::with_adapter(recorder.clone(), "vendor/pinned-model");
    classifier
        .classify("PANE-SENTINEL-42")
        .await
        .expect("classifies");

    let seen = recorder.seen.lock().expect("recorded");
    assert_eq!(
        seen.len(),
        1,
        "exactly one inference call per classification"
    );
    let request = &seen[0];
    assert_eq!(request.model, "vendor/pinned-model");
    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, "user");
    let content = request.messages[0]
        .content
        .as_deref()
        .expect("user turn carries content");
    assert!(
        content.contains("PANE-SENTINEL-42"),
        "pane text is embedded"
    );
    assert!(
        content.contains("blocked_on_permission"),
        "states are listed"
    );
    assert!(request.tools.is_none(), "the classifier requests no tools");
}

/// Why: the fail-fast credential contract. An operator with no key configured
/// must get `MissingApiKey` — which `ActivityMonitor::check` turns into a
/// degraded `Unknown` verdict — and NOT a network call or a generic `Llm` error.
/// This drives the real credentialed source over an empty store, so it covers
/// the `Configurator::build` → `MissingCredential` → `MissingApiKey` mapping the
/// migration introduced.
/// Test: itself.
#[tokio::test]
#[serial(inference_env)]
async fn classify_missing_credential_maps_to_missing_api_key() {
    clear_credential_env();
    let classifier = credentialed_over(MemoryKeyStore::new(), DEFAULT_CLASSIFIER_MODEL);
    let err = classifier
        .classify("pane")
        .await
        .expect_err("empty store must not resolve a provider");
    assert!(matches!(err, ActivityError::MissingApiKey), "got {err:?}");
}

/// Why: the same mapping must hold when the ADAPTER (not the configurator)
/// reports the missing credential — e.g. a factory that resolves but finds no
/// usable key. Both routes have to reach the degrade arm.
/// Test: itself.
#[tokio::test]
async fn classify_adapter_missing_credential_maps_to_missing_api_key() {
    let classifier = classifier_returning(Err(InferenceError::MissingCredential {
        provider: ProviderId::OpenRouter,
    }));
    let err = classifier
        .classify("pane")
        .await
        .expect_err("credential error");
    assert!(matches!(err, ActivityError::MissingApiKey), "got {err:?}");
}

/// Why: a provider outage must be reported as `Llm`, never mis-attributed to
/// `Serialization` — the #3757 lesson, preserved by the blocking call. It must
/// also carry the provider's status/body so an operator can diagnose it.
/// Test: itself.
#[tokio::test]
async fn classify_adapter_error_maps_to_llm() {
    let classifier = classifier_returning(Err(InferenceError::Api {
        status: 503,
        body: "upstream unavailable".into(),
    }));
    let err = classifier.classify("pane").await.expect_err("api error");
    let ActivityError::Llm(message) = err else {
        panic!("expected ActivityError::Llm, got {err:?}");
    };
    assert!(message.contains("503"), "status is preserved: {message}");
    assert!(
        message.contains("upstream unavailable"),
        "body is preserved: {message}"
    );
}

/// Why: prose with no JSON in it is the model's fault, not the provider's, and
/// must be reported as `Serialization` with the raw text attached so an operator
/// can see what the model actually said.
/// Test: itself.
#[tokio::test]
async fn classify_unparseable_output_maps_to_serialization() {
    let classifier = classifier_returning(Ok(response_with(
        "I'm not sure what that session is doing.",
        5,
        9,
    )));
    let err = classifier.classify("pane").await.expect_err("unparseable");
    let ActivityError::Serialization(message) = err else {
        panic!("expected ActivityError::Serialization, got {err:?}");
    };
    assert!(
        message.contains("I'm not sure"),
        "raw model text is attached: {message}"
    );
}

/// Why: a truncated/malformed object (the shape a mid-stream cut used to
/// produce) must also be `Serialization`, not a silent `Unknown` verdict.
/// Test: itself.
#[tokio::test]
async fn classify_malformed_json_maps_to_serialization() {
    let classifier = classifier_returning(Ok(response_with(r#"{"state": "worki"#, 1, 0)));
    let err = classifier.classify("pane").await.expect_err("malformed");
    assert!(
        matches!(err, ActivityError::Serialization(_)),
        "got {err:?}"
    );
}

/// Why: a provider that returns a choice with no content at all must degrade to
/// a parse error rather than panicking on an unwrap of `first_text()`.
/// Test: itself.
#[tokio::test]
async fn classify_empty_content_maps_to_serialization() {
    let mut response = response_with("", 0, 0);
    response.choices[0].message.content = None;
    let classifier = classifier_returning(Ok(response));
    let err = classifier
        .classify("pane")
        .await
        .expect_err("empty content");
    assert!(
        matches!(err, ActivityError::Serialization(_)),
        "got {err:?}"
    );
}

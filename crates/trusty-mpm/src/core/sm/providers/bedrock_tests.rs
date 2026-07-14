//! Unit tests for the SM Bedrock provider (`bedrock` feature only).
//!
//! Why: as of #2411 the Bedrock Converse wire path lives in the shared
//! `trusty_common::inference::bedrock` adapter (covered by its own offline
//! conversion tests + an `#[ignore]`-gated live call). What remains SM-specific
//! — and what these tests pin — is the POLICY the bridge re-applies on top of
//! the shared adapter: cross-region inference-profile prefix validation, region
//! resolution, the deterministic empty-message guard, and the
//! [`SmLlmError`] classification the resolver's retry/alarm loop depends on. All
//! tests are offline: construction is lazy (no AWS client is built), and the
//! error-classification tests drive the pure `map_sdk_error` string classifier
//! directly rather than forcing a real SDK failure.
//! What: region resolution, model-id validation, offline construction, the
//! empty-message validation path, and error classification.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `bedrock.rs`.

use super::{BedrockProvider, LlmProvider, LlmRequest, map_sdk_error, validate_model_id};
use crate::core::sm::providers::SmLlmError;

/// Why: region resolution (`explicit` > env > `us-east-1`) is the shared
/// adapter's, but the SM must keep exposing the resolved region through
/// `BedrockProvider::region()`; pin that the explicit region flows through and
/// the empty/absent cases default to `us-east-1`.
/// What: constructs providers (offline — the AWS client is lazy) and asserts
/// `region()`.
/// Test: this is the test.
#[tokio::test]
async fn bedrock_region_resolution() {
    let explicit = BedrockProvider::new("us.anthropic.claude-haiku", Some("eu-west-1"))
        .await
        .expect("construct");
    assert_eq!(explicit.region(), "eu-west-1");

    // NOTE: this asserts the default only holds when neither TRUSTY_AWS_REGION
    // nor AWS_REGION is set in the test environment. The shared adapter's own
    // tests cover the env-var precedence exhaustively.
    if std::env::var("TRUSTY_AWS_REGION").is_err() && std::env::var("AWS_REGION").is_err() {
        let empty = BedrockProvider::new("us.anthropic.claude-haiku", Some(""))
            .await
            .expect("construct");
        assert_eq!(empty.region(), "us-east-1");
    }
}

#[test]
fn bedrock_prefix_validation() {
    for id in [
        "us.anthropic.claude-sonnet-4-6",
        "eu.anthropic.claude-sonnet-4-6",
        "ap.anthropic.claude-haiku",
        "jp.anthropic.claude-haiku",
        "global.anthropic.claude-opus",
    ] {
        assert!(validate_model_id(id).is_ok(), "{id} should validate");
    }
    let err = validate_model_id("anthropic.claude-sonnet-4-6").unwrap_err();
    assert!(matches!(err, SmLlmError::Validation(_)));
    assert!(err.is_alarm());
    assert!(!err.is_retryable());
}

/// Why: construction must report name/region/model without touching AWS (the
/// shared adapter's client is lazy).
/// What: builds a provider and checks `name()`/`region()`/`model`.
/// Test: no network.
#[tokio::test]
async fn bedrock_provider_stores_model_and_region() {
    let provider = BedrockProvider::new("us.anthropic.claude-haiku", Some("us-east-1"))
        .await
        .expect("construct");
    assert_eq!(provider.name(), "bedrock");
    assert_eq!(provider.region(), "us-east-1");
    assert_eq!(provider.model, "us.anthropic.claude-haiku");
}

/// Why: a bad model id must fail at construction, before any AWS work.
/// What: `new` with a bare (non-prefixed) id returns `Validation`.
/// Test: no network.
#[tokio::test]
async fn bedrock_new_rejects_bare_model_id() {
    let err = BedrockProvider::new("anthropic.claude-sonnet-4-6", None)
        .await
        .expect_err("bare id must fail validation");
    assert!(matches!(err, SmLlmError::Validation(_)));
}

/// Why: an empty message list is a deterministic client-side validation error,
/// not something to send upstream — the bridge must reject it before delegating
/// to the shared adapter (so this stays offline).
/// What: calls `complete` with no messages and asserts a `Validation` error.
/// Test: no network.
#[tokio::test]
async fn bedrock_empty_messages_is_validation_error() {
    let provider = BedrockProvider::new("us.anthropic.claude-sonnet-4-6", Some("us-east-1"))
        .await
        .expect("construct");
    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "sys".to_string(),
        messages: vec![],
        temperature: 0.3,
        max_tokens: 256,
    };
    let err = provider.complete(req).await.expect_err("empty must fail");
    assert!(matches!(err, SmLlmError::Validation(_)));
}

/// Why: the resolver's retry/alarm loop classifies via `is_retryable`/`is_alarm`;
/// after #2411 the SM classifies the shared adapter's
/// `InferenceError::Provider` message string (which wraps the AWS
/// `DisplayErrorContext` source chain). Pin that the real AWS error-class
/// markers still route to the right [`SmLlmError`] variant.
/// What: feeds representative commons-shaped error strings through
/// `map_sdk_error` and asserts the variant + retry/alarm classification.
/// Test: this is the test (pure — no client, no network).
#[test]
fn map_sdk_error_classifies_aws_markers() {
    let model = "us.anthropic.claude-sonnet-4-6";
    let region = "us-east-1";
    let wrap = |ctx: &str| {
        format!(
            "inference provider error: Converse call failed (model={model}, region={region}): {ctx}"
        )
    };

    // Access denied / credential failures → non-retryable alarm.
    for ctx in [
        "AccessDeniedException: User is not authorized to perform bedrock:InvokeModel",
        "NoCredentialsError: no credentials in the property bag",
        "dispatch failure: could not load credentials",
    ] {
        let err = map_sdk_error(wrap(ctx), model, region);
        assert!(
            matches!(err, SmLlmError::AccessDenied(_)),
            "{ctx} => {err:?}"
        );
        assert!(err.is_alarm() && !err.is_retryable());
    }

    // ResourceNotFound → model-not-found alarm.
    let err = map_sdk_error(
        wrap("ResourceNotFoundException: model not available"),
        model,
        region,
    );
    assert!(matches!(err, SmLlmError::ModelNotFound(_)));
    assert!(err.is_alarm());

    // Throttling → retryable rate-limit.
    let err = map_sdk_error(
        wrap("ThrottlingException: too many requests"),
        model,
        region,
    );
    assert!(matches!(err, SmLlmError::RateLimited));
    assert!(err.is_retryable());

    // 5xx service errors → retryable upstream.
    let err = map_sdk_error(wrap("ServiceUnavailableException"), model, region);
    assert!(matches!(err, SmLlmError::Upstream { status: 503, .. }));
    assert!(err.is_retryable());

    // ValidationException → non-retryable alarm.
    let err = map_sdk_error(
        wrap("ValidationException: malformed request"),
        model,
        region,
    );
    assert!(matches!(err, SmLlmError::Validation(_)));
    assert!(err.is_alarm());

    // Anything unrecognised → retryable transport (safe default).
    let err = map_sdk_error(wrap("connection reset by peer"), model, region);
    assert!(matches!(err, SmLlmError::Transport(_)));
    assert!(err.is_retryable());
}

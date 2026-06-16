//! Unit tests for the SM Bedrock provider (`bedrock` feature only).
//!
//! Why: extracted from `bedrock.rs` to keep that file under the 500-SLOC cap.
//! All tests are unit-level — no real AWS calls. The `complete`/`call_once`
//! path is exercised with a `no_credentials()` client, which errors inside the
//! SDK before any TCP connection, proving the error-mapping wiring.
//! What: region resolution, model-id validation, construction, and the
//! no-credentials `complete` error path. Cost estimation is covered centrally
//! in `pricing_tests.rs`.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `bedrock.rs`.

use super::{BedrockProvider, LlmProvider, LlmRequest, resolve_bedrock_region, validate_model_id};
use crate::core::sm::providers::{ChatMessage, SmLlmError};
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;

/// Build a `no_credentials()` Bedrock client for offline tests.
///
/// Why: `complete`/`call_once` need a client, but tests must not touch AWS; a
/// no-credentials client fails in the SDK before any network I/O.
/// What: loads a default config with a fixed region and no credentials.
/// Test: used by `bedrock_*` tests below.
async fn no_creds_client() -> BedrockClient {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_types::region::Region::new("us-east-1"))
        .no_credentials()
        .load()
        .await;
    BedrockClient::new(&config)
}

#[test]
fn bedrock_region_resolution() {
    assert_eq!(resolve_bedrock_region(Some("eu-west-1")), "eu-west-1");
    assert_eq!(resolve_bedrock_region(Some("")), "us-east-1");
    assert_eq!(resolve_bedrock_region(None), "us-east-1");
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

/// Why: the `from_client` accessors must report name/region without AWS calls.
/// What: builds a no-creds client and checks `name()`/`region()`.
/// Test: no network.
#[tokio::test]
async fn bedrock_provider_stores_model_and_region() {
    let provider = BedrockProvider::from_client(
        no_creds_client().await,
        "us.anthropic.claude-haiku",
        "us-east-1",
    );
    assert_eq!(provider.name(), "bedrock");
    assert_eq!(provider.region(), "us-east-1");
    assert_eq!(provider.model, "us.anthropic.claude-haiku");
}

/// Why: a misconfigured AWS environment must yield a typed [`SmLlmError`] (the
/// error-mapping path in `map_sdk_error`), never a panic.
/// What: calls `complete` on a no-credentials client and asserts it returns an
/// error mentioning credentials/Bedrock.
/// Test: the SDK errors before any TCP connection — no real network.
#[tokio::test]
async fn bedrock_no_credentials_returns_error() {
    let provider = BedrockProvider::from_client(
        no_creds_client().await,
        "us.anthropic.claude-sonnet-4-6",
        "us-east-1",
    );
    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "sys".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "go".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 256,
    };
    let err = provider.complete(req).await.expect_err("must fail offline");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("bedrock") || msg.contains("credential") || msg.contains("access"),
        "error should mention bedrock/credentials, got: {msg}"
    );
}

/// Why: an empty message list is a deterministic client-side validation error,
/// not something to send upstream.
/// What: calls `complete` with no messages and asserts a `Validation` error.
/// Test: no network.
#[tokio::test]
async fn bedrock_empty_messages_is_validation_error() {
    let provider = BedrockProvider::from_client(
        no_creds_client().await,
        "us.anthropic.claude-sonnet-4-6",
        "us-east-1",
    );
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

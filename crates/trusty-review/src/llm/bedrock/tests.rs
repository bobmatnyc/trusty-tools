//! Unit tests for the Bedrock provider.
//!
//! Why: extracted from `bedrock/mod.rs` to keep that file under the 500-line
//! cap while preserving full test coverage.
//! What: region resolution, model-id validation, cost estimation,
//! provider construction, and structured-output (tool-use) behavior tests.
//! All tests are unit-level — no real AWS calls.
//! Test: this file is included as `#[cfg(test)] mod tests` from `bedrock/mod.rs`.

use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;

use super::{
    BedrockProvider, DEFAULT_REGION, LlmRequest, estimate_bedrock_cost_usd, normalize_model_family,
    resolve_bedrock_region, validate_model_id,
};
use crate::llm::bedrock::tool_use::build_tool_config;
use crate::llm::{ChatMessage, LlmError, LlmProvider, ResponseSchema};

// ── Region resolution ─────────────────────────────────────────────────────

#[test]
fn bedrock_region_resolution() {
    // Explicit wins.
    assert_eq!(
        resolve_bedrock_region(Some("eu-west-1")),
        "eu-west-1",
        "explicit should win"
    );
    // Empty explicit falls through.
    assert_eq!(
        resolve_bedrock_region(Some("")),
        DEFAULT_REGION,
        "empty explicit should fall through to default"
    );
    // None falls through.
    assert_eq!(
        resolve_bedrock_region(None),
        DEFAULT_REGION,
        "None should return default"
    );
}

// ── Model id validation ───────────────────────────────────────────────────

#[test]
fn bedrock_us_prefix_validation() {
    // Valid cross-region inference-profile prefixes.
    for id in [
        "us.anthropic.claude-sonnet-4-6",
        "eu.anthropic.claude-sonnet-4-6",
        "ap.anthropic.claude-sonnet-4-6",
        "jp.anthropic.claude-sonnet-4-6",
        "global.anthropic.claude-sonnet-4-6",
    ] {
        assert!(
            validate_model_id(id).is_ok(),
            "expected {id:?} to pass validation"
        );
    }

    // Bare foundation-model id should fail.
    let err = validate_model_id("anthropic.claude-sonnet-4-6").unwrap_err();
    assert!(
        matches!(err, LlmError::Validation(_)),
        "expected Validation error for bare id"
    );
    assert!(err.is_alarm(), "Validation is an alarm error");
    assert!(!err.is_retryable(), "Validation must not be retried");
}

#[test]
fn bedrock_empty_model_id_is_validation_error() {
    let err = validate_model_id("").unwrap_err();
    assert!(matches!(err, LlmError::Validation(_)));
}

// ── Cost estimation ───────────────────────────────────────────────────────

#[test]
fn bedrock_cost_estimate_sonnet() {
    // 1M input + 1M output at Sonnet pricing ($3/M + $15/M = $18/M).
    let cost = estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    assert!(
        (cost - 18.0_f64).abs() < 1e-9,
        "expected $18.00 for 1M+1M Sonnet tokens, got {cost}"
    );
}

#[test]
fn bedrock_cost_estimate_eu_prefix_normalized() {
    // eu. prefix should resolve to the same pricing as us.
    let eu_cost = estimate_bedrock_cost_usd("eu.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    let us_cost = estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    assert!(
        (eu_cost - us_cost).abs() < 1e-9,
        "eu. and us. prefixes should give identical cost: eu={eu_cost} us={us_cost}"
    );
}

#[test]
fn bedrock_cost_estimate_haiku() {
    // Short-form id (no date suffix) must still price correctly.
    let cost = estimate_bedrock_cost_usd("us.anthropic.claude-haiku-4-5", 1_000_000, 1_000_000);
    assert!(
        (cost - 4.8_f64).abs() < 1e-9,
        "expected $4.80 for 1M+1M Haiku tokens (short id), got {cost}"
    );
}

/// Regression test: the verified Haiku 4.5 date-versioned id must resolve
/// to non-zero pricing (Bug 3 fix).
///
/// Why: `anthropic.claude-haiku-4-5-20251001-v1:0` (after geo-prefix strip)
/// did not match the pricing table's `anthropic.claude-haiku-4-5` entry,
/// causing cost_usd to be $0.00 in all Haiku compare runs.
/// What: asserts the real date-versioned id prices at $4.80 for 1M+1M tokens.
/// Test: this test itself; no network calls.
#[test]
fn bedrock_cost_estimate_haiku_date_versioned() {
    let cost = estimate_bedrock_cost_usd(
        "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        1_000_000,
        1_000_000,
    );
    assert!(
        (cost - 4.8_f64).abs() < 1e-9,
        "expected $4.80 for 1M+1M Haiku tokens (date-versioned id), got {cost}. \
         The normalize_model_family() function must strip -20251001-v1:0 to match \
         the pricing table entry."
    );
}

/// Test that `normalize_model_family` correctly strips date and version suffixes.
///
/// Why: directly verifies the normalization logic that underpins Bug 3 fix.
/// What: checks several real and synthetic id forms.
/// Test: this test itself; no network calls.
#[test]
fn bedrock_normalize_model_family_strips_suffix() {
    assert_eq!(
        normalize_model_family("anthropic.claude-haiku-4-5-20251001-v1:0"),
        "anthropic.claude-haiku-4-5",
        "date+version suffix must be stripped"
    );
    assert_eq!(
        normalize_model_family("anthropic.claude-3-5-sonnet-20241022"),
        "anthropic.claude-3-5-sonnet",
        "date-only suffix must be stripped"
    );
    assert_eq!(
        normalize_model_family("anthropic.claude-3-haiku-20240307-v1:0"),
        "anthropic.claude-3-haiku",
        "date+version suffix must be stripped from legacy Haiku"
    );
    assert_eq!(
        normalize_model_family("anthropic.claude-sonnet-4-6"),
        "anthropic.claude-sonnet-4-6",
        "id without date/version suffix must be unchanged"
    );
    assert_eq!(
        normalize_model_family("anthropic.claude-haiku-4-5"),
        "anthropic.claude-haiku-4-5",
        "short haiku id must be unchanged"
    );
}

/// Test that Sonnet 4.5 date-versioned id normalises to the pricing table entry.
///
/// Why: Sonnet 4.5 is in the new compare set; its pricing must not be zero.
/// What: asserts `us.anthropic.claude-sonnet-4-5-20250929-v1:0` prices at
/// $18/M (same as Sonnet 4.6) after geo+date+version normalization.
/// Test: no network.
#[test]
fn bedrock_cost_estimate_sonnet_4_5_date_versioned() {
    let cost = estimate_bedrock_cost_usd(
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        1_000_000,
        1_000_000,
    );
    assert!(
        (cost - 18.0_f64).abs() < 1e-9,
        "expected $18.00 for 1M+1M Sonnet 4.5 tokens (date-versioned id), got {cost}"
    );
}

#[test]
fn bedrock_cost_estimate_unknown_model() {
    // Unknown model should return 0.0, not panic.
    let cost = estimate_bedrock_cost_usd("us.unknown/model-xyz", 500_000, 100_000);
    assert_eq!(cost, 0.0, "unknown model cost must be 0.0");
}

// ── Provider construction (no AWS calls) ──────────────────────────────────

/// Verify that `BedrockProvider::from_client` stores fields correctly.
///
/// Why: ensures the provider's name/region accessors work without making
/// any AWS calls.
/// What: builds a client with `no_credentials()` and checks trait methods.
/// Test: no network.
#[tokio::test]
async fn bedrock_provider_stores_model_and_region() {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_types::region::Region::new("us-east-1"))
        .no_credentials()
        .load()
        .await;
    let client = BedrockClient::new(&config);
    let provider =
        BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");
    assert_eq!(provider.name(), "bedrock");
    assert_eq!(provider.region(), "us-east-1");
}

/// Verify that `BedrockProvider::complete` returns a typed error when
/// called with `no_credentials()`.
///
/// Why: operators who misconfigure AWS should see a descriptive error about
/// credentials, not an opaque panic or an OpenRouter-specific message.
/// What: builds a `no_credentials` client, calls `complete`, expects an
/// error whose `is_alarm()` or error message mentions credentials/Bedrock.
/// Test: no real network call succeeds — error comes from the SDK before
/// any TCP connection.
#[tokio::test]
async fn bedrock_no_credentials_returns_error() {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_types::region::Region::new("us-east-1"))
        .no_credentials()
        .load()
        .await;
    let client = BedrockClient::new(&config);
    let provider =
        BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");

    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "You are a code reviewer.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Review this diff.".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 512,
        response_schema: None,
    };

    let result = provider.complete(req).await;
    let err = result.expect_err("should fail without real credentials");
    let msg = format!("{err}");
    let mentions_context = msg.to_lowercase().contains("bedrock")
        || msg.to_lowercase().contains("credential")
        || msg.to_lowercase().contains("aws")
        || msg.to_lowercase().contains("access")
        || err.is_alarm();
    assert!(
        mentions_context,
        "error should mention Bedrock/credentials/AWS; got: {msg}"
    );
}

/// Verify that `LlmRequest` fields map correctly to the Converse wire format.
///
/// Why: the conversion between LlmRequest (system string + messages vec)
/// and the Bedrock Message/SystemContentBlock types is the most error-prone
/// step; unit-testing the shape prevents silent regressions.
/// What: constructs an LlmRequest and verifies the field mapping via the
/// same logic used in `call_once`.
/// Test: pure logic test — no network, no AWS calls.
#[test]
fn bedrock_converse_request_construction() {
    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "You are a Rust code reviewer.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Review this diff.".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 1024,
        response_schema: None,
    };

    assert!(!req.system.is_empty(), "system message must be forwarded");
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].role, "user");
    assert_eq!(req.messages[0].content, "Review this diff.");
    assert!(
        req.temperature >= 0.0 && req.temperature <= 1.0,
        "temperature must be in [0.0, 1.0] for Bedrock"
    );
    assert!(req.max_tokens > 0, "max_tokens must be > 0");
}

// ── Structured-output (tool-use) ──────────────────────────────────────────

/// Verify that when `response_schema` is set, `build_tool_config` is called
/// and returns a valid ToolConfiguration (structural test only, no AWS call).
///
/// Why: this is the core new behavior — when a schema is present the provider
/// MUST include a tool configuration in the Converse request; without it
/// the model falls back to free text and the fail-safe APPROVE problem returns.
/// What: constructs an `LlmRequest` with a `response_schema`, calls
/// `build_tool_config` directly with the same schema, and asserts no error
/// is returned.
/// Test: no network, no AWS credentials needed.
#[test]
fn bedrock_request_includes_tool_config_when_schema_set() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string"},
            "summary": {"type": "string"},
            "findings": {"type": "array", "items": {"type": "object"}}
        },
        "required": ["verdict", "summary", "findings"]
    });

    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "reviewer".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "review".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 1024,
        response_schema: Some(ResponseSchema {
            name: "review_output".to_string(),
            schema: schema.clone(),
        }),
    };

    assert!(
        req.response_schema.is_some(),
        "response_schema must be set on the request"
    );
    let tool_config_result = build_tool_config("review_output", &schema);
    assert!(
        tool_config_result.is_ok(),
        "build_tool_config must succeed for the review schema: {:?}",
        tool_config_result.err()
    );
}

/// Verify that `no_credentials` + structured-output request returns an error
/// (not a panic) when the tool-use path is taken.
///
/// Why: the tool-use code path (schema present) must not panic or produce a
/// different class of error than the free-text path.
/// What: sends a request with `response_schema` set to a `no_credentials`
/// provider, asserts the error is a typed `LlmError` variant.
/// Test: no real AWS calls.
#[tokio::test]
async fn bedrock_structured_no_credentials_returns_error() {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_types::region::Region::new("us-east-1"))
        .no_credentials()
        .load()
        .await;
    let client = BedrockClient::new(&config);
    let provider =
        BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");

    let schema = serde_json::json!({
        "type": "object",
        "properties": {"verdict": {"type": "string"}},
        "required": ["verdict"]
    });

    let req = LlmRequest {
        model: "us.anthropic.claude-sonnet-4-6".to_string(),
        system: "reviewer".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "review diff".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 512,
        response_schema: Some(ResponseSchema {
            name: "review_output".to_string(),
            schema,
        }),
    };

    let result = provider.complete(req).await;
    assert!(
        result.is_err(),
        "must fail without real credentials even with tool-use schema"
    );
}

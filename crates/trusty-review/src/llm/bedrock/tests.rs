//! Unit tests for the Bedrock provider.
//!
//! Why: extracted from `bedrock/mod.rs` to keep that file under the 500-line
//! cap while preserving full test coverage.
//! What: region resolution, model-id validation, cost estimation,
//! provider construction, SDK-error rendering, and structured-output (tool-use)
//! behavior tests. All tests are unit-level — no real AWS calls.
//! Test: this file is included as `#[cfg(test)] mod tests` from `bedrock/mod.rs`.

use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::error::SdkError;
use aws_sdk_bedrockruntime::operation::converse::ConverseError;
use aws_sdk_bedrockruntime::types::error::ResourceNotFoundException;
use aws_smithy_types::error::ErrorMetadata;

use trusty_common::inference::bedrock::{DEFAULT_REGION, resolve_region_from};

use super::{
    BedrockProvider, LlmRequest, describe_sdk_error, estimate_bedrock_cost_usd,
    normalize_model_family, resolve_bedrock_region, validate_model_id,
};
use crate::llm::bedrock::tool_use::{self, build_tool_config};
use crate::llm::{ChatMessage, LlmError, LlmProvider, ResponseSchema};

// ── Region resolution ─────────────────────────────────────────────────────

/// Pin the precedence order explicit > `TRUSTY_AWS_REGION` > `AWS_REGION` >
/// default.
///
/// Why: operators set the region through different env vars in different
/// deployment contexts, so the ordering must stay stable. #5706: this test used
/// to call `resolve_bedrock_region` and assert that an empty explicit region
/// reaches the default, which skips the two env tiers and fails outright on any
/// machine with `AWS_REGION` exported. #5469: the walk itself now lives in
/// `trusty_common::inference::bedrock`, so this test drives the SHARED
/// implementation — reorder its tiers and these assertions fail.
/// What: drives the pure `resolve_region_from` walk with both env tiers passed
/// as arguments, so every case is deterministic. The one `resolve_bedrock_region`
/// assertion left exercises the tier that is env-independent by construction.
/// Test: this test.
#[test]
fn bedrock_region_resolution() {
    assert_eq!(
        resolve_region_from(Some("eu-west-1"), Some("ap-south-1"), Some("us-west-2")),
        "eu-west-1",
        "explicit should win over both env tiers"
    );
    assert_eq!(
        resolve_region_from(Some(""), Some("ap-south-1"), Some("us-west-2")),
        "ap-south-1",
        "empty explicit should fall through to TRUSTY_AWS_REGION"
    );
    assert_eq!(
        resolve_region_from(None, None, Some("us-west-2")),
        "us-west-2",
        "AWS_REGION should be used when TRUSTY_AWS_REGION is unset"
    );
    assert_eq!(
        resolve_region_from(Some(""), None, None),
        DEFAULT_REGION,
        "empty explicit with no env should reach the default"
    );
    assert_eq!(
        resolve_region_from(None, None, None),
        DEFAULT_REGION,
        "None with no env should reach the default"
    );
    assert_eq!(
        resolve_region_from(Some(""), Some("  "), Some("\t")),
        DEFAULT_REGION,
        "a whitespace-only env var counts as unset"
    );
    assert_eq!(
        resolve_region_from(None, Some(" eu-central-1 "), None),
        "eu-central-1",
        "an env tier is trimmed before use"
    );
    assert_eq!(
        resolve_bedrock_region(Some("eu-west-1")),
        "eu-west-1",
        "the public wrapper's explicit tier ignores the environment"
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

/// Per-million `(input, output)` rate the estimator applies to `model`.
fn priced(model: &str) -> (f64, f64) {
    (
        estimate_bedrock_cost_usd(model, 1_000_000, 0),
        estimate_bedrock_cost_usd(model, 0, 1_000_000),
    )
}

/// Why: a `us.` profile bills at the AWS "Regional" rate (10% above global)
/// and a `global.` profile at the global rate; pricing every id at one rate
/// misreported the reviewer default's cost, and an unpriced Sonnet 5.5 id
/// reported every review as $0.
/// What: asserts the exact input and output rate for the four compare-set
/// families under both prefixes, against the AWS Price List API values
/// recorded in `pricing.rs` (retrieved 2026-10-05).
/// Test: this test itself; no network calls.
#[test]
fn bedrock_cost_estimate_matches_price_list_per_profile() {
    let cases: [(&str, (f64, f64)); 8] = [
        ("us.anthropic.claude-sonnet-5-5", (2.20, 11.00)),
        ("global.anthropic.claude-sonnet-5-5", (2.00, 10.00)),
        ("us.anthropic.claude-opus-5-5", (4.40, 22.00)),
        ("global.anthropic.claude-opus-5-5", (4.00, 20.00)),
        ("us.anthropic.claude-sonnet-4-6", (3.30, 16.50)),
        ("global.anthropic.claude-sonnet-4-6", (3.00, 15.00)),
        ("us.anthropic.claude-haiku-4-5-20251001-v1:0", (1.10, 5.50)),
        (
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
            (1.00, 5.00),
        ),
    ];
    for (model, expected) in cases {
        assert_eq!(
            priced(model),
            expected,
            "per-MTok (input, output) for {model}"
        );
    }
}

#[test]
fn bedrock_cost_estimate_sonnet() {
    // 1M input + 1M output on the us. profile: $3.30 + $16.50 = $19.80.
    let cost = estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    assert!(
        (cost - 19.8_f64).abs() < 1e-9,
        "expected $19.80 for 1M+1M Sonnet 4.6 us. tokens, got {cost}"
    );
}

#[test]
fn bedrock_cost_estimate_eu_prefix_normalized() {
    // eu. is a geographic profile, so it prices at the same geo rate as us.
    let eu_cost = estimate_bedrock_cost_usd("eu.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    let us_cost = estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
    assert!(
        (eu_cost - us_cost).abs() < 1e-9,
        "eu. and us. prefixes should give identical cost: eu={eu_cost} us={us_cost}"
    );
}

#[test]
fn bedrock_cost_estimate_haiku() {
    // Short-form id (no date suffix) must price like the date-versioned one.
    let cost = estimate_bedrock_cost_usd("us.anthropic.claude-haiku-4-5", 1_000_000, 1_000_000);
    assert!(
        (cost - 6.6_f64).abs() < 1e-9,
        "expected $6.60 for 1M+1M Haiku us. tokens (short id), got {cost}"
    );
}

/// Regression test: the verified Haiku 4.5 date-versioned id must resolve
/// to its geo price, not zero and not the retired 0.80/4.00 rate.
///
/// Why: `anthropic.claude-haiku-4-5-20251001-v1:0` (after geo-prefix strip)
/// once missed the `anthropic.claude-haiku-4-5` entry and priced at $0.00.
/// What: asserts the real date-versioned id prices at $6.60 for 1M+1M tokens.
/// Test: this test itself; no network calls.
#[test]
fn bedrock_cost_estimate_haiku_date_versioned() {
    let cost = estimate_bedrock_cost_usd(
        "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        1_000_000,
        1_000_000,
    );
    assert!(
        (cost - 6.6_f64).abs() < 1e-9,
        "expected $6.60 for 1M+1M Haiku us. tokens (date-versioned id), got {cost}. \
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
/// Why: a `--models` override can still name Sonnet 4.5; its pricing must not
/// be zero.
/// What: asserts `us.anthropic.claude-sonnet-4-5-20250929-v1:0` prices at
/// $19.80 for 1M+1M (geo rate, same as Sonnet 4.6) after normalization.
/// Test: no network.
#[test]
fn bedrock_cost_estimate_sonnet_4_5_date_versioned() {
    let cost = estimate_bedrock_cost_usd(
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        1_000_000,
        1_000_000,
    );
    assert!(
        (cost - 19.8_f64).abs() < 1e-9,
        "expected $19.80 for 1M+1M Sonnet 4.5 us. tokens (date-versioned id), got {cost}"
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

// ── Per-model tool choice (#9292) ─────────────────────────────────────────

/// The structured-output schema the #9292 tests send.
fn review_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {"verdict": {"type": "string"}},
        "required": ["verdict"]
    })
}

/// A structured review request with `system` as its system prompt.
fn structured_request(system: &str) -> LlmRequest {
    LlmRequest {
        model: String::new(),
        system: system.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "review diff".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 512,
        response_schema: Some(ResponseSchema {
            name: "review_output".to_string(),
            schema: review_schema(),
        }),
    }
}

/// Model ids Bedrock rejects a forced `toolChoice` for, in each id shape.
const AUTO_MODEL_IDS: &[&str] = &[
    "anthropic.claude-sonnet-5-5",
    "us.anthropic.claude-sonnet-5-5",
    "global.anthropic.claude-sonnet-5-5",
    "bedrock/us.anthropic.claude-sonnet-5-5",
    "anthropic.claude-opus-5-5",
    "us.anthropic.claude-opus-5-5",
    "bedrock/us.anthropic.claude-opus-5-5",
    "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/apac.anthropic.claude-sonnet-5-5",
    "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/au.anthropic.claude-opus-5-5",
];

/// Model ids that keep the forced `toolChoice`.
const FORCED_MODEL_IDS: &[&str] = &[
    "us.anthropic.claude-sonnet-4-6",
    "bedrock/us.anthropic.claude-sonnet-4-6",
    "us.anthropic.claude-haiku-4-5-20251001-v1:0",
    "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
    "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/apac.anthropic.claude-sonnet-4-5",
    "us.amazon.nova-pro-v1:0",
];

/// Sonnet 5.5 and Opus 5.5 get `auto`; Sonnet 4.6 and Haiku 4.5 get the
/// exact forced configuration they had before #9292.
///
/// Why: Bedrock rejects `toolChoice = tool` for the 5.5 models, which broke the
/// default reviewer (#9292); every other model must keep its request.
/// What: builds the per-model config for each id shape and checks the choice;
/// a forced model's config must equal `build_tool_config`'s output.
/// Test: this test.
#[test]
fn build_tool_config_for_model_picks_choice_per_model() {
    use aws_sdk_bedrockruntime::types::ToolChoice;
    for model in AUTO_MODEL_IDS {
        let config =
            tool_use::build_tool_config_for_model(model, "review_output", &review_schema())
                .expect("config builds");
        assert!(
            matches!(config.tool_choice(), Some(ToolChoice::Auto(_))),
            "{model} must get toolChoice auto, got {:?}",
            config.tool_choice()
        );
        assert_eq!(config.tools().len(), 1, "{model} keeps the tool definition");
    }
    let forced = build_tool_config("review_output", &review_schema()).expect("config builds");
    for model in FORCED_MODEL_IDS {
        let config =
            tool_use::build_tool_config_for_model(model, "review_output", &review_schema())
                .expect("config builds");
        assert!(
            matches!(config.tool_choice(), Some(ToolChoice::Tool(_))),
            "{model} must keep the forced tool choice"
        );
        assert_eq!(config, forced, "{model} config must be unchanged");
    }
}

/// The capability check reads every model-id shape the provider accepts.
///
/// Why: a model reaches the provider bare, prefixed, date-stamped or as an
/// ARN; a shape the check misreads sends a forced request that fails (#9292).
/// What: covers suffixes, regional prefixes and each ARN kind. An
/// application-inference-profile ARN names no model, so it gets `auto`.
/// Test: this test.
#[test]
fn forced_tool_choice_capability_per_model_id_shape() {
    let cases = [
        ("us.anthropic.claude-sonnet-5-5-20260901-v1:0", false),
        ("eu.anthropic.claude-opus-5-5", false),
        (
            "arn:aws:bedrock:us-east-2:111122223333:inference-profile/us.anthropic.claude-sonnet-5-5",
            false,
        ),
        (
            "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-opus-5-5",
            false,
        ),
        (
            "arn:aws:bedrock:us-west-2:111122223333:application-inference-profile/9iatxd8u1751",
            false,
        ),
        (
            "arn:aws:bedrock:us-east-2:111122223333:inference-profile/us.anthropic.claude-sonnet-4-6",
            true,
        ),
        (
            "arn:aws:bedrock:eu-central-1::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0",
            true,
        ),
        ("us.anthropic.claude-opus-4-8", true),
        ("us.anthropic.claude-sonnet-4-5-20250929-v1:0", true),
        // #9292: region prefixes absent from the shared prefix list.
        (
            "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/apac.anthropic.claude-sonnet-5-5",
            false,
        ),
        (
            "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/au.anthropic.claude-opus-5-5",
            false,
        ),
        ("jp.anthropic.claude-sonnet-5-5-20260901-v1:0", false),
        ("us-gov.anthropic.claude-opus-5-5", false),
        (
            "arn:aws:bedrock:ap-southeast-2:111122223333:inference-profile/apac.anthropic.claude-sonnet-4-5-20250929-v1:0",
            true,
        ),
        ("au.anthropic.claude-sonnet-4-5", true),
        ("us.amazon.nova-pro-v1:0", true),
        ("claude-sonnet-5-5", false),
    ];
    for (model, expected) in cases {
        assert_eq!(
            tool_use::supports_forced_tool_choice(model),
            expected,
            "supports_forced_tool_choice({model})"
        );
    }
    for model in AUTO_MODEL_IDS {
        assert!(!tool_use::supports_forced_tool_choice(model), "{model}");
    }
    for model in FORCED_MODEL_IDS {
        assert!(tool_use::supports_forced_tool_choice(model), "{model}");
    }
}

/// Every compare-set candidate and the reviewer default has a stated answer.
///
/// Why: the compare set and the default reviewer are the ids operators run;
/// the reviewer default is Sonnet 5.5, which forcing broke (#9292).
/// What: pins the expected answer for each `COMPARE_CANDIDATE_MODELS` entry,
/// in order, so an added candidate fails here until it gets one.
/// Test: this test.
#[test]
fn forced_tool_choice_capability_covers_every_compare_candidate() {
    use crate::llm::models::{COMPARE_CANDIDATE_MODELS, DEFAULT_REVIEWER_MODEL};
    let expected = [
        ("bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0", true),
        ("bedrock/us.anthropic.claude-sonnet-4-6", true),
        ("bedrock/us.anthropic.claude-sonnet-5-5", false),
        ("bedrock/us.anthropic.claude-opus-5-5", false),
    ];
    let ids: Vec<&str> = expected.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids, COMPARE_CANDIDATE_MODELS,
        "a compare candidate has no case"
    );
    for (model, forced) in expected {
        assert_eq!(
            tool_use::supports_forced_tool_choice(model),
            forced,
            "supports_forced_tool_choice({model})"
        );
    }
    assert!(!tool_use::supports_forced_tool_choice(
        DEFAULT_REVIEWER_MODEL
    ));
}

/// Only an `auto` model's structured request gets the extra system line.
///
/// Why: `auto` alone does not require a tool call, so the request asks for
/// one; a forced model's request must stay byte-identical (#9292).
/// What: Sonnet 5.5 and Opus 5.5 get the request's system block plus the tool
/// line; Sonnet 4.6, Haiku 4.5 and an unstructured request get only the
/// system block.
/// Test: this test.
#[test]
fn system_blocks_add_the_tool_line_only_for_auto_models() {
    use aws_sdk_bedrockruntime::types::SystemContentBlock;
    let req = structured_request("reviewer");
    let base = SystemContentBlock::Text("reviewer".to_string());
    let line = SystemContentBlock::Text(tool_use::auto_tool_instruction("review_output"));
    for model in AUTO_MODEL_IDS {
        assert_eq!(
            super::system_blocks(&req, model),
            vec![base.clone(), line.clone()],
            "{model} must carry the tool line"
        );
    }
    for model in FORCED_MODEL_IDS {
        assert_eq!(
            super::system_blocks(&req, model),
            vec![base.clone()],
            "{model} system blocks must be unchanged"
        );
    }
    let unstructured = LlmRequest {
        response_schema: None,
        ..structured_request("reviewer")
    };
    assert_eq!(
        super::system_blocks(&unstructured, "us.anthropic.claude-sonnet-5-5"),
        vec![base],
        "an unstructured request never gets the tool line"
    );
    assert!(
        tool_use::auto_tool_instruction("review_output").contains("`review_output` tool"),
        "the line names the tool"
    );
}

/// A Converse reply holding one text block and no `toolUse` block.
fn text_only_reply(text: &str) -> aws_sdk_bedrockruntime::operation::converse::ConverseOutput {
    use aws_sdk_bedrockruntime::types::{
        ContentBlock, ConversationRole, ConverseMetrics, ConverseOutput as Output, Message,
        StopReason, TokenUsage,
    };
    let message = Message::builder()
        .role(ConversationRole::Assistant)
        .content(ContentBlock::Text(text.to_string()))
        .build()
        .expect("message builds");
    aws_sdk_bedrockruntime::operation::converse::ConverseOutput::builder()
        .output(Output::Message(message))
        .stop_reason(StopReason::EndTurn)
        .usage(
            TokenUsage::builder()
                .input_tokens(10)
                .output_tokens(5)
                .total_tokens(15)
                .build()
                .expect("usage builds"),
        )
        .metrics(
            ConverseMetrics::builder()
                .latency_ms(1)
                .build()
                .expect("metrics builds"),
        )
        .build()
        .expect("output builds")
}

/// An `auto` reply in text, not `toolUse`, reaches the review parser, which
/// parses JSON text or fails closed.
///
/// Why: an `auto` model may answer in prose (#9292); that reply must never
/// read as a successful review unless it carries the review JSON.
/// What: JSON text parses to its verdict; prose with a verdict keyword is
/// the fail-safe UNKNOWN, as a Haiku free-text reply is today.
/// Test: this test.
#[test]
fn auto_mode_free_text_reply_reaches_the_review_parser() {
    use crate::models::Verdict;
    use crate::pipeline::parser::parse_review_response;
    let json = r#"{"verdict":"APPROVE","summary":"Clean change.","findings":[]}"#;
    let text = super::response_text(&text_only_reply(json), true);
    assert_eq!(text, json, "the text block is the reply");
    let parsed = parse_review_response(&text);
    assert!(!parsed.is_fail_safe, "JSON text must parse");
    assert_eq!(parsed.verdict, Verdict::Approve);

    let prose = "The change looks fine to me. APPROVE";
    let text = super::response_text(&text_only_reply(prose), true);
    assert_eq!(text, prose);
    let parsed = parse_review_response(&text);
    assert!(parsed.is_fail_safe, "prose must fail closed");
    assert_eq!(parsed.verdict, Verdict::Unknown);
}

// ── SDK error rendering (#6912) ───────────────────────────────────────────

/// Build the `SdkError` a real Bedrock `ResourceNotFoundException` produces.
///
/// Why: the deserializer fills `ErrorMetadata` with the AWS code (from
/// `__type`) and message, and that pair is what the operator needs to see.
/// What: wraps the modeled exception in `ConverseError` and in the
/// `ServiceError` variant; the raw-response slot is `()` because
/// `describe_sdk_error` never reads it.
/// Test: used by the three tests below.
fn resource_not_found_sdk_error(message: &str) -> SdkError<ConverseError, ()> {
    let exception = ResourceNotFoundException::builder()
        .message(message)
        .meta(
            ErrorMetadata::builder()
                .code("ResourceNotFoundException")
                .message(message)
                .build(),
        )
        .build();
    SdkError::service_error(ConverseError::ResourceNotFoundException(exception), ())
}

/// A Bedrock service error must surface its AWS code and message.
///
/// Why: #6912 — `SdkError`'s `Display` renders every service error as the bare
/// literal "service error", so a wrong region, a missing credential, and an
/// unapproved model were indistinguishable in the reported error. Sessions lost
/// days to "inference: unreachable" diagnoses because of it.
/// What: renders a `ResourceNotFoundException`-shaped service error and asserts
/// the code and the message both survive. The last assertion pins the SDK
/// behavior this works around: `to_string()` — the expression the mapper used
/// before this fix — still yields only "service error", so the same assertions
/// applied to `origin/main`'s expression fail.
/// Test: this test.
#[test]
fn bedrock_service_error_surfaces_code_and_message() {
    let message = "Model use case details have not been submitted for this account.";
    let sdk_err = resource_not_found_sdk_error(message);

    let rendered = describe_sdk_error(&sdk_err);
    assert!(
        rendered.contains("ResourceNotFoundException"),
        "rendered error must name the AWS error code, got: {rendered}"
    );
    assert!(
        rendered.contains(message),
        "rendered error must carry the AWS message, got: {rendered}"
    );
    assert_ne!(
        rendered, "service error",
        "the flattened SDK Display must not be what reaches the operator"
    );

    assert_eq!(
        sdk_err.to_string(),
        "service error",
        "SdkError's own Display is still the flattened literal this fix routes around"
    );
}

/// A service error carrying no metadata falls back to the modeled Display.
///
/// Why: metadata is populated by the response deserializer, so a synthesized or
/// unparsed error can reach the mapper with an empty `ErrorMetadata`; falling
/// back to the SDK's flattened Display there would reintroduce #6912.
/// What: builds the same exception with no `meta`, and asserts the fallback is
/// the modeled error's own Display — which already spells the code and message.
/// Test: this test.
#[test]
fn bedrock_service_error_without_metadata_falls_back_to_display() {
    let exception = ResourceNotFoundException::builder()
        .message("no metadata attached")
        .build();
    let sdk_err: SdkError<ConverseError, ()> =
        SdkError::service_error(ConverseError::ResourceNotFoundException(exception), ());

    let rendered = describe_sdk_error(&sdk_err);
    assert_eq!(
        rendered, "ResourceNotFoundException: no metadata attached",
        "empty metadata must fall back to the modeled error's Display"
    );
}

/// Non-service `SdkError` variants keep the SDK's existing rendering.
///
/// Why: timeout, dispatch, and construction failures carry no error metadata,
/// so #6912's fix must leave their wording untouched rather than invent one.
/// What: renders a timeout and a construction failure and asserts each matches
/// `SdkError`'s own Display exactly.
/// Test: this test.
#[test]
fn bedrock_timeout_error_keeps_sdk_rendering() {
    let timeout: SdkError<ConverseError, ()> = SdkError::timeout_error("read timed out");
    assert_eq!(describe_sdk_error(&timeout), "request has timed out");

    let construction: SdkError<ConverseError, ()> =
        SdkError::construction_failure("missing model id");
    assert_eq!(
        describe_sdk_error(&construction),
        "failed to construct request"
    );
}

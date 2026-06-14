use crate::classify::tiers::llm::{
    LlmClassifier, LlmVerdict, ANTHROPIC_API_VERSION, ANTHROPIC_DEFAULT_MODEL, ANTHROPIC_ENDPOINT,
    SYSTEM_PROMPT,
};
use crate::core::config::LlmSource;
use crate::core::models::ClassificationMethod;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn has_api_key_reflects_key_state() {
    let with_key = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()));
    assert!(with_key.has_api_key());
    let without_key = LlmClassifier::new("gpt-4o-mini", None);
    assert!(!without_key.has_api_key());
}

/// Why: build_anthropic must set use_anthropic_format and embed the
/// anthropic-version header; a missing or wrong header causes the real
/// API to reject requests with HTTP 400.
/// What: construct a classifier via build_anthropic and verify
/// use_anthropic_format is true and the header is present in extra_headers.
/// Test: pure field inspection, no network.
#[test]
fn build_anthropic_sets_format_flag_and_version_header() {
    let llm = LlmClassifier::build_anthropic(
        "claude-3-5-haiku-latest",
        Some("sk-ant-test".to_string()), // pragma: allowlist secret
    );
    assert!(llm.use_anthropic_format, "must set use_anthropic_format");
    assert!(llm.api_key.is_some(), "api_key must be set");
    assert_eq!(llm.endpoint, ANTHROPIC_ENDPOINT);
    assert!(
        llm.extra_headers.contains_key("anthropic-version"),
        "anthropic-version header must be set"
    );
    let ver = llm.extra_headers.get("anthropic-version").unwrap();
    assert_eq!(ver, ANTHROPIC_API_VERSION);
}

/// Why: when build_anthropic is called with no key, has_api_key must
/// return false so the pipeline fail-loudly guard fires before any DB
/// writes occur.
/// What: construct with None and assert has_api_key() == false.
/// Test: pure field inspection.
#[test]
fn build_anthropic_without_key_has_no_api_key() {
    let llm = LlmClassifier::build_anthropic("claude-3-5-haiku-latest", None);
    assert!(!llm.has_api_key());
}

/// Why: the Anthropic response shape differs from OpenAI — text lives in
/// `content[].text`, not `choices[].message.content`. A regression here
/// would cause all anthropic-api verdicts to silently return None.
/// What: mock the Anthropic endpoint with a valid Anthropic Messages
/// response body and assert the parsed verdict matches.
/// Test: wiremock server at /v1/messages.
#[tokio::test]
async fn anthropic_response_parsing() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "content": [
            {
                "type": "text",
                "text": "{\"category\":\"bugfix\",\"subcategory\":\"null-check\",\"confidence\":0.92,\"complexity\":2}"
            }
        ]
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let llm = LlmClassifier::build_anthropic(
        "claude-3-5-haiku-latest",
        Some("sk-ant-test".to_string()), // pragma: allowlist secret
    )
    .with_endpoint(format!("{}/v1/messages", server.uri()));

    let r = llm
        .classify("fix: handle null in user endpoint")
        .await
        .expect("verdict");
    assert_eq!(r.category, "bugfix");
    assert_eq!(r.subcategory.as_deref(), Some("null-check"));
    assert!((r.confidence - 0.92).abs() < 1e-6);
    assert_eq!(r.complexity, Some(2));
    assert_eq!(r.method, ClassificationMethod::LlmFallback);
}

/// Why: the Anthropic API requires the `x-api-key` header (not
/// `Authorization: Bearer`). A wrong auth scheme causes HTTP 401.
/// What: mount a mock that matches on the `x-api-key` header and
/// assert the classifier sends the right header.
/// Test: wiremock header matcher.
#[tokio::test]
async fn anthropic_api_request_sets_correct_headers() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "content": [
            {
                "type": "text",
                "text": "{\"category\":\"chore\",\"subcategory\":null,\"confidence\":0.8,\"complexity\":1}"
            }
        ]
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test")) // pragma: allowlist secret
        .and(header("anthropic-version", ANTHROPIC_API_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let llm = LlmClassifier::build_anthropic(
        "claude-3-5-haiku-latest",
        Some("sk-ant-test".to_string()), // pragma: allowlist secret
    )
    .with_endpoint(format!("{}/v1/messages", server.uri()));

    let r = llm
        .classify("chore: bump version")
        .await
        .expect("verdict with correct headers");
    assert_eq!(r.category, "chore");
}

/// Why: `from_llm_config` must resolve the API key from the env var
/// named by `api_key_env` for the `anthropic-api` source, not from
/// a hardcoded var like `ANTHROPIC_API_KEY`.
/// What: set a custom env var, build from a config with that var name,
/// and assert the classifier has a key.
/// Test: env-var manipulation + constructor check.
#[tokio::test]
async fn from_llm_config_anthropic_api_reads_api_key_env() {
    // Use a unique var name to avoid polluting parallel tests.
    let var_name = "TGA_TEST_ANTHROPIC_KEY_9f2e";
    std::env::set_var(var_name, "sk-ant-from-env"); // pragma: allowlist secret

    let cfg = crate::core::config::LlmConfig {
        source: LlmSource::AnthropicApi,
        api_key_env: var_name.to_string(),
        region: None,
        model: Some("claude-3-5-haiku-latest".to_string()),
    };
    let result = LlmClassifier::from_llm_config(&cfg, "claude-3-5-haiku-latest").await;
    std::env::remove_var(var_name);

    let llm = result.expect("should build from env var");
    assert!(llm.has_api_key());
    assert!(llm.use_anthropic_format);
}

/// Why: when the named env var is absent, from_llm_config must return
/// an error naming the variable — no silent no-op allowed.
/// What: call from_llm_config with an env var that does not exist and
/// assert Err mentions the var name.
/// Test: pure error-path check, no network.
#[tokio::test]
async fn from_llm_config_anthropic_api_missing_key_errors() {
    let var_name = "TGA_TEST_ANTHROPIC_MISSING_KEY_7c4b";
    std::env::remove_var(var_name); // ensure absent

    let cfg = crate::core::config::LlmConfig {
        source: LlmSource::AnthropicApi,
        api_key_env: var_name.to_string(),
        region: None,
        model: None,
    };
    let result = LlmClassifier::from_llm_config(&cfg, "gpt-4o-mini").await;
    assert!(result.is_err(), "missing env var must produce Err");
    let err = result.err().expect("just asserted is_err");
    assert!(
        err.contains(var_name),
        "error must name the missing var: {err}"
    );
}

/// Why: when the user specifies `source: anthropic-api` but does not set
/// `model:`, the classifier must default to ANTHROPIC_DEFAULT_MODEL
/// (not the OpenRouter fallback "gpt-4o-mini") so the Anthropic endpoint
/// receives a valid model ID.
/// What: call from_llm_config with `model: None` and `source: anthropic-api`
/// and assert the classifier's model field equals ANTHROPIC_DEFAULT_MODEL.
/// Test: env-var + constructor inspection.
#[tokio::test]
async fn anthropic_default_model_used_when_none_configured() {
    let var_name = "TGA_TEST_ANTHROPIC_DEFAULT_MODEL_3a8d";
    std::env::set_var(var_name, "sk-ant-test-model"); // pragma: allowlist secret

    let cfg = crate::core::config::LlmConfig {
        source: LlmSource::AnthropicApi,
        api_key_env: var_name.to_string(),
        region: None,
        model: None, // user did not set a model
    };
    // The pipeline passes "gpt-4o-mini" as the fallback when model is None.
    let result = LlmClassifier::from_llm_config(&cfg, "gpt-4o-mini").await;
    std::env::remove_var(var_name);

    let llm = result.expect("build from env");
    assert_eq!(
        llm.model, ANTHROPIC_DEFAULT_MODEL,
        "must substitute ANTHROPIC_DEFAULT_MODEL, not gpt-4o-mini"
    );
}

/// Why: presence of a top-level `llm:` section in the config must
/// self-enable the LLM tier without requiring `classification.use_llm: true`.
/// What: build a Config with llm.source = anthropic-api but no
/// classification section; assert that the effective use_llm flag is true.
/// Test: pure logic check on the precedence computation used in build_engine.
#[test]
fn llm_section_presence_self_enables_tier() {
    let cfg = crate::core::config::Config {
        llm: Some(crate::core::config::LlmConfig {
            source: LlmSource::AnthropicApi,
            api_key_env: "ANTHROPIC_ANALYTICS_API_KEY".to_string(), // pragma: allowlist secret
            region: None,
            model: Some("claude-3-5-haiku-latest".to_string()),
        }),
        classification: None,
        ..crate::core::config::Config::default()
    };
    // Replicate the logic from build_engine to verify it evaluates to true.
    let use_llm = cfg.llm.is_some()
        || cfg
            .classification
            .as_ref()
            .map(|c| c.use_llm)
            .unwrap_or(false);
    assert!(
        use_llm,
        "llm: section presence must self-enable the LLM tier"
    );
}

#[tokio::test]
async fn classify_returns_none_without_api_key() {
    let llm = LlmClassifier::new("gpt-4o-mini", None);
    assert!(llm.classify("feat: anything").await.is_none());
}

/// Regression for the hive-review finding in PR #(issue 99): the raw
/// `LlmClassifier` always sets `ticket_id: None`. The engine wrapper
/// (`Engine::llm_classify_only`) backfills it via regex extraction.
/// This test pins the raw classifier's contract so any future change
/// that starts surfacing `ticket_id` from the LLM verdict prompts the
/// engine wrapper to be revisited (so we don't double-backfill).
#[tokio::test]
async fn classify_does_not_set_ticket_id() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "choices": [{
            "message": {
                "content": "{\"category\": \"bugfix\", \
                             \"subcategory\": null, \
                             \"confidence\": 0.8}"
            }
        }]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let llm = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let r = llm
        .classify("fix: handle null in PROJ-1234 endpoint")
        .await
        .expect("LLM verdict");
    assert_eq!(r.ticket_id, None);
}

/// Why: the LLM tier is the only producer of complexity scores; if the
/// `complexity` key stops deserializing, all scoring silently breaks.
/// What: a JSON verdict with `"complexity": 3` must deserialize to
/// `Some(3)`, and a verdict omitting the key must default to `None`.
/// Test: `serde_json::from_str` two payloads and assert the field.
#[test]
fn llm_verdict_deserializes_complexity() {
    let with: LlmVerdict =
        serde_json::from_str(r#"{"category":"feature","confidence":0.9,"complexity":3}"#)
            .expect("deserialize verdict with complexity");
    assert_eq!(with.complexity, Some(3));

    let without: LlmVerdict = serde_json::from_str(r#"{"category":"feature","confidence":0.9}"#)
        .expect("deserialize verdict without complexity");
    assert_eq!(without.complexity, None);
}

/// Why: the model only emits complexity if the prompt asks for it; this
/// pins the prompt so a future edit can't drop the request silently.
/// What: asserts the system prompt mentions the `complexity` key.
/// Test: substring check on `SYSTEM_PROMPT`.
#[test]
fn system_prompt_requests_complexity() {
    assert!(
        SYSTEM_PROMPT.contains("complexity"),
        "system prompt must instruct the model to return a complexity score"
    );
}

#[tokio::test]
async fn classify_dispatches_to_endpoint_when_keyed() {
    // Regression: issue #99 — when the pipeline asks the LLM tier
    // directly, an HTTP call must happen even for messages a regex
    // tier would have caught. This test verifies the raw classifier
    // hits its configured endpoint and returns the LLM verdict.
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "choices": [{
            "message": {
                "content": "{\"category\": \"feature\", \
                             \"subcategory\": \"new-auth\", \
                             \"confidence\": 0.91}"
            }
        }]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let llm = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let r = llm.classify("chore: bump deps").await.expect("LLM verdict");
    assert_eq!(r.category, "feature");
    assert_eq!(r.subcategory.as_deref(), Some("new-auth"));
    assert!((r.confidence - 0.91).abs() < 1e-6);
    assert_eq!(r.method, ClassificationMethod::LlmFallback);
}

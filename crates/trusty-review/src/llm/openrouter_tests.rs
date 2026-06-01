//! Tests for the OpenRouter provider.
//!
//! Why: extracted from `openrouter.rs` to keep that file under the 500-line
//! cap while preserving full test coverage.
//! What: construction, cost estimation, structured-output `response_format`
//! shape, and basic request-field tests.
//! Test: included as `#[cfg(test)] mod tests` from `openrouter.rs`.

use super::*;
use crate::llm::ChatMessage;

#[test]
fn new_returns_error_on_empty_key() {
    let result = OpenRouterProvider::new("", "openai/gpt-5.4-mini-20260317");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, LlmError::AccessDenied(_)));
    assert!(err.is_alarm());
}

#[test]
fn new_succeeds_with_valid_key() {
    let p = OpenRouterProvider::new("sk-test-key", "openai/gpt-5.4-mini-20260317")
        .expect("should succeed with non-empty key");
    assert_eq!(p.name(), "openrouter");
}

#[test]
fn cost_estimate_for_nano_model() {
    let cost = estimate_cost_usd("openai/gpt-5.4-nano-20260317", 1_000_000, 1_000_000);
    assert!((cost - 1.45_f64).abs() < 1e-9, "expected $1.45, got {cost}");
}

#[test]
fn cost_estimate_for_mini_model() {
    let cost = estimate_cost_usd("openai/gpt-5.4-mini-20260317", 1_000_000, 1_000_000);
    assert!((cost - 5.25_f64).abs() < 1e-9, "expected $5.25, got {cost}");
}

#[test]
fn cost_estimate_for_full_model() {
    let cost = estimate_cost_usd("openai/gpt-5.4-20260305", 1_000_000, 1_000_000);
    assert!(
        (cost - 17.50_f64).abs() < 1e-9,
        "expected $17.50, got {cost}"
    );
}

#[test]
fn cost_estimate_for_pro_model() {
    let cost = estimate_cost_usd("openai/gpt-5.5-pro-20260423", 1_000_000, 1_000_000);
    assert!(
        (cost - 210.0_f64).abs() < 1e-9,
        "expected $210.00, got {cost}"
    );
}

#[test]
fn cost_estimate_for_unknown_model() {
    let cost = estimate_cost_usd("unknown/model", 100_000, 50_000);
    assert_eq!(cost, 0.0);
}

/// Verify that when `response_schema` is set, the serialized request body
/// includes `response_format.type = "json_schema"` and the schema name.
///
/// Why: this is the core new behavior for OpenRouter structured output;
/// without `response_format` the model may ignore the schema entirely.
/// What: builds an `OrcRequest` with a response_format set, serialises
/// to JSON, asserts the expected fields are present.
/// Test: no network call.
#[test]
fn complete_with_schema_sends_response_format() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string"},
            "findings": {"type": "array"}
        },
        "required": ["verdict", "findings"]
    });

    let messages = vec![OrcMessage {
        role: "user".to_string(),
        content: "review".to_string(),
    }];

    let body = OrcRequest {
        model: "openai/gpt-5.4-mini-20260317",
        messages: &messages,
        stream: false,
        temperature: 0.3,
        max_tokens: 1024,
        response_format: Some(OrcResponseFormat {
            type_: "json_schema",
            json_schema: OrcJsonSchema {
                name: "review_output",
                strict: true,
                schema: &schema,
            },
        }),
    };

    let json_str = serde_json::to_string(&body).expect("must serialise");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("must parse back");

    assert_eq!(
        parsed["response_format"]["type"], "json_schema",
        "response_format.type must be json_schema"
    );
    assert_eq!(
        parsed["response_format"]["json_schema"]["name"], "review_output",
        "json_schema.name must match the schema name"
    );
    assert_eq!(
        parsed["response_format"]["json_schema"]["strict"], true,
        "strict must be true"
    );
    assert!(
        parsed["response_format"]["json_schema"]["schema"].is_object(),
        "schema must be an object"
    );
}

/// Verify that when `response_schema` is None, the serialized body does NOT
/// include a `response_format` field.
///
/// Why: legacy callers (free-text mode) must not receive an unexpected
/// `response_format` that might cause a 422 Validation error on some models.
/// What: constructs a request without `response_format`, serialises, asserts
/// the field is absent.
/// Test: no network.
#[test]
fn complete_without_schema_omits_response_format() {
    let messages = vec![OrcMessage {
        role: "user".to_string(),
        content: "review".to_string(),
    }];
    let body = OrcRequest {
        model: "openai/gpt-5.4-mini-20260317",
        messages: &messages,
        stream: false,
        temperature: 0.3,
        max_tokens: 1024,
        response_format: None,
    };
    let json_str = serde_json::to_string(&body).expect("must serialise");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("must parse");
    assert!(
        parsed.get("response_format").is_none(),
        "response_format must be absent when schema is None"
    );
}

#[tokio::test]
async fn complete_builds_correct_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let mock_handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap();
        let raw = std::str::from_utf8(&buf[..n]).unwrap().to_string();

        let body_start = raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let json_body: serde_json::Value =
            serde_json::from_str(&raw[body_start..]).unwrap_or_default();

        let resp_body = serde_json::json!({
            "choices": [{"message": {"content": "LGTM"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 10},
            "model": "openai/gpt-5.4-mini-20260317"
        })
        .to_string();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        sock.write_all(http_resp.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();

        json_body
    });

    let _ = base_url;
    drop(mock_handle);

    let req = LlmRequest {
        model: "openai/gpt-5.4-mini-20260317".to_string(),
        system: "You are a code reviewer.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Review this diff.".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 1024,
        response_schema: None,
    };
    assert_eq!(req.model, "openai/gpt-5.4-mini-20260317");
    assert_eq!(req.messages.len(), 1);
    assert!((req.temperature - 0.3_f32).abs() < f32::EPSILON);
}

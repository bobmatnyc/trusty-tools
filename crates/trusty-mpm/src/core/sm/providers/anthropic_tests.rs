//! Tests for the SM Anthropic native provider.
//!
//! Why: extracted from `anthropic.rs` to keep that file under the 500-SLOC cap
//! while exercising the full `complete` path against a local mock (no real
//! `api.anthropic.com` calls).
//! What: construction validation, native-body shaping, a `complete` round-trip
//! against a one-shot `TcpListener` mock returning a native `/v1/messages`
//! response, and an error-mapping check for a 400 response.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `anthropic.rs`.

use super::*;
use crate::core::sm::providers::ChatMessage;
use crate::core::sm::providers::test_support::read_full_request;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Spawn a one-shot HTTP mock returning `status_line`/`body`; yields the base
/// `http://127.0.0.1:PORT` URL.
///
/// Why: drive `complete` without touching the real Anthropic API.
/// What: binds an ephemeral port, accepts one connection, drains the full
/// request via [`read_full_request`] (so the reply never races ahead of the
/// client's `write`), replies, and shuts down.
/// Test: used by the `complete_*` tests.
async fn spawn_mock(status_line: &'static str, body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        read_full_request(&mut sock).await;
        let resp = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    format!("http://{addr}")
}

#[test]
fn new_rejects_empty_key() {
    let result = AnthropicProvider::with_base_url("", "claude-sonnet-4-6", "http://unused");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), SmLlmError::AccessDenied(_)));
}

#[test]
fn provider_name_is_anthropic() {
    let p =
        AnthropicProvider::with_base_url("sk-ant", "claude-haiku", "http://unused").expect("ok");
    assert_eq!(p.name(), "anthropic");
}

/// Why: Anthropic requires `system` at the TOP LEVEL, not as a message — a
/// mis-shaped body silently drops the system prompt.
/// What: builds the body from a request with a system prompt and asserts the
/// top-level `system` field plus the user message shape.
/// Test: this is the test.
#[test]
fn build_body_places_system_top_level() {
    let req = LlmRequest {
        model: "claude-sonnet-4-6".to_string(),
        system: "You are the SM.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "go".to_string(),
        }],
        temperature: 0.3,
        max_tokens: 512,
    };
    let body = super::build_body(&req);
    assert_eq!(body["system"], "You are the SM.");
    assert_eq!(body["model"], "claude-sonnet-4-6");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "go");
    // The system prompt must NOT also appear as a message.
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}

/// Why: prove `complete` drives the native path — building the body, parsing a
/// real `content[]` response, and reading `usage`.
/// What: points the provider at a mock returning a native response and asserts
/// the concatenated text + token counts + cost.
/// Test: this is the test (no real network).
#[tokio::test]
async fn complete_roundtrips_against_mock() {
    let body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "stop_reason": "end_turn",
        "content": [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ],
        "usage": {"input_tokens": 2000, "output_tokens": 100}
    })
    .to_string();
    let base = spawn_mock("HTTP/1.1 200 OK", body).await;

    let provider =
        AnthropicProvider::with_base_url("sk-ant", "claude-sonnet-4-6", base).expect("provider");

    let resp = provider
        .complete(LlmRequest {
            model: "claude-sonnet-4-6".to_string(),
            system: "sys".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 256,
        })
        .await
        .expect("complete ok");

    assert_eq!(resp.text, "first\nsecond");
    assert_eq!(resp.input_tokens, 2000);
    assert_eq!(resp.output_tokens, 100);
    // 2000/1e6*3.00 + 100/1e6*15.00 = 0.006 + 0.0015 = 0.0075
    assert!(
        (resp.cost_usd - 0.0075_f64).abs() < 1e-9,
        "expected $0.0075, got {}",
        resp.cost_usd
    );
}

/// Why: a 400 from Anthropic is deterministic (bad request) and must map to the
/// non-retryable `Validation` alarm.
/// What: mock returns HTTP 400; asserts the mapped error is an alarm, not
/// retryable.
/// Test: this is the test.
#[tokio::test]
async fn complete_maps_http_errors() {
    let base = spawn_mock("HTTP/1.1 400 Bad Request", "bad model".to_string()).await;
    let provider =
        AnthropicProvider::with_base_url("sk-ant", "claude-haiku", base).expect("provider");

    let err = provider
        .complete(LlmRequest {
            model: "claude-haiku".to_string(),
            system: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "x".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 64,
        })
        .await
        .expect_err("400 should error");

    assert!(matches!(err, SmLlmError::Validation(_)));
    assert!(err.is_alarm());
    assert!(!err.is_retryable());
}

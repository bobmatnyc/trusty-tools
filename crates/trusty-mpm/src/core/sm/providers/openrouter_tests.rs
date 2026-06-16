//! Tests for the SM OpenRouter provider.
//!
//! Why: extracted from `openrouter.rs` to keep that file under the 500-SLOC
//! cap while exercising the full `complete` path against a local mock server
//! (no real OpenRouter calls).
//! What: construction validation, cost estimation, and a round-trip of
//! `complete` against a one-shot `TcpListener` mock that returns a canned
//! chat-completions body, plus an error-mapping check for a 429 response.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `openrouter.rs`.

use super::*;
use crate::core::sm::providers::ChatMessage;
use crate::core::sm::providers::test_support::read_full_request;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Spawn a one-shot HTTP mock that replies with `status`/`body` and returns the
/// bound `http://127.0.0.1:PORT` base URL.
///
/// Why: the providers must be tested without touching the real OpenRouter API;
/// a raw `TcpListener` (matching the trusty-review test pattern) is the
/// lightest dependency-free mock.
/// What: binds an ephemeral port, accepts ONE connection, drains the FULL
/// request via [`read_full_request`] (headers + body) so the reply never races
/// ahead of the client's `write`, writes a fixed HTTP response, and shuts down.
/// Test: used by `complete_roundtrips_against_mock` / `complete_maps_http_errors`.
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
    let result = OpenRouterProvider::new("", "anthropic/claude-sonnet-4-6");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, SmLlmError::AccessDenied(_)));
    assert!(err.is_alarm());
}

#[test]
fn new_succeeds_with_valid_key() {
    let p = OpenRouterProvider::new("sk-test", "anthropic/claude-haiku").expect("ok");
    assert_eq!(p.name(), "openrouter");
}

/// Why: prove `complete` actually drives the HTTP path — building the request,
/// parsing a real chat-completions response, and computing usage/cost — not
/// just constructing the type.
/// What: points the provider at a local mock returning a canned body and
/// asserts the parsed text + token counts + cost.
/// Test: this is the test (no real network).
#[tokio::test]
async fn complete_roundtrips_against_mock() {
    let body = serde_json::json!({
        "choices": [{"message": {"content": "delegated to engineer session"}}],
        "usage": {"prompt_tokens": 1000, "completion_tokens": 500},
        "model": "anthropic/claude-sonnet-4-6"
    })
    .to_string();
    let base = spawn_mock("HTTP/1.1 200 OK", body).await;

    let provider =
        OpenRouterProvider::with_base_url("sk-test", "anthropic/claude-sonnet-4-6", base)
            .expect("provider");

    let resp = provider
        .complete(LlmRequest {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            system: "You are the SM.".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "plan this goal".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 1024,
        })
        .await
        .expect("complete ok");

    assert_eq!(resp.text, "delegated to engineer session");
    assert_eq!(resp.input_tokens, 1000);
    assert_eq!(resp.output_tokens, 500);
    // 1000/1e6*3.00 + 500/1e6*15.00 = 0.003 + 0.0075 = 0.0105
    assert!(
        (resp.cost_usd - 0.0105_f64).abs() < 1e-9,
        "expected $0.0105, got {}",
        resp.cost_usd
    );
}

/// Why: a 429 from OpenRouter must map to the retryable `RateLimited` variant
/// so the fallback chain (§5.3) can advance.
/// What: points the provider at a mock returning HTTP 429 and asserts the
/// mapped error classifies as retryable, not an alarm.
/// Test: this is the test.
#[tokio::test]
async fn complete_maps_http_errors() {
    let base = spawn_mock("HTTP/1.1 429 Too Many Requests", "rate limited".to_string()).await;
    let provider = OpenRouterProvider::with_base_url("sk-test", "anthropic/claude-haiku", base)
        .expect("provider");

    let err = provider
        .complete(LlmRequest {
            model: "anthropic/claude-haiku".to_string(),
            system: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "summarize".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 256,
        })
        .await
        .expect_err("429 should error");

    assert!(matches!(err, SmLlmError::RateLimited));
    assert!(err.is_retryable());
    assert!(!err.is_alarm());
}

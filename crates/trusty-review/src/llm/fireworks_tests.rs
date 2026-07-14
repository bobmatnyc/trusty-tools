//! Tests for the Fireworks provider.
//!
//! Why: kept out of `fireworks.rs` to respect the 500-line file cap while
//! preserving full coverage.
//! What: construction, cost estimation, wire-format shape (including the
//! no-`strict` invariant that distinguishes Fireworks from OpenRouter), a
//! real mock-server round-trip via the injectable base URL, and an
//! `#[ignore]`d live smoke test proving structured output + enum enforcement
//! against the real API.
//! Test: included as `#[cfg(test)] mod tests` from `fireworks.rs`.

use super::*;
use crate::llm::ChatMessage;

#[test]
fn new_returns_error_on_empty_key() {
    let result = FireworksProvider::new("", "accounts/fireworks/models/llama-v3p1-8b-instruct");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, LlmError::AccessDenied(_)));
    assert!(err.is_alarm());
}

#[test]
fn new_succeeds_with_valid_key_and_name_is_fireworks() {
    let p = FireworksProvider::new(
        "fw-test-key", // pragma: allowlist secret
        "accounts/fireworks/models/llama-v3p1-8b-instruct",
    )
    .expect("should succeed with non-empty key");
    assert_eq!(p.name(), "fireworks");
}

// ── Cost estimation ─────────────────────────────────────────────────────────

#[test]
fn cost_estimate_kimi() {
    let cost = estimate_cost_usd("accounts/fireworks/models/kimi-k2p5", 1_000_000, 1_000_000);
    assert!((cost - 3.10_f64).abs() < 1e-9, "expected $3.10, got {cost}");
}

#[test]
fn cost_estimate_llama_70b() {
    let cost = estimate_cost_usd(
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        1_000_000,
        1_000_000,
    );
    assert!((cost - 1.80_f64).abs() < 1e-9, "expected $1.80, got {cost}");
}

#[test]
fn cost_estimate_llama_8b_cheapest() {
    let cost = estimate_cost_usd(
        "accounts/fireworks/models/llama-v3p1-8b-instruct",
        1_000_000,
        1_000_000,
    );
    assert!((cost - 0.40_f64).abs() < 1e-9, "expected $0.40, got {cost}");
}

#[test]
fn cost_estimate_deepseek_r1_before_v3() {
    // Family order: "deepseek-r1" must match before size-tier fallbacks.
    let cost = estimate_cost_usd(
        "accounts/fireworks/models/deepseek-r1",
        1_000_000,
        1_000_000,
    );
    assert!(
        (cost - 11.00_f64).abs() < 1e-9,
        "expected $11.00, got {cost}"
    );
}

#[test]
fn cost_estimate_unknown_model_is_zero() {
    let cost = estimate_cost_usd(
        "accounts/fireworks/models/some-future-model",
        100_000,
        50_000,
    );
    assert_eq!(cost, 0.0);
}

// ── Wire-format shape ───────────────────────────────────────────────────────

/// Verify that when `response_schema` is set, the serialized request body
/// includes `response_format.type = "json_schema"` and the schema name, and
/// does NOT include a `strict` field.
///
/// Why: Fireworks documents no `strict` field (constrained generation enforces
/// the schema inherently); sending undocumented fields risks a validation
/// rejection.  This is the deliberate divergence from the OpenRouter wire
/// struct, which sends `strict: true`.
/// What: builds a `FwRequest` with a response_format set, serialises to JSON,
/// asserts the expected fields are present and `strict` is absent.
/// Test: no network call.
#[test]
fn complete_with_schema_sends_response_format_without_strict() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "judgment": {"type": "string", "enum": ["CONFIRMED", "REFUTED"]}
        },
        "required": ["judgment"],
        "additionalProperties": false
    });

    let messages = vec![FwMessage {
        role: "user".to_string(),
        content: "verify".to_string(),
    }];

    let body = FwRequest {
        model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        messages: &messages,
        stream: false,
        temperature: 0.0,
        max_tokens: 256,
        response_format: Some(FwResponseFormat {
            type_: "json_schema",
            json_schema: FwJsonSchema {
                name: "verification_judgment",
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
        parsed["response_format"]["json_schema"]["name"], "verification_judgment",
        "json_schema.name must match the schema name"
    );
    assert!(
        parsed["response_format"]["json_schema"]["schema"].is_object(),
        "schema must be an object"
    );
    assert!(
        parsed["response_format"]["json_schema"]
            .get("strict")
            .is_none(),
        "Fireworks wire format must NOT contain a strict field"
    );
}

/// Verify that when `response_schema` is None, the serialized body does NOT
/// include a `response_format` field.
#[test]
fn complete_without_schema_omits_response_format() {
    let messages = vec![FwMessage {
        role: "user".to_string(),
        content: "review".to_string(),
    }];
    let body = FwRequest {
        model: "accounts/fireworks/models/llama-v3p1-8b-instruct",
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

// ── Mock-server round-trip ──────────────────────────────────────────────────

/// Full request/response round-trip against a local mock HTTP server.
///
/// Why: unlike the OpenRouter provider (hardcoded URL, so its "mock server"
/// test never actually connects), the injectable base URL lets this test
/// exercise the REAL request path: URL joining, bearer auth, JSON body,
/// response parsing, usage extraction, and finish_reason normalisation.
/// What: binds a TcpListener, points the provider at it, sends a structured
/// request, and asserts on both the captured request (path, auth header, bare
/// model id, json_schema response_format without strict) and the parsed
/// `LlmResponse`.
/// Test: this test itself; no external network.
#[tokio::test]
async fn complete_round_trips_through_mock_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let mock_handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // Read until the full body has arrived (headers + Content-Length).
        let mut raw = Vec::new();
        let mut buf = vec![0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(header_end) = text.find("\r\n\r\n") {
                let content_length = text.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                });
                let body_len = raw.len() - (header_end + 4);
                if content_length.is_none_or(|cl| body_len >= cl) {
                    break;
                }
            }
        }
        let raw = String::from_utf8(raw).unwrap();

        let resp_body = serde_json::json!({
            "choices": [{
                "message": {"content": "{\"judgment\":\"CONFIRMED\",\"reason\":\"repro\"}"},
                "finish_reason": "STOP"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 10},
            "model": "accounts/fireworks/models/llama-v3p1-70b-instruct"
        })
        .to_string();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        sock.write_all(http_resp.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();

        raw
    });

    let provider = FireworksProvider::with_base_url(
        "fw-mock-key", // pragma: allowlist secret
        "accounts/fireworks/models/llama-v3p1-70b-instruct",
        &base_url,
    )
    .expect("provider construction");

    let req = LlmRequest {
        model: "accounts/fireworks/models/llama-v3p1-70b-instruct".to_string(),
        system: "You are a verifier.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Verify this finding.".to_string(),
        }],
        temperature: 0.0,
        max_tokens: 256,
        response_schema: Some(crate::llm::ResponseSchema {
            name: "verification_judgment".to_string(),
            schema: serde_json::json!({"type": "object", "properties": {"judgment": {"type": "string"}}}),
        }),
    };

    let resp = provider.complete(req).await.expect("mock round-trip");
    let raw_request = mock_handle.await.expect("mock server task");

    // ── Request assertions ──────────────────────────────────────────────
    let first_line = raw_request.lines().next().unwrap_or_default();
    assert!(
        first_line.starts_with("POST /chat/completions"),
        "must POST to /chat/completions, got: {first_line}"
    );
    assert!(
        raw_request.contains("authorization: Bearer fw-mock-key")
            || raw_request.contains("Authorization: Bearer fw-mock-key"),
        "must send bearer auth"
    );
    let body_start = raw_request.find("\r\n\r\n").map(|i| i + 4).unwrap();
    let sent: serde_json::Value = serde_json::from_str(&raw_request[body_start..]).unwrap();
    assert_eq!(
        sent["model"], "accounts/fireworks/models/llama-v3p1-70b-instruct",
        "wire model must be the bare provider-native id"
    );
    assert_eq!(sent["stream"], false);
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["response_format"]["type"], "json_schema");
    assert!(
        sent["response_format"]["json_schema"]
            .get("strict")
            .is_none(),
        "no strict field on the wire"
    );

    // ── Response assertions ─────────────────────────────────────────────
    assert_eq!(
        resp.text,
        "{\"judgment\":\"CONFIRMED\",\"reason\":\"repro\"}"
    );
    assert_eq!(resp.input_tokens, 100);
    assert_eq!(resp.output_tokens, 10);
    assert_eq!(
        resp.finish_reason.as_deref(),
        Some("stop"),
        "finish_reason must be normalised to lowercase"
    );
    assert_eq!(
        resp.model,
        "accounts/fireworks/models/llama-v3p1-70b-instruct"
    );
}

// ── Live smoke test ─────────────────────────────────────────────────────────

/// Live structured-output smoke test against the real Fireworks API.
///
/// Why: proves the two things the docs leave open — that the real endpoint
/// accepts our exact wire format, and that `enum` values inside a
/// `json_schema` response_format are actually ENFORCED by constrained
/// generation (the verify pipeline's `judgment` field depends on this).
/// What: sends the real `verify_response_schema()` (judgment enum + reason)
/// with a trivial verification prompt and asserts the response is clean JSON
/// whose `judgment` is one of the two legal values.
/// Test: `FIREWORKS_API_KEY=... cargo test -p trusty-review -- --include-ignored live_fireworks`.
#[tokio::test]
#[ignore = "live network test; requires FIREWORKS_API_KEY"]
async fn live_fireworks_structured_output_enforces_enum() {
    let Ok(key) = std::env::var("FIREWORKS_API_KEY") else {
        eprintln!("skipping: FIREWORKS_API_KEY not set");
        return;
    };

    // NOTE: Fireworks' serverless catalog rotates; llama-v3p1-* was retired
    // (NOT_FOUND as of July 2026). kimi-k2p6 is the family featured in the
    // structured-output docs. If this 404s, list current models with:
    // curl -s https://api.fireworks.ai/inference/v1/models -H "Authorization: Bearer $FIREWORKS_API_KEY"
    let model = "accounts/fireworks/models/kimi-k2p6";
    let provider = FireworksProvider::new(key, model).expect("provider");

    let schema = crate::pipeline::verify_prompt::verify_response_schema();
    let req = LlmRequest {
        model: model.to_string(),
        system: "You are a code-review finding verifier. Respond with a JSON object \
                 containing your judgment."
            .to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Finding: `let x = items[0];` panics on an empty vector, and the \
                      call site never checks emptiness. Verify this finding and answer \
                      in JSON."
                .to_string(),
        }],
        temperature: 0.0,
        // Reasoning models (kimi-k2p6) spend a reasoning budget BEFORE the
        // constrained JSON lands in `content` (reasoning goes to the separate
        // `reasoning_content` field, which we ignore).  256 tokens truncates
        // mid-reasoning and yields no JSON; give it real headroom.
        max_tokens: 2048,
        response_schema: Some(schema),
    };

    let resp = provider.complete(req).await.expect("live fireworks call");
    eprintln!("live fireworks structured response: {}", resp.text);

    // The text must be a clean JSON object (no fences) with an enforced enum.
    let parsed: serde_json::Value =
        serde_json::from_str(resp.text.trim()).expect("response must be clean JSON");
    let judgment = parsed["judgment"]
        .as_str()
        .expect("judgment must be a string");
    assert!(
        judgment == "CONFIRMED" || judgment == "REFUTED",
        "judgment must be one of the enum values, got {judgment:?}"
    );
    assert!(resp.input_tokens > 0, "usage must be populated");
}

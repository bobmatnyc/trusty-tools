//! Black-box e2e: prove trusty-code's OpenRouter/Fireworks/Together inference
//! flows through the shared `trusty_common::inference` adapter (#2406, #2494,
//! epic #2400).
//!
//! Why: #2406's core claim is that tcode's OpenAI-compatible transport is now
//! the shared adapter, and that Fireworks is reachable; #2494 extends the same
//! claim to Together. tcode's standing directive requires that claim be proven
//! end-to-end, black-box, offline, and deterministic — not merely via internal
//! code paths. This drives tcode's real production transport
//! (`OpenAiCompatClient`, the same type `main.rs` and `task::mock_llm`
//! construct) over a REAL loopback socket served by the shared
//! `test_support::MockInferenceServer`, then asserts on the exact wire request
//! the shared adapter emitted. That request shape (endpoint path, `Bearer` auth,
//! provider-specific `usage` directive, and the Fireworks/Together
//! prefix-stripped model) is producible ONLY by the shared adapter — so a green
//! run is direct evidence the migration is wired correctly.
//! What: three cases — OpenRouter (asserts the shared adapter injected
//! `usage:{include:true}` and sent the slug verbatim), Fireworks (asserts NO
//! usage directive and the `fireworks/` routing prefix stripped to the
//! provider-native model id), and Together (asserts NO usage directive, `Bearer`
//! auth, and the `together/` routing prefix stripped to the bare Together
//! catalog slug). Credentials come from an injected `MemoryKeyStore` and base
//! URLs from `OpenAiCompatClient::with_config`, so the test mutates no process
//! env and is fully parallel-safe.
//! Test: this file (`cargo test -p trusty-code --test inference_shared_adapter_e2e`).

use serde_json::json;

use trusty_code::llm::{ChatMessage, ChatRequest, OpenAiCompatClient};
use trusty_common::inference::credentials::{KeyStore, MemoryKeyStore};
use trusty_common::inference::test_support::MockInferenceServer;

/// A canned OpenAI-compatible chat/completions response the mock serves.
fn canned_response() -> serde_json::Value {
    json!({
        "id": "gen-mock",
        "model": "served/model",
        "choices": [{
            "message": {"role": "assistant", "content": "pong-mock", "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    })
}

/// Build a minimal single-turn tcode request for `model` with no usage directive
/// set by the caller (so any directive on the wire came from the shared adapter).
fn request_for(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::system("You are a concise assistant."),
            ChatMessage::user("Reply with exactly the word: pong"),
        ],
        temperature: Some(0.0),
        max_tokens: Some(16),
        tools: None,
        tool_choice: None,
        usage: None,
    }
}

/// An OpenRouter-routed request flows through the shared adapter, which injects
/// the OpenRouter-only `usage:{include:true}` directive and sends the slug
/// verbatim.
///
/// Why: proves the OpenRouter path is the shared adapter (the usage-directive
/// injection is shared-adapter behaviour driven by the capability registry, not
/// anything tcode does) and that behaviour is preserved unchanged.
/// What: point the OpenRouter base URL at the mock, chat, then assert the parsed
/// response AND the captured wire request (path, auth, model, usage directive).
/// Test: this test.
#[tokio::test]
async fn openrouter_routes_through_shared_adapter_and_injects_usage() {
    let server = MockInferenceServer::spawn(200, canned_response())
        .await
        .expect("spawn mock");

    let store = MemoryKeyStore::new();
    store.set("openrouter", "sk-or-mock").expect("seed key"); // pragma: allowlist secret

    // OpenRouter → mock; Fireworks and Together base URLs are unused here.
    let client = OpenAiCompatClient::with_config(
        Box::new(store),
        server.url().to_string(),
        "http://127.0.0.1:1/unused".to_string(),
        "http://127.0.0.1:1/unused".to_string(),
    );

    let resp = client
        .chat(&request_for("openai/gpt-4o-mini"))
        .await
        .expect("chat through shared adapter");

    // Response flowed back through the tcode conversion.
    assert_eq!(resp.first_text().as_deref(), Some("pong-mock"));
    assert_eq!(resp.token_usage().prompt_tokens, 5);

    // The shared adapter's outbound wire request.
    let captured = server.last_request().expect("one request captured");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/chat/completions");
    assert!(
        captured
            .header("authorization")
            .is_some_and(|v| v.starts_with("Bearer ")),
        "shared adapter must send a Bearer auth header"
    );
    let body = captured.body.expect("json body");
    assert_eq!(body["model"], "openai/gpt-4o-mini", "slug sent verbatim");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(
        body["usage"],
        json!({"include": true}),
        "shared OpenRouter adapter must inject the detailed-usage directive"
    );
}

/// A Fireworks-routed request flows through the shared adapter, which strips the
/// `fireworks/` routing prefix to the provider-native model id and sends NO
/// usage directive (Fireworks does not support it).
///
/// Why: this is the concrete payoff of #2406 — Fireworks is reachable, routed
/// through the shared adapter, with the correct provider-specific wire shape.
/// What: point the Fireworks base URL at the mock, chat with a `fireworks/`
/// slug, then assert the captured request carries the stripped model and no
/// usage directive.
/// Test: this test.
#[tokio::test]
async fn fireworks_routes_through_shared_adapter_strips_prefix_and_omits_usage() {
    let server = MockInferenceServer::spawn(200, canned_response())
        .await
        .expect("spawn mock");

    let store = MemoryKeyStore::new();
    store.set("fireworks", "fw-mock").expect("seed key"); // pragma: allowlist secret

    // Fireworks → mock; OpenRouter and Together base URLs are unused here.
    let client = OpenAiCompatClient::with_config(
        Box::new(store),
        "http://127.0.0.1:1/unused".to_string(),
        server.url().to_string(),
        "http://127.0.0.1:1/unused".to_string(),
    );

    let resp = client
        .chat(&request_for(
            "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct",
        ))
        .await
        .expect("chat through shared fireworks adapter");

    assert_eq!(resp.first_text().as_deref(), Some("pong-mock"));

    let captured = server.last_request().expect("one request captured");
    assert_eq!(captured.path, "/chat/completions");
    let body = captured.body.expect("json body");
    assert_eq!(
        body["model"], "accounts/fireworks/models/llama-v3p1-8b-instruct",
        "the fireworks/ routing prefix must be stripped to the provider-native id"
    );
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "Fireworks must NOT receive the OpenRouter-only usage directive"
    );
}

/// A Together-routed request flows through the shared adapter, which strips the
/// `together/` routing prefix to the bare Together catalog slug and sends NO
/// usage directive (Together does not support the OpenRouter directive).
///
/// Why: this is the concrete payoff of #2494 — Together is reachable, routed
/// through the shared adapter, with the correct provider-specific wire shape:
/// `Bearer` auth, the prefix-stripped model id, and no usage directive.
/// What: point the Together base URL at the mock, chat with a `together/` slug,
/// then assert the parsed response AND the captured wire request (path, auth,
/// stripped model, absent usage directive).
/// Test: this test.
#[tokio::test]
async fn together_routes_through_shared_adapter_strips_prefix_and_omits_usage() {
    let server = MockInferenceServer::spawn(200, canned_response())
        .await
        .expect("spawn mock");

    let store = MemoryKeyStore::new();
    store.set("together", "tgp_v1_mock").expect("seed key"); // pragma: allowlist secret

    // Together → mock; OpenRouter and Fireworks base URLs are unused here.
    let client = OpenAiCompatClient::with_config(
        Box::new(store),
        "http://127.0.0.1:1/unused".to_string(),
        "http://127.0.0.1:1/unused".to_string(),
        server.url().to_string(),
    );

    let resp = client
        .chat(&request_for(
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo",
        ))
        .await
        .expect("chat through shared together adapter");

    // Response flowed back through the tcode conversion.
    assert_eq!(resp.first_text().as_deref(), Some("pong-mock"));
    assert_eq!(resp.token_usage().prompt_tokens, 5);

    let captured = server.last_request().expect("one request captured");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/chat/completions");
    assert!(
        captured
            .header("authorization")
            .is_some_and(|v| v.starts_with("Bearer ")),
        "shared adapter must send a Bearer auth header"
    );
    let body = captured.body.expect("json body");
    assert_eq!(
        body["model"], "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        "the together/ routing prefix must be stripped to the bare Together slug"
    );
    assert!(
        body.get("usage").is_none() || body["usage"].is_null(),
        "Together must NOT receive the OpenRouter-only usage directive"
    );
}
